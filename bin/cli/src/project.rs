use std::process::ExitCode;

use clap::Subcommand;
use serde_json::{Map, Value};

use crate::client::Failure;
use crate::{Context, output};

#[derive(Subcommand)]
pub(crate) enum Command {
    #[command(about = "Print the settings of a project, or change them and print the result")]
    Settings {
        #[arg(help = "The project, as <organisation>/<project> or its id")]
        project: String,
        #[arg(
            long = "set",
            value_name = "KEY=VALUE",
            value_parser = setting,
            help = "Change a setting; a value that reads as JSON is sent as that JSON, and any \
                    other value as a string"
        )]
        set: Vec<(String, Value)>,
    },
}

pub(crate) async fn run(context: &Context, command: Command) -> Result<ExitCode, Failure> {
    match command {
        Command::Settings { project, set } => settings(context, &project, set).await,
    }
}

async fn settings(
    context: &Context,
    project: &str,
    set: Vec<(String, Value)>,
) -> Result<ExitCode, Failure> {
    let mut changes = Map::new();
    for (key, value) in set {
        if changes.insert(key.clone(), value).is_some() {
            return Err(Failure::new(format!("--set changes `{key}` twice")));
        }
    }
    let client = context.client()?;
    let path = format!("/api/v1/projects/{project}/settings");
    let (mut document, tag): (Value, String) = client.tagged(&path).await?;
    if !changes.is_empty() {
        document = client.patch(&path, Some(&tag), &changes).await?;
    }
    if context.json() {
        output::json(&document)?;
    } else {
        output::table(&["SETTING", "VALUE"], &rows(&document));
    }
    Ok(ExitCode::SUCCESS)
}

fn setting(text: &str) -> Result<(String, Value), String> {
    let (key, value) = text
        .split_once('=')
        .filter(|(key, _)| !key.is_empty())
        .ok_or_else(|| format!("`{text}` is not KEY=VALUE"))?;
    let value = serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_owned()));
    Ok((key.to_owned(), value))
}

fn rows(document: &Value) -> Vec<Vec<String>> {
    document
        .as_object()
        .into_iter()
        .flatten()
        .map(|(key, value)| {
            let shown = match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            vec![key.clone(), shown]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{rows, setting};

    #[test]
    fn a_setting_is_sent_as_json_when_it_reads_as_json_and_as_text_otherwise() {
        for (text, key, value) in [
            ("fork_policy=approval", "fork_policy", json!("approval")),
            ("github_workflows=true", "github_workflows", json!(true)),
            (
                "token_permissions={\"contents\":\"read\"}",
                "token_permissions",
                json!({ "contents": "read" }),
            ),
            ("report=\"checks\"", "report", json!("checks")),
            ("note=a=b", "note", json!("a=b")),
            ("note=", "note", json!("")),
        ] {
            assert_eq!(setting(text), Ok((key.to_owned(), value)), "{text}");
        }
        for text in ["fork_policy", "=approval"] {
            assert!(setting(text).is_err(), "{text}");
        }
    }

    #[test]
    fn the_settings_read_as_one_row_each_with_values_that_are_not_text_as_json() {
        let document = json!({
            "github_workflows": false,
            "fork_policy": "disabled",
            "token_permissions": { "contents": "read", "metadata": "read" },
            "report": "checks"
        });
        let shown: Vec<Vec<String>> = rows(&document);
        let expected: Vec<Vec<String>> = [
            ["fork_policy", "disabled"],
            ["github_workflows", "false"],
            ["report", "checks"],
            [
                "token_permissions",
                "{\"contents\":\"read\",\"metadata\":\"read\"}",
            ],
        ]
        .iter()
        .map(|row| row.iter().map(|cell| (*cell).to_owned()).collect())
        .collect();
        assert_eq!(shown, expected);
        assert!(rows(&Value::Null).is_empty());
    }
}
