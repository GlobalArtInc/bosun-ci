use std::process::ExitCode;

use bosun_types::api::{Agent, UpdateAgentRequest};
use clap::Subcommand;

use crate::Context;
use crate::client::{Client, Failure};
use crate::output::{self, name};

#[derive(Subcommand)]
pub(crate) enum Command {
    #[command(about = "List the registered agents with their labels and what they run")]
    List,
    #[command(
        about = "Drain an agent: it finishes the job it runs and is offered no other until the \
                 drain is lifted"
    )]
    Drain {
        #[arg(help = "The agent, by its id or by a name no other agent has")]
        agent: String,
        #[arg(long, help = "Lift the drain, so that the agent is offered jobs again")]
        lift: bool,
    },
    #[command(
        about = "Delete an agent: revoke its credential, end its stream and fail the job it runs"
    )]
    Delete {
        #[arg(help = "The agent, by its id or by a name no other agent has")]
        agent: String,
    },
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
                        state(agent),
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
        Command::Drain { agent, lift } => {
            let found = resolve(&client, &agent).await?;
            let changed: Agent = client
                .patch(
                    &format!("/api/v1/agents/{}", found.id),
                    None,
                    &UpdateAgentRequest { drain: Some(!lift) },
                )
                .await?;
            if context.json() {
                output::json(&changed)?;
            } else if lift {
                println!(
                    "lifted the drain of the agent {} {}; it is offered jobs again",
                    changed.name, changed.id
                );
            } else {
                println!(
                    "drained the agent {} {}; it finishes the job it runs and is offered no other",
                    changed.name, changed.id
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Delete { agent } => {
            let found = resolve(&client, &agent).await?;
            client
                .delete(&format!("/api/v1/agents/{}", found.id))
                .await?;
            if context.json() {
                output::json(&found)?;
            } else {
                println!(
                    "deleted the agent {} {} and revoked its credential",
                    found.name, found.id
                );
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn state(agent: &Agent) -> String {
    let connection = if agent.online { "online" } else { "offline" };
    if agent.drain {
        format!("{connection}, drained")
    } else {
        connection.to_owned()
    }
}

async fn resolve(client: &Client, reference: &str) -> Result<Agent, Failure> {
    let agents: Vec<Agent> = client.all("/api/v1/agents").await?;
    pick(agents, reference)
}

fn pick(agents: Vec<Agent>, reference: &str) -> Result<Agent, Failure> {
    if let Some(agent) = agents
        .iter()
        .find(|agent| agent.id.to_string() == reference)
    {
        return Ok(agent.clone());
    }
    let mut named: Vec<Agent> = agents
        .into_iter()
        .filter(|agent| agent.name == reference)
        .collect();
    match named.len() {
        0 => Err(Failure::new(format!("there is no agent `{reference}`"))),
        1 => Ok(named.remove(0)),
        count => {
            let mut failure = Failure::new(format!(
                "{count} agents are named `{reference}`; name one by its id"
            ));
            failure
                .details
                .extend(named.iter().map(|agent| agent.id.to_string()));
            Err(failure)
        }
    }
}

#[cfg(test)]
mod tests {
    use bosun_types::api::Agent;
    use chrono::Utc;

    use super::{pick, state};

    fn agent(number: u8, name: &str) -> Agent {
        Agent {
            id: format!("0192f3a4-5b6c-7d8e-9f01-0000000000{number:02x}")
                .parse()
                .expect("an agent id"),
            name: name.to_owned(),
            pool_id: "0192f3a4-5b6c-7d8e-9f01-00000000000d"
                .parse()
                .expect("a pool id"),
            labels: Vec::new(),
            capabilities: Vec::new(),
            version: None,
            online: true,
            drain: false,
            last_seen_at: None,
            execution_id: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn an_agent_is_named_by_its_id_or_by_a_name_no_other_agent_has() {
        let first = agent(1, "runner1-1");
        let second = agent(2, "runner1-2");
        let twin = agent(3, "runner1-2");
        let agents = vec![first.clone(), second.clone(), twin.clone()];

        assert_eq!(pick(agents.clone(), "runner1-1").ok(), Some(first));
        assert_eq!(
            pick(agents.clone(), &twin.id.to_string()).ok(),
            Some(twin.clone())
        );
        let ambiguous = pick(agents.clone(), "runner1-2").expect_err("a refusal");
        assert_eq!(
            ambiguous.message,
            "2 agents are named `runner1-2`; name one by its id"
        );
        assert_eq!(
            ambiguous.details,
            [second.id.to_string(), twin.id.to_string()]
        );
        let unknown = pick(agents, "runner9-9").expect_err("a refusal");
        assert_eq!(unknown.message, "there is no agent `runner9-9`");
    }

    #[test]
    fn a_drained_agent_says_so_beside_its_connection() {
        let mut drained = agent(1, "runner1-1");
        drained.drain = true;
        assert_eq!(state(&drained), "online, drained");
        drained.online = false;
        assert_eq!(state(&drained), "offline, drained");
        assert_eq!(state(&agent(2, "runner1-2")), "online");
    }
}
