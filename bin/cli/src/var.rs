use std::process::ExitCode;

use bosun_types::api::{SetVariableRequest, Variable};
use clap::Subcommand;

use crate::client::Failure;
use crate::secret::{Scope, set_or_replaced};
use crate::{Context, output};

#[derive(Subcommand)]
pub(crate) enum Command {
    #[command(about = "Set a variable of a project or an organisation")]
    Set {
        #[arg(help = "The name of the variable, matched without regard to case")]
        name: String,
        #[arg(help = "The value of the variable")]
        value: String,
        #[command(flatten)]
        scope: Scope,
    },
    #[command(about = "List the variables of a project or an organisation with their values")]
    List {
        #[command(flatten)]
        scope: Scope,
    },
    #[command(about = "Delete a variable of a project or an organisation")]
    Delete {
        #[arg(help = "The name of the variable, matched without regard to case")]
        name: String,
        #[command(flatten)]
        scope: Scope,
    },
}

pub(crate) async fn run(context: &Context, command: Command) -> Result<ExitCode, Failure> {
    let client = context.client()?;
    match command {
        Command::Set { name, value, scope } => {
            let variable: Variable = client
                .post(
                    &scope.path("variables", None),
                    &SetVariableRequest { name, value },
                )
                .await?;
            if context.json() {
                output::json(&variable)?;
            } else {
                println!(
                    "{} the variable {} of {}",
                    set_or_replaced(variable.created_at == variable.updated_at),
                    variable.name,
                    scope.described()
                );
            }
        }
        Command::List { scope } => {
            let variables: Vec<Variable> = client.all(&scope.path("variables", None)).await?;
            if context.json() {
                output::json(&variables)?;
            } else {
                let rows: Vec<Vec<String>> = variables
                    .iter()
                    .map(|variable| {
                        vec![
                            variable.name.clone(),
                            variable.value.clone(),
                            variable.updated_at.to_rfc3339(),
                        ]
                    })
                    .collect();
                output::table(&["NAME", "VALUE", "UPDATED"], &rows);
            }
        }
        Command::Delete { name, scope } => {
            client.delete(&scope.path("variables", Some(&name))).await?;
            println!(
                "deleted the variable {} of {}",
                name.to_ascii_uppercase(),
                scope.described()
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}
