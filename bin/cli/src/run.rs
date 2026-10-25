use std::collections::{HashMap, HashSet};
use std::process::ExitCode;
use std::time::Duration;

use bosun_types::actor::Actor;
use bosun_types::api::{Execution, Job, Page, Run, Workflow};
use bosun_types::command::{Command as Operation, CommandRequest, Target};
use bosun_types::event::RunEvent;
use bosun_types::id::{ExecutionId, JobId, RunId, WorkflowId};
use bosun_types::key::JobKey;
use bosun_types::status::{ExecutionStatus, JobStatus, RunConclusion, RunStatus};
use clap::Subcommand;
use tokio::task::{JoinError, JoinSet};
use tokio::time::sleep;

use crate::client::{Client, Failure, diagnostic, patiently};
use crate::output::{self, name};
use crate::{Context, follow, workflow};

const POLL: Duration = Duration::from_secs(1);

const BRANCHES: &str = "refs/heads/";

#[derive(Subcommand)]
pub(crate) enum Command {
    #[command(about = "List the runs you may view, newest first, that match every option given")]
    List {
        #[arg(
            long,
            value_name = "PROJECT",
            help = "Only the runs of a project, as <organisation>/<project> or its id"
        )]
        project: Option<String>,
        #[arg(
            long,
            value_name = "WORKFLOW",
            help = "Only the runs of a workflow, as <organisation>/<project>/<name> or its id"
        )]
        workflow: Option<String>,
        #[arg(
            long,
            value_name = "BRANCH",
            help = "Only the runs of a branch, such as main"
        )]
        branch: Option<String>,
        #[arg(
            long,
            value_name = "EVENT",
            help = "Only the runs of an event, such as push or workflow_dispatch"
        )]
        event: Option<String>,
        #[arg(
            long,
            value_name = "STATUS",
            help = "Only the runs with a status or a conclusion, such as in_progress or failure"
        )]
        status: Option<String>,
        #[arg(
            long,
            value_name = "LOGIN",
            help = "Only the runs whose actor has this login: a user, the user of a token or an \
                    account of the forge"
        )]
        actor: Option<String>,
        #[arg(
            long,
            value_name = "N",
            default_value_t = 50,
            value_parser = clap::value_parser!(u16).range(1..=500),
            help = "The most runs to print, 1 to 500"
        )]
        limit: u16,
    },
    #[command(about = "Print a run, its notices and the jobs of its latest attempt")]
    Show {
        #[arg(help = "The id of the run")]
        run: String,
    },
    #[command(
        about = "Follow a run to its end with the logs of its jobs, and exit with the status of \
                 its conclusion"
    )]
    Watch {
        #[arg(help = "The id of the run", conflicts_with = "json")]
        run: String,
    },
    #[command(
        about = "Cancel a run: its pending and queued jobs end, its running jobs are stopped"
    )]
    Cancel {
        #[arg(help = "The id of the run")]
        run: String,
    },
    #[command(
        about = "Admit a run from a fork that waits for admission: its jobs go on to the agents \
                 of untrusted pools"
    )]
    Admit {
        #[arg(help = "The id of the run")]
        run: String,
    },
}

struct Filters {
    project: Option<String>,
    workflow: Option<String>,
    branch: Option<String>,
    event: Option<String>,
    status: Option<String>,
    actor: Option<String>,
    limit: u16,
}

pub(crate) async fn run(context: &Context, command: Command) -> Result<ExitCode, Failure> {
    let client = context.client()?;
    match command {
        Command::List {
            project,
            workflow,
            branch,
            event,
            status,
            actor,
            limit,
        } => {
            let filters = Filters {
                project,
                workflow,
                branch,
                event,
                status,
                actor,
                limit,
            };
            list(context, &client, filters).await
        }
        Command::Show { run } => show(context, &client, id(&run)?).await,
        Command::Watch { run } => {
            let run: Run = client
                .get(&format!("/api/v1/runs/{}", id(&run)?), &[])
                .await?;
            let workflow: Workflow = client
                .get(&format!("/api/v1/workflows/{}", run.workflow_id), &[])
                .await?;
            watch(&client, &workflow, run).await
        }
        Command::Cancel { run } => {
            operate(
                context,
                &client,
                id(&run)?,
                Operation::RunCancel,
                "cancellation",
            )
            .await
        }
        Command::Admit { run } => {
            operate(
                context,
                &client,
                id(&run)?,
                Operation::RunAdmit,
                "admission",
            )
            .await
        }
    }
}

async fn operate(
    context: &Context,
    client: &Client,
    id: RunId,
    operation: Operation,
    what: &str,
) -> Result<ExitCode, Failure> {
    let accepted: RunEvent = client
        .post(
            &format!("/api/v1/runs/{id}/commands"),
            &CommandRequest {
                command: operation,
                target: Target::default(),
                reason: None,
            },
        )
        .await?;
    if context.json() {
        output::json(&accepted)?;
    } else {
        println!("the {what} of run {id} is accepted");
    }
    Ok(ExitCode::SUCCESS)
}

fn id(text: &str) -> Result<RunId, Failure> {
    text.parse()
        .map_err(|_| Failure::new(format!("`{text}` is not the id of a run")))
}

async fn list(context: &Context, client: &Client, filters: Filters) -> Result<ExitCode, Failure> {
    let workflow = match &filters.workflow {
        Some(reference) => match reference.parse::<WorkflowId>() {
            Ok(id) => Some(id.to_string()),
            Err(_) => Some(workflow::find(client, reference).await?.id.to_string()),
        },
        None => None,
    };
    let limit = filters.limit.to_string();
    let mut query = vec![("limit", limit.as_str())];
    for (name, value) in [
        ("project", &filters.project),
        ("workflow", &workflow),
        ("branch", &filters.branch),
        ("event", &filters.event),
        ("status", &filters.status),
        ("actor", &filters.actor),
    ] {
        if let Some(value) = value {
            query.push((name, value));
        }
    }
    let page: Page<Run> = client.get("/api/v1/runs", &query).await?;
    if context.json() {
        output::json(&page.items)?;
        return Ok(ExitCode::SUCCESS);
    }
    let rows: Vec<Vec<String>> = page
        .items
        .iter()
        .map(|run| {
            vec![
                state(run),
                format!("{} #{}", run.display_name, run.number),
                run.event.clone(),
                branch(run.r#ref.as_deref()),
                actor(&run.actor),
                run.created_at.to_rfc3339(),
                run.id.to_string(),
            ]
        })
        .collect();
    output::table(
        &["STATUS", "RUN", "EVENT", "BRANCH", "ACTOR", "CREATED", "ID"],
        &rows,
    );
    Ok(ExitCode::SUCCESS)
}

async fn show(context: &Context, client: &Client, id: RunId) -> Result<ExitCode, Failure> {
    let run: Run = client.get(&format!("/api/v1/runs/{id}"), &[]).await?;
    if context.json() {
        output::json(&run)?;
        return Ok(ExitCode::SUCCESS);
    }
    let jobs: Vec<Job> = client
        .all(&format!("/api/v1/runs/{id}/attempts/{}/jobs", run.attempt))
        .await?;
    let time = |at: Option<chrono::DateTime<chrono::Utc>>| {
        at.map_or_else(|| "-".to_owned(), |at| at.to_rfc3339())
    };
    output::fields(&[
        ("run", format!("{} #{}", run.display_name, run.number)),
        ("status", state(&run)),
        ("event", run.event.clone()),
        ("ref", run.r#ref.clone().unwrap_or_else(|| "-".to_owned())),
        ("commit", run.sha.clone().unwrap_or_else(|| "-".to_owned())),
        ("actor", actor(&run.actor)),
        ("attempt", run.attempt.to_string()),
        ("created", run.created_at.to_rfc3339()),
        ("started", time(run.started_at)),
        ("completed", time(run.completed_at)),
        ("workflow", run.workflow_id.to_string()),
        ("id", run.id.to_string()),
    ]);
    for notice in &run.notices {
        println!("{}", diagnostic(&notice.diagnostic));
    }
    println!();
    let rows: Vec<Vec<String>> = jobs
        .iter()
        .map(|job| {
            let status = match job.conclusion {
                Some(conclusion) => format!("{} {}", name(&job.status), name(&conclusion)),
                None => name(&job.status),
            };
            vec![
                job.key.to_string(),
                job.name.clone(),
                status,
                job.id.to_string(),
            ]
        })
        .collect();
    output::table(&["JOB", "NAME", "STATUS", "ID"], &rows);
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn state(run: &Run) -> String {
    match run.conclusion {
        Some(conclusion) => format!("{} {}", name(&run.status), name(&conclusion)),
        None => name(&run.status),
    }
}

fn branch(git_ref: Option<&str>) -> String {
    match git_ref {
        Some(git_ref) => git_ref.strip_prefix(BRANCHES).unwrap_or(git_ref).to_owned(),
        None => "-".to_owned(),
    }
}

fn actor(actor: &Actor) -> String {
    match actor {
        Actor::User { login, .. } | Actor::Token { login, .. } | Actor::Forge { login, .. } => {
            login.clone()
        }
        Actor::Agent { name, .. } => format!("agent {name}"),
        Actor::System => "system".to_owned(),
    }
}

fn exit_status(conclusion: RunConclusion) -> u8 {
    match conclusion {
        RunConclusion::Success => 0,
        RunConclusion::Failure => 3,
        RunConclusion::Cancelled => 4,
        RunConclusion::TimedOut => 5,
        RunConclusion::StartupFailure => 6,
        RunConclusion::Skipped => 7,
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
    let Some(conclusion) = run.conclusion else {
        return Err(Failure::new(format!(
            "run {} completed without a conclusion",
            run.id
        )));
    };
    println!("run {}: completed {}", run.id, name(&conclusion));
    Ok(ExitCode::from(exit_status(conclusion)))
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;

    use bosun_types::actor::Actor;
    use bosun_types::status::RunConclusion;

    use super::{actor, branch, exit_status};
    use crate::output::name;

    #[test]
    fn a_watch_exits_with_the_status_the_design_gives_each_conclusion() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/design/11-api.md");
        let document = fs::read_to_string(&path).expect("the API document is readable");
        let documented: BTreeMap<String, u8> = document
            .lines()
            .map(str::trim_start)
            .skip_while(|line| !line.starts_with("| Conclusion | Exit status |"))
            .skip(2)
            .take_while(|line| line.starts_with('|'))
            .map(|line| {
                let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
                (
                    cells[0].trim_matches('`').to_owned(),
                    cells[1].parse().expect("an exit status"),
                )
            })
            .collect();
        let implemented: BTreeMap<String, u8> = RunConclusion::ALL
            .into_iter()
            .map(|conclusion| (name(&conclusion), exit_status(conclusion)))
            .collect();
        assert_eq!(implemented, documented);
        assert_eq!(exit_status(RunConclusion::Success), 0);
        let mut statuses: Vec<u8> = implemented.values().copied().collect();
        statuses.sort_unstable();
        statuses.dedup();
        assert_eq!(statuses.len(), RunConclusion::ALL.len());
        assert!(!statuses.contains(&1) && !statuses.contains(&2));
    }

    #[test]
    fn a_run_reads_as_its_branch_and_the_login_of_its_actor() {
        assert_eq!(branch(Some("refs/heads/feature/guide")), "feature/guide");
        assert_eq!(branch(Some("refs/tags/v1.0.0")), "refs/tags/v1.0.0");
        assert_eq!(branch(None), "-");
        let user_id = "0192f3a4-5b6c-7d8e-9f01-000000000003"
            .parse()
            .expect("a user id");
        assert_eq!(
            actor(&Actor::Token {
                id: "0192f3a4-5b6c-7d8e-9f01-000000000004"
                    .parse()
                    .expect("a token id"),
                user_id,
                login: "dana".to_owned(),
            }),
            "dana"
        );
        assert_eq!(
            actor(&Actor::Forge {
                id: 7001,
                login: "roman".to_owned(),
            }),
            "roman"
        );
        assert_eq!(
            actor(&Actor::Agent {
                id: "0192f3a4-5b6c-7d8e-9f01-000000000009"
                    .parse()
                    .expect("an agent id"),
                name: "runner1-1".to_owned(),
            }),
            "agent runner1-1"
        );
        assert_eq!(actor(&Actor::System), "system");
    }
}
