use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::time::Duration;

use bosun_proto::agent::v1::agent_message::Message as Report;
use bosun_proto::agent::v1::{Accept, Conclusion, ControlKind, Hello, JobFinished, Reject};
use bosun_testkit::TestDatabase;
use bosun_testkit::agent::FakeAgent;
use bosun_types::ir::{Action, JobKind, Shell, Template};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::time::{Instant, sleep, timeout};

use crate::api::{fixture, text};
use crate::{Launch, Server, WAIT, client, message, owner, strings};

const SERVER: &str = "[server]\nlisten = \"127.0.0.1:0\"\n\n[database]\nmax_connections = 4\n";

struct Cluster {
    database: TestDatabase,
    owner: String,
    servers: Vec<Server>,
    addresses: Vec<SocketAddr>,
}

impl Cluster {
    async fn start(replicas: usize, settings: &str) -> Cluster {
        let database = TestDatabase::migrated().await;
        let owner = owner(&database.url(), "roman").await;
        let mut servers = Vec::new();
        let mut addresses = Vec::new();
        for _ in 0..replicas {
            let mut server = Launch::serve(&database.url())
                .config(&format!("{SERVER}{settings}"))
                .spawn();
            addresses.push(server.listening().await);
            servers.push(server);
        }
        Cluster {
            database,
            owner,
            servers,
            addresses,
        }
    }

    async fn post(&self, path: &str, body: &Value) -> Value {
        let response = client()
            .post(format!("http://{}{path}", self.addresses[0]))
            .bearer_auth(&self.owner)
            .json(body)
            .send()
            .await
            .unwrap_or_else(|error| panic!("POST {path}: {error}"));
        let status = response.status().as_u16();
        let body: Value = response.json().await.expect("a JSON body");
        assert_eq!(status, 201, "POST {path}: {body}");
        body
    }

    async fn agent(&self, name: &str, labels: &[&str]) -> FakeAgent {
        let token = self
            .post(
                "/api/v1/registration-tokens",
                &json!({ "pool": "default", "reusable": false, "expires_at": null }),
            )
            .await;
        FakeAgent::register(self.addresses[0], text(&token["token"]), name, labels).await
    }

    async fn workflow(&self, source: &str) -> String {
        self.post(
            "/api/v1/organisations",
            &json!({ "slug": "globalart", "name": "GlobalArt" }),
        )
        .await;
        let project = self
            .post(
                "/api/v1/projects",
                &json!({ "organisation": "globalart", "slug": "ops", "name": "Operations" }),
            )
            .await;
        let workflow = self
            .post(
                &format!("/api/v1/projects/{}/workflows", text(&project["id"])),
                &json!({ "name": "ci", "source": source }),
            )
            .await;
        text(&workflow["id"]).to_owned()
    }

    async fn dispatch(&self, workflow: &str) -> String {
        let run = self
            .post(
                &format!("/api/v1/workflows/{workflow}/dispatch"),
                &json!({ "ref": null, "inputs": {} }),
            )
            .await;
        text(&run["id"]).to_owned()
    }

    async fn values(&self, sql: &str) -> Vec<String> {
        strings(self.database.connection(), sql).await
    }

    async fn until(&self, sql: &str, expected: &[&str]) {
        let deadline = Instant::now() + WAIT;
        loop {
            let found = self.values(sql).await;
            if found == expected {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{sql} gave {found:?} and not {expected:?} within {WAIT:?}"
            );
            sleep(Duration::from_millis(50)).await;
        }
    }

    async fn job(&self, run: &str, key: &str) -> String {
        self.values(&format!(
            "SELECT j.id::text AS value FROM bosun.job j \
             JOIN bosun.attempt a ON a.id = j.attempt_id \
             WHERE a.run_id = '{run}' AND j.key = '{key}'"
        ))
        .await
        .pop()
        .unwrap_or_else(|| panic!("no job `{key}` in run {run}"))
    }
}

fn many_jobs(count: usize) -> String {
    let mut source = "name: ci\non: workflow_dispatch\njobs:\n".to_owned();
    for index in 0..count {
        source.push_str(&format!(
            "  j{index}:\n    runs-on: [self-hosted]\n    steps:\n      - run: \"true\"\n"
        ));
    }
    source
}

#[tokio::test]
async fn fifty_fake_agents_claiming_concurrently_from_three_replicas_never_receive_the_same_job() {
    const RUNS: usize = 3;
    const JOBS: usize = 40;
    let cluster = Cluster::start(3, "").await;
    let workflow = cluster.workflow(&many_jobs(JOBS)).await;
    let (received, mut assignments) = mpsc::unbounded_channel();
    let mut tasks = Vec::new();
    for number in 0..50 {
        let replica = number % cluster.addresses.len();
        let fake = cluster.agent(&format!("runner{number}"), &[]).await;
        let mut connection = fake.connect(cluster.addresses[replica]).await;
        let received = received.clone();
        tasks.push(tokio::spawn(async move {
            connection.idle();
            loop {
                let assignment = connection.assignment().await;
                let _ = received.send((replica, fake.id(), assignment.job_id.clone()));
                connection.send(Report::Accept(Accept {
                    execution_id: assignment.execution_id.clone(),
                }));
                connection.send(Report::JobFinished(JobFinished {
                    execution_id: assignment.execution_id,
                    conclusion: Conclusion::Success.into(),
                    outputs: Default::default(),
                }));
                connection.idle();
            }
        }));
    }
    let mut runs = Vec::new();
    for _ in 0..RUNS {
        runs.push(cluster.dispatch(&workflow).await);
    }

    let mut taken = Vec::new();
    let deadline = Instant::now() + WAIT;
    while taken.len() < RUNS * JOBS {
        match tokio::time::timeout_at(deadline, assignments.recv()).await {
            Ok(Some(assignment)) => taken.push(assignment),
            Ok(None) => panic!("every fake agent stopped"),
            Err(_) => panic!(
                "the agents received {} assignments of {} within {WAIT:?}",
                taken.len(),
                RUNS * JOBS
            ),
        }
    }
    cluster
        .until(
            "SELECT concat_ws(' ', status, conclusion, count(*)) AS value FROM bosun.run \
             GROUP BY status, conclusion",
            &["completed success 3"],
        )
        .await;
    for task in &tasks {
        task.abort();
    }

    let jobs: BTreeSet<&str> = taken.iter().map(|(_, _, job)| job.as_str()).collect();
    assert_eq!(
        jobs.len(),
        taken.len(),
        "a job reached more than one agent: {taken:?}"
    );
    let replicas: BTreeSet<usize> = taken.iter().map(|(replica, _, _)| *replica).collect();
    assert_eq!(replicas.len(), 3, "not every replica handed out jobs");
    assert_eq!(
        cluster
            .values(
                "SELECT concat_ws(' ', count(*), count(DISTINCT job_id), \
                 count(*) FILTER (WHERE status = 'completed' AND conclusion = 'success')) \
                 AS value FROM bosun.execution"
            )
            .await,
        ["120 120 120"]
    );
    let mut per_agent: BTreeMap<String, usize> = BTreeMap::new();
    for (_, agent, _) in &taken {
        *per_agent.entry(agent.to_string()).or_default() += 1;
    }
    assert!(per_agent.len() > 1, "one agent took every job");
}

#[tokio::test]
async fn an_assignment_that_is_not_confirmed_returns_the_job_to_the_queue_after_the_lease() {
    let mut cluster = Cluster::start(1, "\n[agents]\nassignment_lease = \"1s\"\n").await;
    let workflow = cluster
        .workflow("name: ci\non: workflow_dispatch\njobs:\n  build:\n    runs-on: [self-hosted]\n    steps:\n      - run: make\n")
        .await;
    let fake = cluster.agent("runner1-1", &[]).await;
    let mut connection = fake.connect(cluster.addresses[0]).await;
    connection.idle();
    let run = cluster.dispatch(&workflow).await;
    let build = cluster.job(&run, "build").await;
    let first = connection.assignment().await;
    assert_eq!(first.job_id, build);
    let assigned_at = Instant::now();

    let control = connection.control().await;
    assert!(
        assigned_at.elapsed() >= Duration::from_secs(1),
        "the lease ended after {:?}",
        assigned_at.elapsed()
    );
    assert_eq!(control.kind(), ControlKind::Cancel);
    assert_eq!(control.execution_id, first.execution_id);
    connection.acknowledge(&control, false);
    assert_eq!(
        cluster
            .values(&format!(
                "SELECT concat_ws(' ', status, conclusion, reason) AS value \
                 FROM bosun.execution WHERE id = '{}' UNION ALL \
                 SELECT status FROM bosun.job WHERE id = '{build}'",
                first.execution_id
            ))
            .await,
        ["completed cancelled lease_expired", "queued"]
    );
    assert_eq!(
        cluster
            .values(&format!(
                "SELECT data::text AS value FROM bosun.run_event WHERE job_id = '{build}' \
                 AND kind = 'job.status' ORDER BY seq"
            ))
            .await
            .into_iter()
            .map(|data| {
                let data: Value = serde_json::from_str(&data).expect("JSON data");
                format!("{} {} {}", data["from"], data["to"], data["reason"])
            })
            .collect::<Vec<_>>(),
        [
            "\"pending\" \"queued\" null",
            "\"queued\" \"assigned\" null",
            "\"assigned\" \"queued\" \"lease_expired\"",
        ]
    );
    cluster.servers[0]
        .expect("that the job went back to the queue", |line| {
            message(line)
                == "a job went back to the queue because its agent did not accept it within the lease"
                && line["execution_id"] == first.execution_id.as_str()
        })
        .await;

    connection.idle();
    let second = connection.assignment().await;
    assert_eq!(second.job_id, build);
    assert_ne!(second.execution_id, first.execution_id);
    connection.send(Report::Accept(Accept {
        execution_id: second.execution_id.clone(),
    }));
    cluster
        .until(
            &format!("SELECT status AS value FROM bosun.job WHERE id = '{build}'"),
            &["in_progress"],
        )
        .await;
}

#[tokio::test]
async fn a_job_whose_labels_no_agent_offers_is_cancelled_with_no_agent_after_max_queue_time() {
    let mut cluster = Cluster::start(1, "\n[limits]\nmax_queue_time = \"2s\"\n").await;
    let workflow = cluster
        .workflow("name: ci\non: workflow_dispatch\njobs:\n  gpu:\n    runs-on: [self-hosted, gpu]\n    steps:\n      - run: train\n  after:\n    needs: gpu\n    runs-on: [self-hosted]\n    steps:\n      - run: report\n")
        .await;
    let fake = cluster.agent("runner1-1", &["deploy"]).await;
    let connection = fake.connect(cluster.addresses[0]).await;
    connection.idle();
    let run = cluster.dispatch(&workflow).await;
    let gpu = cluster.job(&run, "gpu").await;

    cluster
        .until(
            &format!(
                "SELECT concat_ws(' ', status, conclusion) AS value FROM bosun.run WHERE id = '{run}'"
            ),
            &["completed cancelled"],
        )
        .await;
    assert_eq!(
        cluster
            .values(&format!(
                "SELECT concat_ws(' ', j.key, j.status, j.conclusion, j.conclusion_source) \
                 AS value FROM bosun.job j JOIN bosun.attempt a ON a.id = j.attempt_id \
                 WHERE a.run_id = '{run}' ORDER BY j.key"
            ))
            .await,
        [
            "after completed skipped natural",
            "gpu completed cancelled natural"
        ]
    );
    let cancelled: Value = serde_json::from_str(
        &cluster
            .values(&format!(
                "SELECT json_build_object('actor', actor, 'data', data)::text AS value \
                 FROM bosun.run_event WHERE job_id = '{gpu}' AND kind = 'job.status' \
                 ORDER BY seq DESC LIMIT 1"
            ))
            .await[0],
    )
    .expect("an event as JSON");
    assert_eq!(
        cancelled,
        json!({
            "actor": { "kind": "system" },
            "data": {
                "from": "queued",
                "to": "completed",
                "waiting_reason": null,
                "conclusion": "cancelled",
                "conclusion_source": "natural",
                "reason": "no_agent",
            },
        })
    );
    assert_eq!(
        cluster
            .values("SELECT count(*)::text AS value FROM bosun.execution")
            .await,
        ["0"]
    );
    cluster.servers[0]
        .expect("that the job was cancelled", |line| {
            message(line) == "a job was cancelled because no agent took it within max_queue_time"
                && line["job_id"] == gpu.as_str()
        })
        .await;
    drop(connection);
}

#[tokio::test]
async fn agents_receive_only_jobs_their_labels_satisfy_and_a_rejected_job_goes_to_another() {
    let cluster = Cluster::start(2, "").await;
    let workflow = cluster
        .workflow("name: ci\non: workflow_dispatch\njobs:\n  deploy:\n    runs-on: [self-hosted, deploy]\n    steps:\n      - run: ship\n  build:\n    runs-on: [self-hosted, linux]\n    steps:\n      - run: make\n")
        .await;
    let plain = cluster.agent("runner1-1", &[]).await;
    let deployer = cluster.agent("runner2-1", &["Deploy"]).await;
    let mut on_a = plain.connect(cluster.addresses[0]).await;
    let mut on_b = deployer.connect(cluster.addresses[1]).await;
    on_a.idle();
    on_b.idle();
    let run = cluster.dispatch(&workflow).await;
    let deploy = cluster.job(&run, "deploy").await;
    let build = cluster.job(&run, "build").await;

    let to_plain = on_a.assignment().await;
    let to_deployer = on_b.assignment().await;
    assert_eq!(to_plain.job_id, build);
    assert_eq!(to_deployer.job_id, deploy);

    on_a.send(Report::Reject(Reject {
        execution_id: to_plain.execution_id.clone(),
        reason: "the agent is stopping".to_owned(),
    }));
    on_a.idle();
    cluster
        .until(
            &format!(
                "SELECT concat_ws(' ', status, conclusion, reason) AS value FROM bosun.execution \
                 WHERE id = '{}'",
                to_plain.execution_id
            ),
            &["completed cancelled rejected_by_agent"],
        )
        .await;
    assert!(
        timeout(Duration::from_millis(500), on_a.assignment())
            .await
            .is_err(),
        "the agent was offered again the job it rejected"
    );

    on_b.send(Report::Accept(Accept {
        execution_id: to_deployer.execution_id.clone(),
    }));
    on_b.send(Report::JobFinished(JobFinished {
        execution_id: to_deployer.execution_id.clone(),
        conclusion: Conclusion::Success.into(),
        outputs: Default::default(),
    }));
    on_b.idle();
    let again = on_b.assignment().await;
    assert_eq!(again.job_id, build);
    assert_eq!(
        cluster
            .values(&format!(
                "SELECT concat_ws(' ', a.name, e.number, e.status) AS value \
                 FROM bosun.execution e JOIN bosun.agent a ON a.id = e.agent_id \
                 WHERE e.job_id = '{build}' ORDER BY e.number"
            ))
            .await,
        ["runner1-1 1 completed", "runner2-1 2 assigned"]
    );
}

#[tokio::test]
async fn an_assignment_carries_the_slugs_the_job_of_the_ir_and_the_values_the_server_evaluated() {
    let cluster = Cluster::start(1, "").await;
    let workflow = cluster.workflow(&fixture("workflows/two-jobs.yml")).await;
    let fake = cluster.agent("runner1-1", &[]).await;
    let mut connection = fake.connect(cluster.addresses[0]).await;
    connection.idle();
    let run = cluster.dispatch(&workflow).await;

    let (build, documents) = connection.job().await;
    let ids = cluster
        .values(&format!(
            "SELECT concat_ws(' ', a.run_id, a.id, j.id, e.id) AS value FROM bosun.execution e \
             JOIN bosun.job j ON j.id = e.job_id JOIN bosun.attempt a ON a.id = j.attempt_id \
             WHERE a.run_id = '{run}' AND j.key = 'build'"
        ))
        .await;
    assert_eq!(
        ids,
        [format!(
            "{} {} {} {}",
            build.run_id, build.attempt_id, build.job_id, build.execution_id
        )]
    );
    assert_eq!(
        (build.organisation.as_str(), build.project.as_str()),
        ("globalart", "ops")
    );
    assert_eq!(
        (build.job_key.as_str(), build.job_name.as_str()),
        ("build", "Build")
    );
    assert_eq!(
        documents.evaluated.env,
        [("CARGO_TERM_COLOR".to_owned(), "always".to_owned())].into()
    );
    assert_eq!(
        (
            documents.evaluated.timeout_minutes,
            documents.evaluated.continue_on_error,
            &documents.evaluated.shell,
            &documents.evaluated.working_directory
        ),
        (360, false, &None, &None)
    );
    assert_eq!(documents.job.key.as_str(), "build");
    let JobKind::Steps(steps) = &documents.job.kind;
    assert_eq!(steps.len(), 2);
    assert_eq!(
        (
            steps[0].key.as_ref().map(|key| key.to_string()),
            &steps[0].shell,
            &steps[0].action,
            &steps[0].env
        ),
        (
            Some("compile".to_owned()),
            &Some(Shell::Bash),
            &Action::Run {
                script: Template::text("cargo build --workspace\n")
            },
            &[("RUSTFLAGS".to_owned(), Template::text("-D warnings"))].into()
        )
    );
    assert_eq!(steps[1].working_directory, Some(Template::text("crates")));
    assert_eq!(documents.contexts.github["job"], "build");
    assert_eq!(documents.contexts.github["event_name"], "workflow_dispatch");
    assert_eq!(documents.contexts.github["workspace"], "");
    assert_eq!(documents.contexts.bosun["run_id"], json!(build.run_id));
    assert_eq!(documents.contexts.bosun["project"], "globalart/ops");
    assert_eq!(
        (
            &documents.contexts.needs,
            &documents.contexts.inputs,
            &documents.contexts.vars,
            &documents.contexts.secrets,
            &documents.contexts.strategy,
            &documents.contexts.matrix
        ),
        (
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &json!({ "fail-fast": true, "job-index": 0, "job-total": 1, "max-parallel": 1 }),
            &Value::Null
        )
    );

    connection.send(Report::Accept(Accept {
        execution_id: build.execution_id.clone(),
    }));
    connection.send(Report::JobFinished(JobFinished {
        execution_id: build.execution_id,
        conclusion: Conclusion::Success.into(),
        outputs: Default::default(),
    }));
    connection.idle();
    let (test, documents) = connection.job().await;
    assert_eq!(test.job_key, "test");
    let JobKind::Steps(steps) = &documents.job.kind;
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].shell, Some(Shell::Custom("perl {0}".to_owned())));
    assert_eq!(
        documents.contexts.needs,
        json!({ "build": { "result": "success", "outputs": {} } })
            .as_object()
            .cloned()
            .expect("an object")
    );
}

#[tokio::test]
async fn an_agent_that_reads_another_revision_of_the_assignment_is_offered_no_job() {
    let mut cluster = Cluster::start(1, "").await;
    let workflow = cluster.workflow(&many_jobs(1)).await;
    let old = cluster.agent("runner0-1", &[]).await;
    let mut on_old = old
        .connect_with(
            cluster.addresses[0],
            Hello {
                assignment_revision: 0,
                ..old.hello()
            },
        )
        .await
        .expect("an agent of the previous revision is admitted");
    on_old.idle();
    let run = cluster.dispatch(&workflow).await;
    let job = cluster.job(&run, "j0").await;
    let warned = cluster.servers[0]
        .expect("the warning about the revision", |line| {
            message(line).starts_with("the agent reads another revision of the assignment")
        })
        .await;
    assert_eq!(
        (&warned["agent_revision"], &warned["server_revision"]),
        (&json!(0), &json!(1))
    );
    assert!(
        timeout(Duration::from_millis(500), on_old.assignment())
            .await
            .is_err(),
        "an agent of another revision was offered a job"
    );

    let current = cluster.agent("runner1-1", &[]).await;
    let mut on_current = current.connect(cluster.addresses[0]).await;
    on_current.idle();
    assert_eq!(on_current.assignment().await.job_id, job);
}
