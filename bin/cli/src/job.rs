use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Duration;

use bosun_types::api::{Execution, Job, LogSearch};
use bosun_types::command::{Command as Operation, CommandRequest, Target};
use bosun_types::event::RunEvent;
use bosun_types::id::JobId;
use bosun_types::status::{ExecutionStatus, JobStatus};
use clap::Subcommand;
use serde::de::DeserializeOwned;
use tokio::time::sleep;

use crate::client::{Client, Failure, patiently};
use crate::output::{self, name};
use crate::{Context, follow};

const POLL: Duration = Duration::from_secs(1);

#[derive(Subcommand)]
pub(crate) enum Command {
    #[command(about = "Print the log of a job's steps, each line with its time")]
    Logs {
        #[arg(help = "The id of the job")]
        job: String,
        #[arg(
            long,
            value_name = "N",
            help = "Only the step with this number, counted from 1"
        )]
        step: Option<usize>,
        #[arg(
            long,
            help = "Wait for the job, and print its lines as they arrive until it ends"
        )]
        follow: bool,
        #[arg(
            long,
            value_name = "TEXT",
            conflicts_with = "follow",
            help = "Only the lines that hold this text in any case, each after its number"
        )]
        grep: Option<String>,
    },
    #[command(
        about = "Cancel one job, or every member of a job group; the jobs that need it see it \
                 cancelled and follow their if"
    )]
    Cancel {
        #[arg(help = "The id of the job")]
        job: String,
    },
}

pub(crate) async fn run(context: &Context, command: Command) -> Result<ExitCode, Failure> {
    let client = context.client()?;
    match command {
        Command::Logs {
            job,
            step,
            follow,
            grep,
        } => {
            let shown = match grep {
                Some(text) => Shown::Matching(text),
                None if follow => Shown::Following,
                None => Shown::Stored,
            };
            logs(&client, id(&job)?, step, shown).await?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Cancel { job } => {
            let job: Job = client
                .get(&format!("/api/v1/jobs/{}", id(&job)?), &[])
                .await?;
            let accepted: RunEvent = client
                .post(
                    &format!("/api/v1/runs/{}/commands", job.run_id),
                    &CommandRequest {
                        command: Operation::JobCancel,
                        target: Target {
                            attempt: None,
                            job: Some(job.id),
                            step: None,
                        },
                        reason: None,
                    },
                )
                .await?;
            if context.json() {
                output::json(&accepted)?;
            } else {
                println!("the cancellation of job {} {} is accepted", job.key, job.id);
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn id(text: &str) -> Result<JobId, Failure> {
    text.parse()
        .map_err(|_| Failure::new(format!("`{text}` is not the id of a job")))
}

enum Shown {
    Stored,
    Following,
    Matching(String),
}

async fn logs(
    client: &Client,
    id: JobId,
    only: Option<usize>,
    shown: Shown,
) -> Result<(), Failure> {
    let live = matches!(shown, Shown::Following);
    let Some(execution) = execution(client, id, live).await? else {
        eprintln!("the job ended without reaching an agent, so it has no log");
        return Ok(());
    };
    let steps: Vec<(usize, _)> = match only {
        Some(number) => {
            let step = number
                .checked_sub(1)
                .and_then(|index| execution.steps.get(index))
                .ok_or_else(|| {
                    Failure::new(format!(
                        "the job has {} steps, and none is number {number}",
                        execution.steps.len()
                    ))
                })?;
            vec![(number, step)]
        }
        None => execution
            .steps
            .iter()
            .enumerate()
            .map(|(index, step)| (index + 1, step))
            .collect(),
    };
    let headed = only.is_none();
    for (number, step) in steps {
        if headed {
            println!("==> step {number}: {}", step.name);
        }
        let path = format!("/api/v1/executions/{}/steps/{}/log", execution.id, step.id);
        match &shown {
            Shown::Following => {
                follow::step(client, execution.id, step.id, |line| {
                    println!("{}", line.line)
                })
                .await?;
            }
            Shown::Stored => {
                let text = client.text(&path).await?;
                let mut stdout = io::stdout().lock();
                stdout
                    .write_all(text.as_bytes())
                    .and_then(|()| stdout.flush())
                    .map_err(|error| Failure::new(format!("the log cannot be printed: {error}")))?;
            }
            Shown::Matching(text) => {
                let found: LogSearch = client.get(&path, &[("search", text.as_str())]).await?;
                for line in found.matches {
                    println!("{}: {}", line.number, line.line);
                }
            }
        }
    }
    Ok(())
}

async fn execution(client: &Client, id: JobId, live: bool) -> Result<Option<Execution>, Failure> {
    loop {
        let job: Job = fetch(client, &format!("/api/v1/jobs/{id}"), live).await?;
        match job.execution_id {
            Some(execution) => {
                let execution: Execution =
                    fetch(client, &format!("/api/v1/executions/{execution}"), live).await?;
                let waiting =
                    execution.steps.is_empty() && execution.status != ExecutionStatus::Completed;
                if !(live && waiting) {
                    return Ok(Some(execution));
                }
            }
            None if job.status == JobStatus::Completed => return Ok(None),
            None if !live => {
                return Err(Failure::new(format!(
                    "the job {} has not reached an agent, it is {}; --follow waits for it",
                    job.key,
                    name(&job.status)
                )));
            }
            None => {}
        }
        sleep(POLL).await;
    }
}

async fn fetch<T: DeserializeOwned>(client: &Client, path: &str, live: bool) -> Result<T, Failure> {
    if live {
        patiently(|| client.get(path, &[])).await
    } else {
        client.get(path, &[]).await
    }
}
