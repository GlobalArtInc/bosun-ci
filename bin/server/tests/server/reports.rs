use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use bosun_testkit::TestDatabase;
use bosun_testkit::forge::{FakeForge, RecordedRequest, Sender};
use serde_json::{Value, json};

use crate::api::{Api, text};
use crate::hooks::{ACCOUNT, PERMISSIONS, delivered, processed};
use crate::{WAIT, message};

const REPOSITORY: &str = "GlobalArtInc/ops";

const ROMAN: Sender<'static> = Sender {
    login: "roman",
    id: 7001,
};

const PLAIN_JOBS: usize = 10;

fn workflow() -> String {
    let mut source = String::from(concat!(
        "name: ci\n",
        "on: push\n",
        "jobs:\n",
        "  build:\n",
        "    name: Build\n",
        "    runs-on: [self-hosted]\n",
        "    strategy:\n",
        "      matrix:\n",
        "        os: [linux]\n",
        "        node: [18]\n",
        "    steps:\n",
        "      - run: make\n",
        "  lint:\n",
        "    runs-on: [self-hosted]\n",
        "    steps:\n",
        "      - run: make lint\n",
    ));
    for number in 1..=PLAIN_JOBS {
        source.push_str(&format!(
            "  job{number:02}:\n    runs-on: [self-hosted]\n    steps:\n      - run: make\n"
        ));
    }
    source
}

async fn reported(database: &TestDatabase, forge: &FakeForge, completed: usize) {
    let deadline = Instant::now() + WAIT;
    loop {
        let pending = bosun_db::outbox::pending(database.connection())
            .await
            .expect("the outbox backlog");
        let done = forge
            .check_runs()
            .iter()
            .filter(|check_run| check_run.status == "completed")
            .count();
        if pending == 0 && done == completed {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{pending} outbox rows were not delivered within {WAIT:?}: {:#?}",
            forge.check_runs()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn reports(requests: &[RecordedRequest]) -> Vec<&RecordedRequest> {
    requests
        .iter()
        .filter(|request| request.path.contains("/check-runs"))
        .collect()
}

#[tokio::test]
async fn two_replicas_deliver_each_report_once_and_name_check_runs_as_the_target_does() {
    let database = TestDatabase::migrated().await;
    let forge = FakeForge::start().await;
    let installation = forge.install(ACCOUNT, PERMISSIONS);
    forge.create_repository(installation, REPOSITORY);
    let first = forge.commit(REPOSITORY, "main", [("README.md", "# ops\n")]);
    let left = Api::with_forge(&database, &forge).await;
    let right = left.replica(&database, &forge).await;
    delivered(
        &left,
        &forge,
        "installation",
        &forge.installation_event(installation, "created"),
    )
    .await;
    processed(&database).await;
    let (status, project) = left
        .post(
            "/api/v1/projects",
            &json!({
                "organisation": "globalartinc",
                "slug": "ops",
                "name": "Operations",
                "repository": REPOSITORY
            }),
        )
        .await;
    assert_eq!(status, 201, "{project}");
    forge.delay(Duration::from_millis(50));

    let after = forge.commit(
        REPOSITORY,
        "main",
        [(".bosun/workflows/ci.yml", workflow().as_str())],
    );
    let push = forge.push_event(REPOSITORY, "refs/heads/main", &first, &after, ROMAN);
    delivered(&right, &forge, "push", &push).await;
    processed(&database).await;
    reported(&database, &forge, 0).await;
    let jobs = PLAIN_JOBS + 2;
    assert_eq!(forge.check_runs().len(), jobs, "{:#?}", forge.check_runs());

    let (status, runs) = left.get("/api/v1/runs").await;
    assert_eq!(status, 200, "{runs}");
    let run = text(&runs["items"][0]["id"]).to_owned();
    let (status, accepted) = left
        .post(
            &format!("/api/v1/runs/{run}/commands"),
            &json!({
                "type": "run.cancel",
                "target": { "attempt": null, "job": null, "step": null },
                "reason": null
            }),
        )
        .await;
    assert_eq!(status, 201, "{accepted}");
    reported(&database, &forge, jobs).await;

    let mut expected: BTreeSet<String> = (1..=PLAIN_JOBS)
        .map(|number| format!("job{number:02}"))
        .collect();
    expected.insert("Build (linux, 18)".to_owned());
    expected.insert("lint".to_owned());
    let check_runs = forge.check_runs();
    let names: BTreeSet<String> = check_runs
        .iter()
        .map(|check_run| check_run.name.clone())
        .collect();
    assert_eq!(names, expected);
    assert!(
        check_runs.iter().all(|check_run| {
            check_run.head_sha == after
                && check_run.status == "completed"
                && check_run.conclusion.as_deref() == Some("cancelled")
        }),
        "{check_runs:#?}"
    );

    let requests = forge.requests();
    let reports = reports(&requests);
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for request in &reports {
        assert!(
            (200..300).contains(&request.status),
            "a report failed: {request:?}"
        );
        let shown = format!(
            "{} {} {}",
            request.method,
            request.path,
            request.body.clone().unwrap_or(Value::Null)
        );
        *seen.entry(shown).or_default() += 1;
    }
    assert!(
        seen.values().all(|times| *times == 1),
        "a report was delivered twice: {seen:#?}"
    );
    let created: Vec<&str> = reports
        .iter()
        .filter(|request| request.method == "POST")
        .filter_map(|request| request.body.as_ref())
        .filter_map(|body| body["name"].as_str())
        .collect();
    assert_eq!(created.len(), jobs, "{created:?}");
    let updated: BTreeSet<&str> = reports
        .iter()
        .filter(|request| request.method == "PATCH")
        .map(|request| request.path.as_str())
        .collect();
    assert_eq!(
        updated.len(),
        reports.len() - jobs,
        "a check run was updated twice"
    );

    let mut delivered_by = Vec::new();
    for replica in [left, right] {
        let lines = replica.stop().await;
        delivered_by.push(
            lines
                .iter()
                .filter(|line| message(line) == "delivered an outbox row")
                .count(),
        );
    }
    assert_eq!(
        delivered_by.iter().sum::<usize>(),
        reports.len(),
        "{delivered_by:?}"
    );
    assert!(
        delivered_by.iter().all(|delivered| *delivered > 0),
        "a replica delivered nothing: {delivered_by:?}"
    );
}
