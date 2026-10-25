use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use bosun_types::api::{
    CreateWorkflowRequest, DispatchRequest, Page, Project, Revision, Run, UpdateSourceRequest,
    UpdateWorkflowRequest, Workflow, WorkflowSource,
};
use bosun_types::id::WorkflowId;
use clap::Subcommand;

use crate::client::{Client, Failure};
use crate::{Context, output, run};

mod inventory;

#[derive(Subcommand)]
pub(crate) enum Command {
    #[command(
        about = "List the workflows of a project, with their latest runs on the default branch"
    )]
    List {
        #[arg(help = "The project, as <organisation>/<project> or its id")]
        project: String,
    },
    #[command(about = "Print a workflow and its latest run on the default branch")]
    Show {
        #[arg(help = "The workflow, as <organisation>/<project>/<name> or its id")]
        workflow: String,
    },
    #[command(about = "Enable a workflow, so that it is dispatched and its triggers run it")]
    Enable {
        #[arg(help = "The workflow, as <organisation>/<project>/<name> or its id")]
        workflow: String,
    },
    #[command(
        about = "Disable a workflow: a dispatch of it is refused and no event or schedule runs it"
    )]
    Disable {
        #[arg(help = "The workflow, as <organisation>/<project>/<name> or its id")]
        workflow: String,
    },
    #[command(
        about = "Create a server-side workflow from a file, or save the file as the workflow's new \
                 source"
    )]
    Push {
        #[arg(help = "The project, as <organisation>/<project> or its id")]
        project: String,
        #[arg(help = "The workflow file")]
        file: PathBuf,
        #[arg(
            long,
            value_name = "NAME",
            help = "The name of the workflow [default: the file name without its extension]"
        )]
        name: Option<String>,
    },
    #[command(about = "Start a run of a workflow")]
    Dispatch {
        #[arg(help = "The workflow, as <organisation>/<project>/<name> or its id")]
        workflow: String,
        #[arg(
            long,
            conflicts_with = "json",
            help = "Follow the run to its end with the logs of its jobs, and exit with 1 unless it \
                    succeeds"
        )]
        watch: bool,
    },
    #[command(
        about = "Count every `uses:` and every key of the workflow files under a directory, and \
                 write the counts as the inventory of the compatibility corpus"
    )]
    Inventory {
        #[arg(help = "The directory, searched for *.yml and *.yaml files at any depth")]
        directory: PathBuf,
        #[arg(
            long,
            value_name = "DIRECTORY",
            default_value = "compat/inventory",
            help = "The directory that receives uses.json and keys.json"
        )]
        output: PathBuf,
    },
}

pub(crate) async fn run(context: &Context, command: Command) -> Result<ExitCode, Failure> {
    match command {
        Command::List { project } => {
            let workflows: Vec<Workflow> = context
                .client()?
                .all(&format!("/api/v1/projects/{project}/workflows"))
                .await?;
            if context.json() {
                output::json(&workflows)?;
                return Ok(ExitCode::SUCCESS);
            }
            let rows: Vec<Vec<String>> = workflows
                .iter()
                .map(|workflow| {
                    vec![
                        workflow.name.clone(),
                        source(&workflow.source).to_owned(),
                        state(workflow).to_owned(),
                        latest(workflow.latest_run.as_ref()),
                        workflow.id.to_string(),
                    ]
                })
                .collect();
            output::table(&["WORKFLOW", "SOURCE", "STATE", "LATEST RUN", "ID"], &rows);
            Ok(ExitCode::SUCCESS)
        }
        Command::Show { workflow } => {
            let client = context.client()?;
            let workflow = find(&client, &workflow).await?;
            if context.json() {
                output::json(&workflow)?;
                return Ok(ExitCode::SUCCESS);
            }
            let project: Project = client
                .get(&format!("/api/v1/projects/{}", workflow.project_id), &[])
                .await?;
            output::fields(&[
                ("workflow", workflow.name.clone()),
                ("project", project.path),
                ("source", source(&workflow.source).to_owned()),
                ("state", state(&workflow).to_owned()),
                (
                    "revision",
                    workflow
                        .revision_id
                        .map_or_else(|| "-".to_owned(), |id| id.to_string()),
                ),
                ("latest run", latest(workflow.latest_run.as_ref())),
                (
                    "latest run id",
                    workflow
                        .latest_run
                        .as_ref()
                        .map_or_else(|| "-".to_owned(), |run| run.id.to_string()),
                ),
                ("id", workflow.id.to_string()),
                ("created", workflow.created_at.to_rfc3339()),
            ]);
            Ok(ExitCode::SUCCESS)
        }
        Command::Enable { workflow } => switch(context, &workflow, true).await,
        Command::Disable { workflow } => switch(context, &workflow, false).await,
        Command::Push {
            project,
            file,
            name,
        } => push(context, &context.client()?, &project, &file, name).await,
        Command::Dispatch { workflow, watch } => {
            let client = context.client()?;
            let workflow = find(&client, &workflow).await?;
            let run: Run = client
                .post(
                    &format!("/api/v1/workflows/{}/dispatch", workflow.id),
                    &DispatchRequest::default(),
                )
                .await?;
            if watch {
                return run::watch(&client, &workflow, run).await;
            }
            if context.json() {
                output::json(&run)?;
            } else {
                println!(
                    "dispatched run {} of the workflow {}, #{}",
                    run.id, workflow.name, run.number
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Inventory { directory, output } => inventory::write(context, &directory, &output),
    }
}

async fn push(
    context: &Context,
    client: &Client,
    project: &str,
    file: &Path,
    name: Option<String>,
) -> Result<ExitCode, Failure> {
    let source = fs::read_to_string(file)
        .map_err(|error| Failure::new(format!("{} is not readable: {error}", file.display())))?;
    let name = match name {
        Some(name) => name,
        None => file
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_owned)
            .ok_or_else(|| {
                Failure::new(format!(
                    "{} names no workflow; give it a name with --name",
                    file.display()
                ))
            })?,
    };
    let workflows = format!("/api/v1/projects/{project}/workflows");
    let existing: Page<Workflow> = client.get(&workflows, &[("name", &name)]).await?;
    let (workflow, said) = match existing.items.into_iter().next() {
        Some(workflow) => {
            let revision: Revision = client
                .put(
                    &format!("/api/v1/workflows/{}/source", workflow.id),
                    &UpdateSourceRequest { source },
                )
                .await?;
            let said = if workflow.revision_id == Some(revision.id) {
                format!(
                    "the workflow {} ({}) already has this source",
                    workflow.name, workflow.id
                )
            } else {
                format!(
                    "saved the workflow {} ({}) as revision {}",
                    workflow.name, workflow.id, revision.id
                )
            };
            let workflow: Workflow = client
                .get(&format!("/api/v1/workflows/{}", workflow.id), &[])
                .await?;
            (workflow, said)
        }
        None => {
            let workflow: Workflow = client
                .post(&workflows, &CreateWorkflowRequest { name, source })
                .await?;
            let said = format!("created the workflow {} ({})", workflow.name, workflow.id);
            (workflow, said)
        }
    };
    if context.json() {
        output::json(&workflow)?;
    } else {
        println!("{said}");
    }
    Ok(ExitCode::SUCCESS)
}

async fn switch(context: &Context, reference: &str, enabled: bool) -> Result<ExitCode, Failure> {
    let client = context.client()?;
    let found = find(&client, reference).await?;
    let changed: Workflow = client
        .patch(
            &format!("/api/v1/workflows/{}", found.id),
            None,
            &UpdateWorkflowRequest {
                enabled: Some(enabled),
            },
        )
        .await?;
    if context.json() {
        output::json(&changed)?;
    } else if enabled {
        println!(
            "enabled the workflow {} {}; it is dispatched and its triggers run it",
            changed.name, changed.id
        );
    } else {
        println!(
            "disabled the workflow {} {}; a dispatch of it is refused and nothing else runs it",
            changed.name, changed.id
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn source(source: &WorkflowSource) -> &'static str {
    match source {
        WorkflowSource::Server => "server",
        WorkflowSource::Repository => "repository",
    }
}

fn state(workflow: &Workflow) -> &'static str {
    if workflow.enabled {
        "enabled"
    } else {
        "disabled"
    }
}

fn latest(run: Option<&Run>) -> String {
    run.map_or_else(
        || "-".to_owned(),
        |latest| format!("#{} {}", latest.number, run::state(latest)),
    )
}

pub(crate) async fn find(client: &Client, reference: &str) -> Result<Workflow, Failure> {
    if let Ok(id) = reference.parse::<WorkflowId>() {
        return client.get(&format!("/api/v1/workflows/{id}"), &[]).await;
    }
    let Some((project, name)) = split(reference) else {
        return Err(Failure::new(format!(
            "`{reference}` names no workflow: give its id or <organisation>/<project>/<name>"
        )));
    };
    let found: Page<Workflow> = client
        .get(
            &format!("/api/v1/projects/{project}/workflows"),
            &[("name", name)],
        )
        .await?;
    found
        .items
        .into_iter()
        .next()
        .ok_or_else(|| Failure::new(format!("the project {project} has no workflow `{name}`")))
}

fn split(reference: &str) -> Option<(String, &str)> {
    let mut parts = reference.splitn(3, '/');
    let (organisation, slug, name) = (parts.next()?, parts.next()?, parts.next()?);
    (!organisation.is_empty() && !slug.is_empty() && !name.is_empty())
        .then(|| (format!("{organisation}/{slug}"), name))
}

#[cfg(test)]
mod tests {
    use super::split;

    #[test]
    fn a_workflow_is_named_by_its_project_and_a_name_that_may_be_a_path() {
        assert_eq!(split("e2e/m0/ci"), Some(("e2e/m0".to_owned(), "ci")));
        assert_eq!(
            split("globalartinc/ops/.bosun/workflows/ci.yml"),
            Some(("globalartinc/ops".to_owned(), ".bosun/workflows/ci.yml"))
        );
        for refused in ["ci", "e2e/ci", "e2e/m0/", "/m0/ci", "e2e//ci"] {
            assert_eq!(split(refused), None, "`{refused}` was taken");
        }
    }
}
