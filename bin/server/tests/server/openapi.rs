use std::fs;
use std::path::Path;

use bosun_testkit::TestDatabase;
use reqwest::Method;
use serde_json::{Value, json};
use tokio::process::Command;
use uuid::Uuid;

use crate::api::{Api, send, text};

const SCRIPT: &str = "hack/openapi.sh";

fn committed() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/openapi.json");
    fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}; generate it with {SCRIPT}", path.display()))
}

#[tokio::test]
async fn the_committed_document_is_the_one_the_handlers_describe() {
    let output = Command::new(env!("CARGO_BIN_EXE_bosun-server"))
        .arg("openapi")
        .output()
        .await
        .expect("bosun-server openapi runs");
    assert!(
        output.status.success(),
        "bosun-server openapi failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let generated = String::from_utf8(output.stdout).expect("UTF-8 on standard output");
    let committed = committed();
    let differs = generated
        .lines()
        .zip(committed.lines())
        .position(|(generated, committed)| generated != committed)
        .map_or_else(
            || "in its length".to_owned(),
            |index| format!("from line {}", index + 1),
        );
    assert!(
        generated == committed,
        "contracts/openapi.json differs {differs} from the document the handlers describe; \
         regenerate it with {SCRIPT}"
    );
}

#[tokio::test]
async fn every_operation_reaches_a_handler_with_the_owners_token() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let project = api.project().await;
    let document: Value = serde_json::from_str(&committed()).expect("the document is JSON");
    let paths = document["paths"]
        .as_object()
        .expect("the document has paths");
    assert!(!paths.is_empty(), "the document has no operations");

    for (template, item) in paths {
        let mut concrete = vec![template.clone()];
        for (name, values) in [
            ("{organisation}", vec!["globalart".to_owned()]),
            (
                "{project}",
                vec![
                    text(&project["id"]).to_owned(),
                    "globalart%2Fops".to_owned(),
                ],
            ),
            ("{workflow}", vec![Uuid::now_v7().to_string()]),
            ("{run}", vec![Uuid::now_v7().to_string()]),
            ("{attempt}", vec!["1".to_owned()]),
            ("{job}", vec![Uuid::now_v7().to_string()]),
            ("{execution}", vec![Uuid::now_v7().to_string()]),
            ("{step}", vec![Uuid::now_v7().to_string()]),
        ] {
            concrete = concrete
                .iter()
                .flat_map(|path| values.iter().map(move |value| path.replace(name, value)))
                .collect();
        }
        for path in &concrete {
            assert!(
                !path.contains('{'),
                "{template} has a parameter the test cannot fill"
            );
            for (method, operation) in item.as_object().expect("a path item is an object") {
                let method: Method = method.to_uppercase().parse().expect("an HTTP method");
                let mut request = api.request(method.clone(), path);
                if operation.get("requestBody").is_some() {
                    request = request.json(&json!({}));
                }
                let (status, body) = send(request).await;
                assert!(
                    status != 401 && status != 403,
                    "{method} {path} refused the owner's token: {body}"
                );
                assert_ne!(
                    body["error"]["message"],
                    format!("there is no {method} {path}"),
                    "{method} {path} is in the document but reaches no handler"
                );
            }
        }
    }
}
