use std::process::ExitCode;

use bosun_types::api::Agent;
use clap::Subcommand;

use crate::Context;
use crate::client::Failure;
use crate::output::{self, name};

#[derive(Subcommand)]
pub(crate) enum Command {
    #[command(about = "List the registered agents with their labels and what they run")]
    List,
}

pub(crate) async fn run(context: &Context, command: Command) -> Result<ExitCode, Failure> {
    let client = context.client()?;
    match command {
        Command::List => {
            let agents: Vec<Agent> = client.all("/api/v1/agents").await?;
            if context.json() {
                output::json(&agents)?;
                return Ok(ExitCode::SUCCESS);
            }
            let rows: Vec<Vec<String>> = agents
                .iter()
                .map(|agent| {
                    vec![
                        agent.name.clone(),
                        if agent.online { "online" } else { "offline" }.to_owned(),
                        agent.labels.join(","),
                        agent
                            .capabilities
                            .iter()
                            .map(name)
                            .collect::<Vec<_>>()
                            .join(","),
                        agent.version.clone().unwrap_or_else(|| "-".to_owned()),
                        agent
                            .execution_id
                            .map_or_else(|| "-".to_owned(), |id| id.to_string()),
                        agent.id.to_string(),
                    ]
                })
                .collect();
            output::table(
                &[
                    "NAME",
                    "STATE",
                    "LABELS",
                    "CAPABILITIES",
                    "VERSION",
                    "RUNNING",
                    "ID",
                ],
                &rows,
            );
            Ok(ExitCode::SUCCESS)
        }
    }
}
