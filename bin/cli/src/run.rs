use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::time::Duration;

use bosun_types::api::{Execution, Job, Run, Workflow};
use bosun_types::command::{Command as Operation, CommandRequest, Target};
use bosun_types::event::RunEvent;
use bosun_types::id::{ExecutionId, JobId, RunId};
use bosun_types::key::JobKey;
use bosun_types::status::{ExecutionStatus, JobStatus, RunConclusion, RunStatus};
use clap::Subcommand;
use tokio::task::{JoinError, JoinSet};
use tokio::time::sleep;

use crate::client::{Client, Failure, patiently};
use crate::output::{self, name};
use crate::{Context, follow};

const POLL: Duration = Duration::from_secs(1);

#[derive(Subcommand)]
pub(crate) enum Command {
    #[command(
        about = "Cancel a run: its pending and queued jobs end, its running jobs are stopped"
    )]
    Cancel {
        #[arg(help = "The id of the run")]
        run: String,
    },
}

pub(crate) async fn run(context: &Context, command: Command) -> Result<ExitCode, Failure> {
    let client = context.client()?;
    match command {
        Command::Cancel { run } => {
            let id: RunId = run
                .parse()
                .map_err(|_| Failure::new(format!("`{run}` is not the id of a run")))?;
            let accepted: RunEvent = client
                .post(
                    &format!("/api/v1/runs/{id}/commands"),
                    &CommandRequest {
                        command: Operation::RunCancel,
                        target: Target::default(),
                        reason: None,
                    },
                )
                .await?;
            if context.json() {
                output::json(&accepted)?;
            } else {
                println!("the cancellation of run {id} is accepted");
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

struct Seen {
    status: JobStatus,
    conclusion: Option<String>,
}

#[derive(Default)]
struct Progress {
    seen: HashMap<JobId, Seen>,
    followed: HashSet<ExecutionId>,
    following: HashMap<JobId, usize>,
    deferred: HashMap<JobId, String>,
    followers: JoinSet<Result<JobId, Failure>>,
}

pub(crate) async fn watch(
    client: &Client,
    workflow: &Workflow,
    run: Run,
) -> Result<ExitCode, Failure> {
    println!(
        "run {} of the workflow {}, #{}",
        run.id, workflow.name, run.number
    );
    let mut progress = Progress::default();
    let path = format!("/api/v1/runs/{}", run.id);
    let run = loop {
        let current: Run = patiently(|| client.get(&path, &[])).await?;
        let jobs_path = format!(
            "/api/v1/runs/{}/attempts/{}/jobs",
            current.id, current.attempt
        );
        let jobs: Vec<Job> = patiently(|| client.all(&jobs_path)).await?;
        for job in &jobs {
            progress.report(client, job);
        }
        if current.status == RunStatus::Completed {
            break current;
        }
        while let Some(finished) = progress.followers.try_join_next() {
            progress.finished(finished)?;
        }
        sleep(POLL).await;
    };
    while let Some(finished) = progress.followers.join_next().await {
        progress.finished(finished)?;
    }
    let conclusion = run
        .conclusion
        .map_or_else(|| "none".to_owned(), |conclusion| name(&conclusion));
    println!("run {}: completed {conclusion}", run.id);
    Ok(if run.conclusion == Some(RunConclusion::Success) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

impl Progress {
    fn report(&mut self, client: &Client, job: &Job) {
        if let Some(execution) = job.execution_id.filter(|id| self.followed.insert(*id)) {
            *self.following.entry(job.id).or_default() += 1;
            let (client, key, id) = (client.clone(), job.key.clone(), job.id);
            self.followers
                .spawn(async move { follow_job(&client, &key, execution).await.map(|()| id) });
        }
        let conclusion = job.conclusion.map(|conclusion| name(&conclusion));
        let state = match &conclusion {
            Some(conclusion) => format!("{} {conclusion}", name(&job.status)),
            None => name(&job.status),
        };
        let line = match self.seen.get(&job.id) {
            None => format!("{}: {state}, job {}", job.key, job.id),
            Some(before) if before.status != job.status || before.conclusion != conclusion => {
                format!("{}: {state}", job.key)
            }
            Some(_) => return,
        };
        if job.status == JobStatus::Completed && self.following.contains_key(&job.id) {
            self.deferred.insert(job.id, line);
        } else {
            println!("{line}");
        }
        self.seen.insert(
            job.id,
            Seen {
                status: job.status,
                conclusion,
            },
        );
    }

    fn finished(
        &mut self,
        finished: Result<Result<JobId, Failure>, JoinError>,
    ) -> Result<(), Failure> {
        let job = finished
            .map_err(|error| Failure::new(format!("following a job failed: {error}")))??;
        if let Some(count) = self.following.get_mut(&job) {
            *count -= 1;
            if *count == 0 {
                self.following.remove(&job);
                if let Some(line) = self.deferred.remove(&job) {
                    println!("{line}");
                }
            }
        }
        Ok(())
    }
}

async fn follow_job(client: &Client, key: &JobKey, id: ExecutionId) -> Result<(), Failure> {
    let path = format!("/api/v1/executions/{id}");
    loop {
        let execution: Execution = patiently(|| client.get(&path, &[])).await?;
        if !execution.steps.is_empty() {
            for step in &execution.steps {
                follow::step(client, id, step.id, |line| {
                    println!("{key} | {}", output::text(&line.line));
                })
                .await?;
            }
            return Ok(());
        }
        if execution.status == ExecutionStatus::Completed {
            return Ok(());
        }
        sleep(POLL).await;
    }
}
