use bosun_expr::Value;
use serde_json::json;

use crate::evaluation::{Recorder, field};
use crate::{read, repository};

const RECORDINGS: &str = "compat/recordings/expr.json";
const FIXTURE: &str = "compat/fixtures/recordings/expr.json";

fn string(recording: &Value, name: &str) -> Option<String> {
    match field(recording, name) {
        Some(Value::String(text)) => Some(text),
        _ => None,
    }
}

fn compare(source: &str) -> Result<usize, String> {
    let recordings = match serde_json::from_str::<Value>(source) {
        Ok(Value::Array(recordings)) => recordings,
        Ok(_) => return Err("the recordings are not a list".to_string()),
        Err(error) => return Err(format!("the recordings are not JSON: {error}")),
    };
    let mut differences = Vec::new();
    for (index, recording) in recordings.iter().enumerate() {
        let case = string(recording, "case").unwrap_or_else(|| format!("recording {index}"));
        let recorder = Recorder::new(field(recording, "contexts").unwrap_or(Value::Null));
        let (text, produced) = match (
            string(recording, "expression"),
            string(recording, "template"),
        ) {
            (Some(expression), None) => {
                let produced = bosun_expr::expression(&format!("toJSON({expression})"))
                    .and_then(|expr| bosun_expr::evaluate(&expr, &recorder))
                    .map(|value| match value {
                        Value::String(text) => text,
                        other => format!("{other:?}"),
                    });
                (expression, produced)
            }
            (None, Some(template)) => {
                let produced = bosun_expr::template(&template)
                    .and_then(|parsed| bosun_expr::render(&parsed, &recorder));
                (template, produced)
            }
            _ => {
                differences.push(format!(
                    "{case}: a recording has `expression` or `template`"
                ));
                continue;
            }
        };
        let recorded = match (string(recording, "output"), field(recording, "failed")) {
            (Some(output), None) => Some(output),
            (None, Some(Value::Boolean(true))) => None,
            _ => {
                differences.push(format!(
                    "{case}: a recording has `output` or `failed: true`"
                ));
                continue;
            }
        };
        match (recorded, produced) {
            (Some(recorded), Ok(produced)) if recorded == produced => {}
            (None, Err(_)) => {}
            (Some(recorded), Ok(produced)) => differences.push(format!(
                "{case} `{text}`: the hosted runner printed {recorded:?}, Bosun prints {produced:?}"
            )),
            (Some(recorded), Err(error)) => differences.push(format!(
                "{case} `{text}`: the hosted runner printed {recorded:?}, Bosun fails with {}: {}",
                error.code.as_str(),
                error.message
            )),
            (None, Ok(produced)) => differences.push(format!(
                "{case} `{text}`: the hosted runner failed, Bosun prints {produced:?}"
            )),
        }
    }
    if differences.is_empty() {
        Ok(recordings.len())
    } else {
        Err(differences.join("\n"))
    }
}

#[test]
fn bosun_prints_what_the_hosted_runners_printed() {
    if let Err(differences) = compare(&read(&repository().join(RECORDINGS))) {
        panic!("{RECORDINGS}: Bosun differs from the hosted runners:\n{differences}");
    }
}

#[test]
fn the_recordings_of_the_fixture_log_match_bosun() {
    assert_eq!(compare(&read(&repository().join(FIXTURE))), Ok(7));
}

#[test]
fn the_comparison_names_the_case_whose_recording_differs() {
    let fixture: serde_json::Value =
        serde_json::from_str(&read(&repository().join(FIXTURE))).expect("the fixture is JSON");
    assert_eq!(fixture[4]["case"], "values.json[1]");
    let mut differs = fixture.clone();
    differs[4]["output"] = json!("\"a\"");
    assert_eq!(
        compare(&differs.to_string()),
        Err(
            "values.json[1] `matrix.list[1]`: the hosted runner printed \"\\\"a\\\"\", Bosun \
             prints \"\\\"b\\\"\""
                .to_string()
        )
    );
    let mut succeeded = fixture.clone();
    assert_eq!(succeeded[5]["case"], "values.json[2]");
    let failed = succeeded[5]
        .as_object_mut()
        .expect("a recording is an object");
    failed.remove("failed");
    failed.insert("output".to_string(), json!("null"));
    let difference = compare(&succeeded.to_string()).expect_err("Bosun fails where it succeeded");
    assert!(
        difference.starts_with(
            "values.json[2] `fromJSON('x')`: the hosted runner printed \"null\", Bosun fails \
             with function_failed: "
        ),
        "{difference}"
    );
    let mut refused = fixture;
    refused[0]["failed"] = json!(true);
    assert_eq!(
        compare(&refused.to_string()),
        Err("texts.json[0]: a recording has `output` or `failed: true`".to_string())
    );
}
