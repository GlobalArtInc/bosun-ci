use std::io::{self, IsTerminal, Read};
use std::process::ExitCode;

use bosun_types::api::{Secret, SetSecretRequest};
use clap::{Args, Subcommand};

use crate::client::Failure;
use crate::{Context, login, output};

#[derive(Args)]
#[group(required = true, multiple = false)]
pub(crate) struct Scope {
    #[arg(
        long,
        value_name = "ORGANISATION/PROJECT",
        help = "The project, named by its slugs or id"
    )]
    project: Option<String>,
    #[arg(
        long,
        value_name = "ORGANISATION",
        help = "The organisation, named by its slug or id, whose projects all receive it"
    )]
    organisation: Option<String>,
}

impl Scope {
    pub(crate) fn path(&self, collection: &str, name: Option<&str>) -> String {
        let (key, value) = match (&self.project, &self.organisation) {
            (Some(project), _) => ("project", project),
            (None, Some(organisation)) => ("organisation", organisation),
            (None, None) => ("project", &String::new()),
        };
        let item = name
            .map(|name| format!("/{}", encoded(name)))
            .unwrap_or_default();
        format!("/api/v1/{collection}{item}?{key}={}", encoded(value))
    }

    pub(crate) fn described(&self) -> String {
        match (&self.project, &self.organisation) {
            (Some(project), _) => format!("project {project}"),
            (None, Some(organisation)) => format!("organisation {organisation}"),
            (None, None) => String::new(),
        }
    }
}

fn encoded(text: &str) -> String {
    text.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

#[derive(Subcommand)]
pub(crate) enum Command {
    #[command(
        about = "Set a secret of a project or an organisation, reading its value from standard \
                 input"
    )]
    Set {
        #[arg(help = "The name of the secret, matched without regard to case")]
        name: String,
        #[command(flatten)]
        scope: Scope,
    },
    #[command(about = "List the names of the secrets of a project or an organisation")]
    List {
        #[command(flatten)]
        scope: Scope,
    },
    #[command(about = "Delete a secret of a project or an organisation")]
    Delete {
        #[arg(help = "The name of the secret, matched without regard to case")]
        name: String,
        #[command(flatten)]
        scope: Scope,
    },
}

pub(crate) async fn run(context: &Context, command: Command) -> Result<ExitCode, Failure> {
    let client = context.client()?;
    match command {
        Command::Set { name, scope } => {
            let value = value()?;
            let secret: Secret = client
                .post(
                    &scope.path("secrets", None),
                    &SetSecretRequest { name, value },
                )
                .await?;
            if context.json() {
                output::json(&secret)?;
            } else {
                println!(
                    "{} the secret {} of {}",
                    set_or_replaced(secret.created_at == secret.updated_at),
                    secret.name,
                    scope.described()
                );
            }
        }
        Command::List { scope } => {
            let secrets: Vec<Secret> = client.all(&scope.path("secrets", None)).await?;
            if context.json() {
                output::json(&secrets)?;
            } else {
                let rows: Vec<Vec<String>> = secrets
                    .iter()
                    .map(|secret| vec![secret.name.clone(), secret.updated_at.to_rfc3339()])
                    .collect();
                output::table(&["NAME", "UPDATED"], &rows);
            }
        }
        Command::Delete { name, scope } => {
            client.delete(&scope.path("secrets", Some(&name))).await?;
            println!(
                "deleted the secret {} of {}",
                name.to_ascii_uppercase(),
                scope.described()
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn set_or_replaced(created: bool) -> &'static str {
    if created { "set" } else { "replaced" }
}

fn value() -> Result<String, Failure> {
    let stdin = io::stdin();
    let text = if stdin.is_terminal() {
        eprint!("Value of the secret: ");
        login::hidden(&stdin)
    } else {
        let mut text = String::new();
        stdin.lock().read_to_string(&mut text).map(|_| text)
    }
    .map_err(|error| Failure::new(format!("the value could not be read: {error}")))?;
    let value = stripped(&text);
    if value.is_empty() {
        return Err(Failure::new(
            "no value arrived on standard input; pass the value of the secret there",
        ));
    }
    Ok(value.to_owned())
}

fn stripped(text: &str) -> &str {
    text.trim_end_matches(['\r', '\n'])
}

#[cfg(test)]
mod tests {
    use super::{Scope, stripped};

    #[test]
    fn a_value_from_standard_input_loses_only_the_line_ends_that_end_it() {
        assert_eq!(stripped("s3cr3t\n"), "s3cr3t");
        assert_eq!(stripped("s3cr3t\r\n\n"), "s3cr3t");
        assert_eq!(stripped("first\nsecond\n"), "first\nsecond");
        assert_eq!(stripped("  spaced  \n"), "  spaced  ");
        assert_eq!(stripped("\n"), "");
    }

    #[test]
    fn a_scope_names_its_project_or_organisation_in_the_query() {
        let project = Scope {
            project: Some("globalart/keel".to_owned()),
            organisation: None,
        };
        assert_eq!(
            project.path("secrets", Some("deploy key")),
            "/api/v1/secrets/deploy%20key?project=globalart/keel"
        );
        let organisation = Scope {
            project: None,
            organisation: Some("global&art".to_owned()),
        };
        assert_eq!(
            organisation.path("variables", None),
            "/api/v1/variables?organisation=global%26art"
        );
        assert_eq!(organisation.described(), "organisation global&art");
    }
}
