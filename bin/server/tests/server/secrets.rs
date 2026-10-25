use std::collections::BTreeMap;
use std::time::Duration;

use bosun_proto::agent::v1::agent_message::Message as Report;
use bosun_proto::agent::v1::{Accept, Conclusion};
use bosun_proto::assignment::Documents;
use bosun_testkit::TestDatabase;
use bosun_testkit::agent::FakeAgent;
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};

use crate::api::{Api, text};
use crate::{KEK, Launch, WAIT, errors, owner, strings};

const VALUE: &str = "Zx9SecretOfTheDeployTokenQv3";

const OTHER: &str = "Wk7SecretOfTheOrganisationPm2";

const NEW_KEK: &str = "f0e1d2c3b4a5968778695a4b3c2d1e0f00112233445566778899aabbccddeeff";

const SOURCE: &str = "name: deploy
on: workflow_dispatch
env:
  FROM_WORKFLOW: ${{ secrets.DEPLOY_TOKEN }}
jobs:
  deploy:
    runs-on: [self-hosted]
    env:
      FROM_JOB: ${{ secrets.deploy_token }}
    steps:
      - run: echo ${{ secrets.DEPLOY_TOKEN }} ${{ secrets.ORGANISATION_TOKEN }}
";

const EVERY_SECRET: &str = "name: everything
on: workflow_dispatch
jobs:
  print:
    runs-on: [self-hosted]
    steps:
      - run: echo '${{ toJSON(secrets) }}'
";

async fn set(api: &Api, query: &str, name: &str, value: &str, status: u16) -> Value {
    let (answered, body) = api
        .post(
            &format!("/api/v1/secrets?{query}"),
            &json!({ "name": name, "value": value }),
        )
        .await;
    assert_eq!(answered, status, "{body}");
    body
}

async fn agent(api: &Api, name: &str) -> FakeAgent {
    let (status, token) = api
        .post(
            "/api/v1/registration-tokens",
            &json!({ "pool": "default", "reusable": false, "expires_at": null }),
        )
        .await;
    assert_eq!(status, 201, "{token}");
    FakeAgent::register(api.address, text(&token["token"]), name, &[]).await
}

async fn dispatched(api: &Api, workflow: &Value, name: &str) -> (Value, Documents) {
    let fake = agent(api, name).await;
    let mut connection = fake.connect(api.address).await;
    connection.idle();
    let (status, run) = api
        .post(
            &format!("/api/v1/workflows/{}/dispatch", text(&workflow["id"])),
            &json!({ "ref": null, "inputs": {} }),
        )
        .await;
    assert_eq!(status, 201, "{run}");
    let (assignment, documents) = connection.job().await;
    connection.send(Report::Accept(Accept {
        execution_id: assignment.execution_id.clone(),
    }));
    connection.finish_job(&assignment.execution_id, Conclusion::Success, &[]);
    let path = format!("/api/v1/runs/{}", text(&run["id"]));
    let deadline = Instant::now() + WAIT;
    loop {
        let (_, shown) = api.get(&path).await;
        if shown["status"] == "completed" {
            return (shown, documents);
        }
        assert!(
            Instant::now() < deadline,
            "the run did not complete: {shown}"
        );
        sleep(Duration::from_millis(50)).await;
    }
}

fn secrets(documents: &Documents) -> BTreeMap<String, String> {
    documents
        .contexts
        .secrets
        .iter()
        .map(|(name, value)| (name.clone(), text(value).to_owned()))
        .collect()
}

#[tokio::test]
async fn no_answer_log_line_or_table_holds_the_value_of_a_secret() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let project = api.project().await;
    let mut answers = vec![
        set(
            &api,
            "project=globalart/ops",
            "deploy_token",
            "a first value",
            201,
        )
        .await,
        set(&api, "project=globalart/ops", "DEPLOY_TOKEN", VALUE, 200).await,
        set(
            &api,
            "organisation=globalart",
            "ORGANISATION_TOKEN",
            OTHER,
            201,
        )
        .await,
    ];
    answers.push(api.get("/api/v1/secrets?project=globalart/ops").await.1);
    answers.push(api.get("/api/v1/secrets?organisation=globalart").await.1);
    let workflow = api.workflow(&project, "deploy", SOURCE).await;

    let (run, documents) = dispatched(&api, &workflow, "runner1-1").await;
    assert_eq!(
        secrets(&documents),
        BTreeMap::from([
            ("DEPLOY_TOKEN".to_owned(), VALUE.to_owned()),
            ("ORGANISATION_TOKEN".to_owned(), OTHER.to_owned()),
        ])
    );
    assert_eq!(documents.evaluated.env["FROM_JOB"], VALUE);
    assert_eq!(documents.evaluated.env["FROM_WORKFLOW"], VALUE);
    assert_eq!(run["conclusion"], "success", "{run}");
    let jobs = api
        .get(&format!(
            "/api/v1/runs/{}/attempts/1/jobs",
            text(&run["id"])
        ))
        .await
        .1;
    answers.push(run);
    answers.push(jobs);

    let mut dump = Vec::new();
    for table in strings(
        database.connection(),
        "SELECT table_name::text AS value FROM information_schema.tables \
         WHERE table_schema = 'bosun' ORDER BY 1",
    )
    .await
    {
        dump.extend(
            strings(
                database.connection(),
                &format!("SELECT t::text AS value FROM bosun.\"{table}\" t"),
            )
            .await,
        );
    }
    assert!(
        dump.iter().any(|row| row.contains("DEPLOY_TOKEN")),
        "the dump holds no row of the secret"
    );
    let lines = api.stop().await;
    assert!(
        lines
            .iter()
            .any(|line| line["message"] == "set a secret" && line["secret"] == "DEPLOY_TOKEN"),
        "{lines:#?}"
    );

    for value in [VALUE, OTHER] {
        let hex: String = value.bytes().map(|byte| format!("{byte:02x}")).collect();
        for (place, texts) in [
            (
                "an answer",
                answers.iter().map(Value::to_string).collect::<Vec<_>>(),
            ),
            ("a row", dump.clone()),
            (
                "a log line",
                lines.iter().map(Value::to_string).collect::<Vec<_>>(),
            ),
        ] {
            for form in [value, hex.as_str()] {
                assert!(
                    !texts.iter().any(|text| text.contains(form)),
                    "{place} holds `{form}`: {texts:#?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn after_secrets_rewrap_with_a_new_kek_every_secret_decrypts_with_the_new_kek_alone() {
    let database = TestDatabase::migrated().await;
    let token = owner(&database.url(), "roman").await;
    let api = Api::serve(Launch::serve(&database.url()), token.clone()).await;
    let project = api.project().await;
    set(&api, "project=globalart/ops", "DEPLOY_TOKEN", VALUE, 201).await;
    set(
        &api,
        "organisation=globalart",
        "ORGANISATION_TOKEN",
        OTHER,
        201,
    )
    .await;
    let workflow = api.workflow(&project, "everything", EVERY_SECRET).await;
    api.stop().await;

    let without = Launch::rewrap(&database.url()).env_remove("BOSUN_KEYS_KEK");
    let generated = without.data_dir().join("keys/kek");
    let nothing = without.finish().await;
    assert!(!nothing.status.success(), "{nothing:#?}");
    assert!(
        errors(&nothing.logs)
            .iter()
            .any(|error| error.contains(&format!(
                "the configuration names no KEK and {} does not exist",
                generated.display()
            ))),
        "{nothing:#?}"
    );

    let refused = Launch::rewrap(&database.url())
        .env("BOSUN_KEYS_KEK", NEW_KEK)
        .finish()
        .await;
    assert!(!refused.status.success(), "{refused:#?}");
    assert!(
        errors(&refused.logs).iter().any(|error| error.contains(
            "re-encrypted the data keys of 0 secrets, but not of 2: the secret `DEPLOY_TOKEN`"
        ) && error
            .contains("which the configuration of this server does not hold")),
        "{refused:#?}"
    );

    let launch = Launch::rewrap(&database.url()).env("BOSUN_KEYS_KEK", NEW_KEK);
    let old = launch.file("old-kek", &format!("{KEK}\n"));
    let rewrapped = launch
        .env("BOSUN_KEYS_PREVIOUS_KEK_FILES", &old.to_string_lossy())
        .finish()
        .await;
    assert!(rewrapped.status.success(), "{rewrapped:#?}");
    let ids = strings(
        database.connection(),
        "SELECT DISTINCT kek_id AS value FROM bosun.secret",
    )
    .await;
    let [id] = ids.as_slice() else {
        panic!("the secrets are under several KEKs: {ids:?}");
    };
    assert_eq!(
        rewrapped.stdout,
        format!("re-encrypted the data keys of 2 secrets under the KEK {id}\n")
    );
    let again = Launch::rewrap(&database.url())
        .env("BOSUN_KEYS_KEK", NEW_KEK)
        .finish()
        .await;
    assert_eq!(
        again.stdout,
        format!("re-encrypted the data keys of 0 secrets under the KEK {id}\n"),
        "{again:#?}"
    );

    let api = Api::serve(
        Launch::serve(&database.url()).env("BOSUN_KEYS_KEK", NEW_KEK),
        token,
    )
    .await;
    let (run, documents) = dispatched(&api, &workflow, "runner1-1").await;
    assert_eq!(run["conclusion"], "success", "{run}");
    assert_eq!(
        secrets(&documents),
        BTreeMap::from([
            ("DEPLOY_TOKEN".to_owned(), VALUE.to_owned()),
            ("ORGANISATION_TOKEN".to_owned(), OTHER.to_owned()),
        ])
    );
}
