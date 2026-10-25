use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Duration;

use bosun_types::api::{Execution, Job};
use bosun_types::id::JobId;
use bosun_types::status::{ExecutionStatus, JobStatus};
use clap::Subcommand;
use serde::de::DeserializeOwned;
use tokio::time::sleep;

use crate::client::{Client, Failure, patiently};
use crate::output::name;
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
    },
}

pub(crate) async fn run(context: &Context, command: Command) -> Result<ExitCode, Failure> {
    let client = context.client()?;
    match command {
        Command::Logs { job, step, follow } => {
            let id: JobId = job
                .parse()
                .map_err(|_| Failure::new(format!("`{job}` is not the id of a job")))?;
            logs(&client, id, step, follow).await?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

async fn logs(client: &Client, id: JobId, only: Option<usize>, live: bool) -> Result<(), Failure> {
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
        if live {
            follow::step(client, execution.id, step.id, |line| {
                println!("{}", line.line)
            })
            .await?;
        } else {
            let text = client
                .text(&format!(
                    "/api/v1/executions/{}/steps/{}/log",
                    execution.id, step.id
                ))
                .await?;
            let mut stdout = io::stdout().lock();
            stdout
                .write_all(text.as_bytes())
                .and_then(|()| stdout.flush())
                .map_err(|error| Failure::new(format!("the log cannot be printed: {error}")))?;
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
