use std::process::ExitCode;

use bosun_types::api::{RegistrationTokenCreated, RegistrationTokenRequest};
use chrono::{TimeDelta, Utc};
use clap::Subcommand;

use crate::Context;
use crate::client::Failure;
use crate::output;

const DURATION_RULE: &str = "a duration is a whole number followed by m, h or d, such as 12h";

#[derive(Subcommand)]
pub(crate) enum Command {
    #[command(
        subcommand,
        about = "Registration tokens, with which agents join a pool"
    )]
    Token(Token),
}

#[derive(Subcommand)]
pub(crate) enum Token {
    #[command(about = "Create a registration token brt_… for a pool and print it once")]
    Create {
        #[arg(help = "The name of the pool")]
        pool: String,
        #[arg(
            long,
            value_name = "DURATION",
            help = "How long the token lives, such as 30m, 12h or 7d [default: the server's, 24h]"
        )]
        expires: Option<String>,
        #[arg(long, help = "Let the token register any number of agents, not one")]
        reusable: bool,
    },
}

pub(crate) async fn run(context: &Context, command: Command) -> Result<ExitCode, Failure> {
    let client = context.client()?;
    match command {
        Command::Token(Token::Create {
            pool,
            expires,
            reusable,
        }) => {
            let expires_at = match expires {
                Some(text) => Some(
                    Utc::now()
                        + lifetime(&text).ok_or_else(|| {
                            Failure::new(format!(
                                "--expires is `{text}`, which is not a duration: {DURATION_RULE}"
                            ))
                        })?,
                ),
                None => None,
            };
            let created: RegistrationTokenCreated = client
                .post(
                    "/api/v1/registration-tokens",
                    &RegistrationTokenRequest {
                        pool: pool.clone(),
                        reusable,
                        expires_at,
                    },
                )
                .await?;
            if context.json() {
                output::json(&created)?;
            } else {
                let uses = if created.reusable {
                    "any number of agents"
                } else {
                    "one agent"
                };
                println!("{}", created.token);
                eprintln!(
                    "the token registers {uses} in the pool {pool} until {}; it is shown only once",
                    created.expires_at.to_rfc3339()
                );
                eprintln!("to install an agent with it on a Linux host, run there:");
                eprintln!("{}", install_command(client.server(), &created.token));
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn install_command(server: &str, token: &str) -> String {
    format!("curl -fsSL {server}/install-agent.sh | sudo sh -s -- --token {token}")
}

fn lifetime(text: &str) -> Option<TimeDelta> {
    let split = text
        .find(|character: char| !character.is_ascii_digit())
        .filter(|split| *split > 0)?;
    let (number, unit) = text.split_at(split);
    let number: i64 = number.parse().ok()?;
    let lifetime = match unit {
        "m" => TimeDelta::try_minutes(number)?,
        "h" => TimeDelta::try_hours(number)?,
        "d" => TimeDelta::try_days(number)?,
        _ => return None,
    };
    (lifetime > TimeDelta::zero()).then_some(lifetime)
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::{install_command, lifetime};

    #[test]
    fn the_install_command_fetches_the_script_from_the_server_and_passes_the_token() {
        assert_eq!(
            install_command("https://ci.example.org", "brt_x"),
            "curl -fsSL https://ci.example.org/install-agent.sh | sudo sh -s -- --token brt_x"
        );
    }

    #[test]
    fn a_lifetime_is_a_whole_number_of_minutes_hours_or_days() {
        assert_eq!(lifetime("30m"), Some(TimeDelta::minutes(30)));
        assert_eq!(lifetime("12h"), Some(TimeDelta::hours(12)));
        assert_eq!(lifetime("7d"), Some(TimeDelta::days(7)));
        for refused in [
            "",
            "0h",
            "12",
            "h",
            "1.5h",
            "-1h",
            "12 h",
            "2w",
            "99999999999999d",
        ] {
            assert_eq!(lifetime(refused), None, "`{refused}` was accepted");
        }
    }
}
