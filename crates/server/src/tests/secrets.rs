use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use axum::http::{Method, StatusCode};
use bosun_db::roles::Subject;
use bosun_db::runs::Evaluated as Stored;
use bosun_db::secrets::Scope;
use bosun_types::id::JobId;
use bosun_types::status::{Conclusion, JobStatus};
use sea_orm::ConnectionTrait;
use sea_orm::prelude::Uuid;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::access::{Site, absent, forbidden, owner_credentials};
use super::queue::{LABELS, LEASE, assigned, claim, connected, parse, values};
use super::{World, keys, site};
use crate::{queue, secrets};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NameCase {
    description: String,
    name: String,
    stored: Option<String>,
}

fn name_cases() -> Vec<NameCase> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/fixtures/names/cases.json");
    let text =
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

async fn scopes(world: &World) -> (Scope, Scope) {
    let project = bosun_db::projects::find(world.db(), world.workflow.project_id)
        .await
        .expect("a project")
        .expect("the project of the world");
    (
        Scope {
            organisation_id: project.organisation_id,
            project_id: None,
        },
        Scope {
            organisation_id: project.organisation_id,
            project_id: Some(project.id),
        },
    )
}

async fn secret(world: &World, scope: Scope, name: &str, value: &str) {
    let sealed = keys()
        .seal(&secrets::binding(&scope, name), value.as_bytes())
        .expect("a sealed value");
    bosun_db::secrets::set(world.db(), scope, name, &sealed)
        .await
        .expect("a secret");
}

async fn variable(world: &World, scope: Scope, name: &str, value: &str) {
    bosun_db::variables::set(world.db(), scope, name, value)
        .await
        .expect("a variable");
}

fn strings(pairs: &[(&str, &str)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), json!(value)))
        .collect()
}

async fn evaluated(world: &World, job: JobId) -> Stored {
    let found = values(
        world.db(),
        &format!("SELECT evaluated::text AS value FROM bosun.job WHERE id = '{job}'"),
    )
    .await;
    serde_json::from_str(&found[0]).expect("the evaluated values")
}

#[tokio::test]
async fn names_of_secrets_and_variables_follow_the_cases_of_the_contract() {
    let site = Site::new().await;
    let owner = owner_credentials(&site).await;
    let long = "A".repeat(255);
    let too_long = "A".repeat(256);
    for collection in ["secrets", "variables"] {
        let path = format!("/api/v1/{collection}?project=globalart/keel");
        let mut cases = name_cases();
        cases.push(NameCase {
            description: "Bosun's limit of 255 characters".to_owned(),
            name: long.clone(),
            stored: Some(long.clone()),
        });
        cases.push(NameCase {
            description: "a name past Bosun's limit".to_owned(),
            name: too_long.clone(),
            stored: None,
        });
        for case in cases {
            let answer = site
                .send(
                    Method::POST,
                    &path,
                    &owner,
                    Some(json!({ "name": case.name, "value": "a value" })),
                )
                .await;
            let context = format!("{collection}: {}", case.description);
            match &case.stored {
                Some(stored) => {
                    assert_eq!(answer.status, StatusCode::CREATED, "{context}: {answer:?}");
                    assert_eq!(answer.body["name"], json!(stored), "{context}");
                }
                None => {
                    assert_eq!(
                        answer.status,
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "{context}: {answer:?}"
                    );
                    assert_eq!(answer.body["error"]["code"], "invalid_input", "{context}");
                    assert_eq!(
                        answer.body["error"]["fields"][0]["field"], "name",
                        "{context}"
                    );
                }
            }
        }
        let again = site
            .send(
                Method::POST,
                &path,
                &owner,
                Some(json!({ "name": "deploy_KEY_2", "value": "another value" })),
            )
            .await;
        assert_eq!(again.status, StatusCode::OK, "{again:?}");
        assert_eq!(again.body["name"], "DEPLOY_KEY_2");
        for value in ["", &"v".repeat(48 * 1024 + 1)] {
            let refused = site
                .send(
                    Method::POST,
                    &path,
                    &owner,
                    Some(json!({ "name": "EMPTY", "value": value })),
                )
                .await;
            assert_eq!(refused.status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(refused.body["error"]["fields"][0]["field"], "value");
        }
        let largest = site
            .send(
                Method::POST,
                &path,
                &owner,
                Some(json!({ "name": "LARGEST", "value": "v".repeat(48 * 1024) })),
            )
            .await;
        assert_eq!(largest.status, StatusCode::CREATED, "{collection}");
    }
}

#[tokio::test]
async fn a_developer_cannot_write_a_secret_a_maintainer_can_and_a_viewer_sees_names_alone() {
    let site = Site::new().await;
    let project = site.project().await;
    let organisation = project.organisation_id;
    let (viewer, viewing) = site.user("vera").await;
    site.bind("viewer", Subject::User(viewer), organisation, None)
        .await;
    let (developer, developing) = site.user("dana").await;
    site.bind("developer", Subject::User(developer), organisation, None)
        .await;
    let (maintainer, maintaining) = site.user("mia").await;
    site.bind(
        "maintainer",
        Subject::User(maintainer),
        organisation,
        Some(project.id),
    )
    .await;
    let secret = json!({ "name": "deploy_token", "value": "s3cr3t-of-the-deploy" });
    let in_project = "/api/v1/secrets?project=globalart/keel";

    let refused = site
        .send(Method::POST, in_project, &developing, Some(secret.clone()))
        .await;
    forbidden(&refused, "a developer's write of a secret");
    assert_eq!(
        refused.body["error"]["message"],
        "`dana` does not hold `secret.manage` on project `keel`"
    );
    let refused = site
        .send(
            Method::POST,
            "/api/v1/variables?project=globalart/keel",
            &developing,
            Some(json!({ "name": "REGION", "value": "eu" })),
        )
        .await;
    forbidden(&refused, "a developer's write of a variable");

    let written = site
        .send(Method::POST, in_project, &maintaining, Some(secret.clone()))
        .await;
    assert_eq!(written.status, StatusCode::CREATED, "{written:?}");
    assert_eq!(written.body["name"], "DEPLOY_TOKEN");
    assert_eq!(written.body["project_id"], json!(project.id));
    let elsewhere = site
        .send(
            Method::POST,
            "/api/v1/secrets?organisation=globalart",
            &maintaining,
            Some(secret.clone()),
        )
        .await;
    assert_eq!(elsewhere.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        elsewhere.body["error"]["fields"][0]["field"],
        "organisation"
    );

    let variable = site
        .send(
            Method::POST,
            "/api/v1/variables?project=globalart/keel",
            &maintaining,
            Some(json!({ "name": "region", "value": "eu-central-1" })),
        )
        .await;
    assert_eq!(variable.status, StatusCode::CREATED, "{variable:?}");

    for credentials in [&viewing, &developing, &maintaining] {
        let listed = site.get(in_project, credentials).await;
        assert_eq!(listed.status, StatusCode::OK, "{listed:?}");
        let [item] = listed.body["items"].as_array().expect("a page").as_slice() else {
            panic!("not one secret: {listed:?}");
        };
        assert_eq!(item["name"], "DEPLOY_TOKEN");
        assert!(item.get("value").is_none(), "{item}");
        assert!(!listed.body.to_string().contains("s3cr3t"), "{listed:?}");
        let variables = site
            .get("/api/v1/variables?project=globalart/keel", credentials)
            .await;
        assert_eq!(variables.body["items"][0]["value"], "eu-central-1");
    }
    assert_eq!(
        site.get("/api/v1/secrets?organisation=globalart", &viewing)
            .await
            .body["items"],
        json!([])
    );

    for (query, fields) in [
        ("", ["organisation", "project"]),
        (
            "?organisation=globalart&project=globalart/keel",
            ["organisation", "project"],
        ),
    ] {
        let answer = site
            .get(&format!("/api/v1/secrets{query}"), &maintaining)
            .await;
        assert_eq!(answer.status, StatusCode::UNPROCESSABLE_ENTITY, "{query}");
        let named: Vec<&str> = answer.body["error"]["fields"]
            .as_array()
            .expect("fields")
            .iter()
            .filter_map(|field| field["field"].as_str())
            .collect();
        assert_eq!(named, fields, "{query}");
    }

    let path = "/api/v1/secrets/deploy_token?project=globalart/keel";
    forbidden(
        &site.send(Method::DELETE, path, &developing, None).await,
        "a developer's delete of a secret",
    );
    let deleted = site.send(Method::DELETE, path, &maintaining, None).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{deleted:?}");
    absent(
        &site.send(Method::DELETE, path, &maintaining, None).await,
        "there is no secret `DEPLOY_TOKEN` in project `globalart/keel`",
    );
}

#[tokio::test]
async fn a_job_receives_the_secrets_it_reads_and_a_computed_name_receives_every_secret_in_scope() {
    let world = World::new().await;
    let replica = Uuid::now_v7();
    let (organisation, project) = scopes(&world).await;
    secret(&world, organisation, "A", "a of the organisation").await;
    secret(&world, project, "A", "a of the project").await;
    secret(&world, organisation, "B", "b of the organisation").await;
    secret(&world, project, "SETTINGS", r#"{"region": "eu"}"#).await;
    variable(&world, organisation, "WHERE", "organisation").await;
    variable(&world, project, "WHERE", "project").await;
    variable(&world, organisation, "ONLY", "of the organisation").await;
    let run = world
        .dispatch(&parse(
            "on: workflow_dispatch
jobs:
  named:
    runs-on: self-hosted
    if: vars.where == 'project'
    env:
      FROM_JOB: ${{ secrets.a }} in ${{ vars.ONLY }}
      REGION: ${{ fromJSON(secrets.SETTINGS).region }}
    steps:
      - run: echo ${{ secrets.A }}
  computed:
    runs-on: self-hosted
    env:
      PLAIN: ${{ vars.WHERE }}
    steps:
      - run: echo ${{ secrets[format('{0}', 'B')] }}
  skipped:
    runs-on: self-hosted
    if: vars.WHERE == 'organisation'
    steps:
      - run: echo never
",
        ))
        .await;
    let projection = world.project(run).await;
    assert_eq!(
        projection.job("skipped").1.conclusion,
        Some(Conclusion::Skipped)
    );
    let (named, _) = projection.job("named");
    assert_eq!(
        evaluated(&world, named).await,
        Stored {
            timeout_minutes: 360,
            env: BTreeMap::new(),
            shell: None,
            working_directory: None,
            deferred: true,
            concurrency: None,
        }
    );
    let (computed, _) = projection.job("computed");
    let stored = evaluated(&world, computed).await;
    assert_eq!(
        (stored.env, stored.deferred),
        ([("PLAIN".to_owned(), "project".to_owned())].into(), false)
    );

    let mut documents = BTreeMap::new();
    for number in 1..=2 {
        let agent = connected(&world, &format!("runner2-{number}"), &LABELS, replica).await;
        let claimed = claim(world.db(), agent.id, replica, LEASE, &[])
            .await
            .expect("a claim");
        let key = claimed.key.to_string();
        documents.insert(key, assigned(&world, claimed).await.1);
    }
    let named = &documents["named"];
    assert_eq!(
        named.contexts.secrets,
        strings(&[
            ("A", "a of the project"),
            ("GITHUB_TOKEN", ""),
            ("SETTINGS", r#"{"region": "eu"}"#)
        ])
    );
    assert_eq!(
        named.contexts.vars,
        strings(&[("ONLY", "of the organisation"), ("WHERE", "project")])
    );
    assert_eq!(
        named.evaluated.env,
        [
            (
                "FROM_JOB".to_owned(),
                "a of the project in of the organisation".to_owned()
            ),
            ("REGION".to_owned(), "eu".to_owned()),
        ]
        .into()
    );
    let computed = &documents["computed"];
    assert_eq!(
        computed.contexts.secrets,
        strings(&[
            ("A", "a of the project"),
            ("B", "b of the organisation"),
            ("GITHUB_TOKEN", ""),
            ("SETTINGS", r#"{"region": "eu"}"#),
        ])
    );
    assert_eq!(
        computed.evaluated.env,
        [("PLAIN".to_owned(), "project".to_owned())].into()
    );
}

pub(super) async fn unreadable_secret_notices() -> Vec<String> {
    let world = World::new().await;
    let replica = Uuid::now_v7();
    let (_, project) = scopes(&world).await;
    secret(&world, project, "A", "the value of a").await;
    secret(&world, project, "B", "the value of b").await;
    world
        .db()
        .execute_unprepared(
            "UPDATE bosun.secret SET (ciphertext, data_key, kek_id) = \
             (SELECT ciphertext, data_key, kek_id FROM bosun.secret WHERE name = 'A') \
             WHERE name = 'B'",
        )
        .await
        .expect("a copied ciphertext");
    let run = world
        .dispatch(&parse(
            "on: workflow_dispatch
jobs:
  build:
    runs-on: self-hosted
    steps:
      - run: echo ${{ secrets.B }}
",
        ))
        .await;
    let agent = connected(&world, "runner1-2", &LABELS, replica).await;
    let claimed = claim(world.db(), agent.id, replica, LEASE, &[])
        .await
        .expect("a claim");
    assert!(
        queue::assignment(
            world.db(),
            claimed,
            &site(),
            world.tokens(),
            world.actions()
        )
        .await
        .expect("a failed assignment")
        .is_none(),
        "a job whose secret does not decrypt was offered"
    );
    let projection = world.project(run).await;
    let job = projection.job("build").1;
    assert_eq!(
        (job.status, job.conclusion),
        (JobStatus::Completed, Some(Conclusion::Failure))
    );
    let notices = world.run(run).await.notices;
    let [notice] = notices.as_slice() else {
        panic!("not one notice: {notices:#?}");
    };
    assert_eq!(
        notice.diagnostic.message,
        format!(
            "job `build` reads the secret `B` of project `globalart/keel`, which cannot be \
             decrypted: it does not decrypt under the KEK `{}`, so its ciphertext or its data key \
             belongs to another secret or was changed",
            keys().current()
        )
    );
    notices
        .into_iter()
        .map(|notice| notice.diagnostic.code)
        .collect()
}

#[tokio::test]
async fn a_ciphertext_copied_onto_another_secret_fails_to_decrypt_and_fails_the_job() {
    assert_eq!(unreadable_secret_notices().await, ["secret_unreadable"]);
}

#[tokio::test]
async fn the_answers_to_the_requests_of_the_fixtures_have_the_shapes_of_the_fixtures() {
    let site = Site::new().await;
    let owner = owner_credentials(&site).await;
    let project = site.project().await;
    for (path, request, expected, owned_by_project) in [
        (
            "/api/v1/secrets?project=globalart/keel",
            "set-secret-request.json",
            "secret.json",
            true,
        ),
        (
            "/api/v1/variables?organisation=globalart",
            "set-variable-request.json",
            "variable.json",
            false,
        ),
    ] {
        let answer = site
            .send(Method::POST, path, &owner, Some(fixture(request)))
            .await;
        assert_eq!(answer.status, StatusCode::CREATED, "{answer:?}");
        let answered = &answer.body;
        let mut fixed = fixture(expected);
        for (key, value) in [
            ("organisation_id", json!(project.organisation_id)),
            (
                "project_id",
                if owned_by_project {
                    json!(project.id)
                } else {
                    Value::Null
                },
            ),
        ] {
            assert_eq!(answered[key], value, "{expected}");
            fixed[key] = value;
        }
        for key in ["id", "created_at", "updated_at"] {
            fixed[key] = answered[key].clone();
        }
        assert_eq!(answered, &fixed, "{expected}");
    }
}

fn fixture(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../contracts/fixtures/api")
        .join(name);
    let text =
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).expect("a fixture")
}
