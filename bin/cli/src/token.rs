use std::process::ExitCode;

use bosun_types::api::{CreatePersonalTokenRequest, PersonalToken, PersonalTokenCreated};
use chrono::Utc;
use clap::Subcommand;

use crate::Context;
use crate::client::Failure;
use crate::output;
use crate::pool::{DURATION_RULE, lifetime};

#[derive(Subcommand)]
pub(crate) enum Command {
    #[command(
        about = "Create a personal token bst_… that carries the permission keys --scope names, and \
                 print it once"
    )]
    Create {
        #[arg(long, help = "The name of the token")]
        name: String,
        #[arg(
            long = "scope",
            value_name = "PERMISSION",
            required = true,
            help = "A permission key the token carries, such as project.view; repeatable"
        )]
        scopes: Vec<String>,
        #[arg(
            long,
            value_name = "DURATION",
            help = "How long the token lives, such as 30m, 12h or 90d; a year at most"
        )]
        expires: String,
        #[arg(
            long = "organisation",
            value_name = "ORGANISATION",
            help = "Restrict the token to an organisation, named by slug or id; repeatable"
        )]
        organisations: Vec<String>,
        #[arg(
            long = "project",
            value_name = "ORGANISATION/PROJECT",
            help = "Restrict the token to one project, named by its slugs or id; repeatable"
        )]
        projects: Vec<String>,
    },
}

pub(crate) async fn run(context: &Context, command: Command) -> Result<ExitCode, Failure> {
    let client = context.client()?;
    match command {
        Command::Create {
            name,
            scopes,
            expires,
            organisations,
            projects,
        } => {
            let lifetime = lifetime(&expires).ok_or_else(|| {
                Failure::new(format!(
                    "--expires is `{expires}`, which is not a duration: {DURATION_RULE}"
                ))
            })?;
            let created: PersonalTokenCreated = client
                .post(
                    "/api/v1/tokens",
                    &CreatePersonalTokenRequest {
                        name,
                        permissions: scopes,
                        organisations,
                        projects,
                        expires_at: Utc::now() + lifetime,
                    },
                )
                .await?;
            if context.json() {
                output::json(&created)?;
                return Ok(ExitCode::SUCCESS);
            }
            println!("{}", created.token);
            eprintln!("{}", described(&created.personal));
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn described(token: &PersonalToken) -> String {
    let keys: Vec<&str> = token
        .permissions
        .iter()
        .map(|permission| permission.key())
        .collect();
    let reach = if token.restricted {
        let places: Vec<&str> = token
            .organisations
            .iter()
            .map(|organisation| organisation.slug.as_str())
            .chain(token.projects.iter().map(|project| project.path.as_str()))
            .collect();
        format!("on {}", places.join(", "))
    } else {
        "wherever its creator holds them".to_owned()
    };
    format!(
        "the token {} carries {} {reach} until {}; it is shown only once",
        token.name,
        keys.join(", "),
        token.expires_at.to_rfc3339()
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use bosun_types::api::{OrganisationRef, PersonalToken, ProjectRef};
    use bosun_types::permission::Permission;
    use chrono::{DateTime, Utc};

    use super::described;

    const ID: &str = "0192f3a4-5b6c-7d8e-9f01-000000000004";

    fn token(restricted: bool) -> PersonalToken {
        let expires_at: DateTime<Utc> = "2027-04-24T18:34:50Z".parse().expect("a time");
        PersonalToken {
            id: ID.parse().expect("an id"),
            name: "ci".to_owned(),
            permissions: BTreeSet::from([Permission::RunDispatch, Permission::ProjectView]),
            restricted,
            organisations: if restricted {
                vec![OrganisationRef {
                    id: ID.parse().expect("an id"),
                    slug: "ownlate".to_owned(),
                }]
            } else {
                Vec::new()
            },
            projects: if restricted {
                vec![ProjectRef {
                    id: ID.parse().expect("an id"),
                    organisation_id: ID.parse().expect("an id"),
                    path: "globalart/keel".to_owned(),
                }]
            } else {
                Vec::new()
            },
            created_at: expires_at,
            expires_at,
            revoked_at: None,
        }
    }

    #[test]
    fn a_token_reads_as_its_keys_where_it_reaches_and_its_expiry() {
        assert_eq!(
            described(&token(true)),
            "the token ci carries project.view, run.dispatch on ownlate, globalart/keel until \
             2027-04-24T18:34:50+00:00; it is shown only once"
        );
        assert_eq!(
            described(&token(false)),
            "the token ci carries project.view, run.dispatch wherever its creator holds them \
             until 2027-04-24T18:34:50+00:00; it is shown only once"
        );
    }
}
