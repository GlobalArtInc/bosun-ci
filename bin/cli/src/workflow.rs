use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use bosun_types::api::{
    CreateWorkflowRequest, DispatchRequest, Page, Revision, Run, UpdateSourceRequest, Workflow,
};
use bosun_types::id::WorkflowId;
use clap::Subcommand;

use crate::client::{Client, Failure};
use crate::{Context, output, run};

mod inventory;

#[derive(Subcommand)]
pub(crate) enum Command {
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

async fn find(client: &Client, reference: &str) -> Result<Workflow, Failure> {
    if let Ok(id) = reference.parse::<WorkflowId>() {
        return client.get(&format!("/api/v1/workflows/{id}"), &[]).await;
    }
    let Some((project, name)) = reference
        .rsplit_once('/')
        .filter(|(project, name)| project.contains('/') && !name.is_empty())
    else {
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
