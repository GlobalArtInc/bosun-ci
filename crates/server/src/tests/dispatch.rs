use std::fs;
use std::path::Path;

use axum::http::{Method, StatusCode};
use bosun_db::entity::run;
use bosun_db::workflows::NewWorkflow;
use bosun_types::api::Workflow;
use bosun_types::error::FieldError;
use sea_orm::EntityTrait;
use sea_orm::prelude::Uuid;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::access::{Answer, Credentials, Site, owner_credentials};
use super::queue::{LABELS, LEASE, assigned, claim, connected};

#[derive(Debug, Deserialize)]
struct Case {
    description: String,
    declared: Option<Map<String, Value>>,
    given: Map<String, Value>,
    #[serde(flatten)]
    expected: Expected,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Expected {
    Refused { fields: Vec<FieldError> },
    Dispatched { inputs: Value, event: Value },
}

fn cases() -> Vec<Case> {
    let file =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/fixtures/inputs/cases.json");
    let text =
        fs::read_to_string(&file).unwrap_or_else(|error| panic!("{}: {error}", file.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", file.display()))
}

fn flow(value: &Value) -> String {
    match value {
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(flow).collect();
            format!("[{}]", items.join(", "))
        }
        Value::Object(fields) => {
            let fields: Vec<String> = fields
                .iter()
                .map(|(name, value)| format!("{}: {}", Value::from(name.as_str()), flow(value)))
                .collect();
            format!("{{{}}}", fields.join(", "))
        }
        scalar => scalar.to_string(),
    }
}

fn source(declared: Option<&Map<String, Value>>) -> String {
    let mut source = "on:\n  workflow_dispatch:\n".to_owned();
    if let Some(declared) = declared {
        source.push_str("    inputs:\n");
        for (name, input) in declared {
            source.push_str(&format!("      {name}: {}\n", flow(input)));
        }
    }
    source.push_str("jobs:\n  build:\n    runs-on: [self-hosted]\n    steps:\n      - run: make\n");
    source
}

async fn create(site: &Site, name: &str, source: &str) -> Workflow {
    let project = site.project().await;
    bosun_db::workflows::create(
        site.db(),
        NewWorkflow {
            project_id: project.id,
            name,
            source,
            sha256: Sha256::digest(source.as_bytes()).to_vec(),
        },
    )
    .await
    .expect("a workflow")
}

async fn dispatch(
    site: &Site,
    workflow: &Workflow,
    credentials: &Credentials,
    inputs: &Map<String, Value>,
) -> Answer {
    site.send(
        Method::POST,
        &format!("/api/v1/workflows/{}/dispatch", workflow.id),
        credentials,
        Some(json!({ "ref": null, "inputs": inputs })),
    )
    .await
}

async fn stored(site: &Site, answer: &Answer) -> run::Model {
    let id: Uuid = answer.body["id"]
        .as_str()
        .and_then(|id| id.parse().ok())
        .unwrap_or_else(|| panic!("the id of the run: {answer:?}"));
    run::Entity::find_by_id(id)
        .one(site.db())
        .await
        .expect("a read")
        .expect("the run")
}

#[tokio::test]
async fn each_case_of_dispatch_inputs_is_refused_or_gives_its_inputs_and_payload() {
    let site = Site::new().await;
    let owner = owner_credentials(&site).await;
    let cases = cases();
    assert!(!cases.is_empty(), "contracts/fixtures/inputs holds no case");
    for (index, case) in cases.iter().enumerate() {
        let what = &case.description;
        let workflow = create(
            &site,
            &format!("inputs-{index}"),
            &source(case.declared.as_ref()),
        )
        .await;
        let answer = dispatch(&site, &workflow, &owner, &case.given).await;
        match &case.expected {
            Expected::Refused { fields } => {
                assert_eq!(
                    answer.status,
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "{what}: {answer:?}"
                );
                assert_eq!(answer.body["error"]["code"], "invalid_input", "{what}");
                assert_eq!(
                    answer.body["error"]["message"], "the dispatch is not valid",
                    "{what}"
                );
                assert_eq!(answer.body["error"]["fields"], json!(fields), "{what}");
            }
            Expected::Dispatched { inputs, event } => {
                assert_eq!(answer.status, StatusCode::CREATED, "{what}: {answer:?}");
                let run = stored(&site, &answer).await;
                assert_eq!(&run.inputs, inputs, "{what}");
                assert_eq!(&run.trigger["payload"]["inputs"], event, "{what}");
            }
        }
    }
}

#[tokio::test]
async fn expressions_and_the_agent_see_typed_inputs_and_github_event_inputs_their_text() {
    let site = Site::new().await;
    let owner = owner_credentials(&site).await;
    let workflow = create(
        &site,
        "typed",
        concat!(
            "run-name: \"${{ toJSON(inputs.flag) }} ${{ toJSON(github.event.inputs.flag) }} ",
            "${{ inputs.flag == true }} ${{ github.event.inputs.flag == 'true' }} ",
            "${{ toJSON(inputs.count) }} ${{ toJSON(github.event.inputs.count) }}\"\n",
            "on:\n",
            "  workflow_dispatch:\n",
            "    inputs:\n",
            "      flag:\n",
            "        type: boolean\n",
            "      count:\n",
            "        type: number\n",
            "jobs:\n",
            "  build:\n",
            "    runs-on: [self-hosted]\n",
            "    steps:\n",
            "      - run: make\n",
        ),
    )
    .await;
    let given = json!({ "flag": "true", "count": "3" });
    let given = given.as_object().expect("an object");
    let answer = dispatch(&site, &workflow, &owner, given).await;
    assert_eq!(answer.status, StatusCode::CREATED, "{answer:?}");
    assert_eq!(
        answer.body["display_name"],
        r#"true "true" true true 3 "3""#
    );

    let replica = Uuid::now_v7();
    let agent = connected(&site.world, "runner1-1", &LABELS, replica).await;
    let claimed = claim(site.db(), agent.id, replica, LEASE, &[])
        .await
        .expect("a claim of the dispatched job");
    let (_, documents) = assigned(&site.world, claimed).await;
    assert_eq!(
        Value::Object(documents.contexts.inputs),
        json!({ "count": 3, "flag": true })
    );
    assert_eq!(
        documents.contexts.github["event"]["inputs"],
        json!({ "count": "3", "flag": "true" })
    );
}
