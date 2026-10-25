use std::net::SocketAddr;
use std::time::Duration;

use bosun_blob::Store as _;
use bosun_proto::agent::v1::agent_message::Message as Report;
use bosun_proto::agent::v1::{
    Accept, Conclusion, Hello, JobFinished, Log, RunningExecution, StepFinished, StepStarted,
};
use bosun_testkit::agent::{Connection, FakeAgent};
use bosun_testkit::{TestBucket, TestDatabase};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::{Instant, sleep, timeout_at};

use crate::api::text;
use crate::proxy::Proxy;
use crate::{Launch, Server, WAIT, client, eventually, owner, strings};

const SOURCE: &str = "name: ci\non: workflow_dispatch\njobs:\n  build:\n    runs-on: [self-hosted]\n    \
                      steps:\n      - run: echo one\n      - run: echo two\n";

struct Bench {
    database: TestDatabase,
    owner: String,
    servers: Vec<Server>,
    addresses: Vec<SocketAddr>,
    proxy: Option<Proxy>,
    store: Shared,
}

enum Shared {
    Directory(TempDir),
    Bucket(TestBucket),
}

struct Job {
    agent: FakeAgent,
    connection: Connection,
    execution: String,
    steps: Vec<String>,
}

impl Bench {
    async fn start(replicas: usize) -> Bench {
        Bench::launch(
            TestDatabase::migrated().await,
            replicas,
            None,
            Shared::directory(),
        )
        .await
    }

    async fn behind_proxy() -> Bench {
        let database = TestDatabase::migrated().await;
        let proxy = Proxy::start(&database.url()).await;
        Bench::launch(database, 1, Some(proxy), Shared::directory()).await
    }

    async fn on_s3(replicas: usize) -> Bench {
        let store = Shared::Bucket(TestBucket::create().await);
        Bench::launch(TestDatabase::migrated().await, replicas, None, store).await
    }

    async fn launch(
        database: TestDatabase,
        replicas: usize,
        proxy: Option<Proxy>,
        store: Shared,
    ) -> Bench {
        let owner = owner(&database.url(), "roman").await;
        let url = match &proxy {
            Some(proxy) => proxy.url(&database.url()),
            None => database.url(),
        };
        let mut servers = Vec::new();
        let mut addresses = Vec::new();
        for _ in 0..replicas {
            let launch = Launch::serve(&url);
            let launch = match &store {
                Shared::Directory(directory) => {
                    launch.env("BOSUN_STORE_PATH", &directory.path().to_string_lossy())
                }
                Shared::Bucket(bucket) => launch.s3(bucket.settings()),
            };
            let mut server = launch.spawn();
            addresses.push(server.listening().await);
            servers.push(server);
        }
        Bench {
            database,
            owner,
            servers,
            addresses,
            proxy,
            store,
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

    async fn values(&self, sql: &str) -> Vec<String> {
        strings(self.database.connection(), sql).await
    }

    async fn job(&self, agent_replica: usize) -> Job {
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
                &json!({ "name": "ci", "source": SOURCE }),
            )
            .await;
        let token = self
            .post(
                "/api/v1/registration-tokens",
                &json!({ "pool": "default", "reusable": false, "expires_at": null }),
            )
            .await;
        let address = self.addresses[agent_replica];
        let fake = FakeAgent::register(address, text(&token["token"]), "runner1", &[]).await;
        let mut connection = fake.connect(address).await;
        self.post(
            &format!("/api/v1/workflows/{}/dispatch", text(&workflow["id"])),
            &json!({ "ref": null, "inputs": {} }),
        )
        .await;
        connection.idle();
        let assignment = connection.assignment().await;
        let execution = assignment.execution_id.clone();
        connection.send(Report::Accept(Accept {
            execution_id: execution.clone(),
        }));
        let sql = format!(
            "SELECT id::text AS value FROM bosun.step WHERE execution_id = '{execution}' \
             ORDER BY index"
        );
        let deadline = Instant::now() + WAIT;
        let steps = loop {
            let steps = self.values(&sql).await;
            if steps.len() == 2 {
                break steps;
            }
            assert!(Instant::now() < deadline, "the steps were not created");
            sleep(Duration::from_millis(50)).await;
        };
        connection.send(Report::StepStarted(StepStarted {
            execution_id: execution.clone(),
            step_index: 0,
            started_at: None,
        }));
        Job {
            agent: fake,
            connection,
            execution,
            steps,
        }
    }

    async fn until_compacted(&self, execution: &str) {
        let sql = format!(
            "SELECT (log_compacted_at IS NOT NULL)::text AS value FROM bosun.execution \
             WHERE id = '{execution}'"
        );
        let deadline = Instant::now() + WAIT;
        while self.values(&sql).await != ["true"] {
            assert!(
                Instant::now() < deadline,
                "the log of {execution} was not compacted within {WAIT:?}"
            );
            sleep(Duration::from_millis(200)).await;
        }
    }

    fn log_path(&self, job: &Job, step: usize) -> String {
        format!(
            "/api/v1/executions/{}/steps/{}/log",
            job.execution, job.steps[step]
        )
    }

    async fn read(&self, replica: usize, path: &str) -> (u16, String) {
        let response = client()
            .get(format!("http://{}{path}", self.addresses[replica]))
            .bearer_auth(&self.owner)
            .send()
            .await
            .unwrap_or_else(|error| panic!("GET {path}: {error}"));
        let status = response.status().as_u16();
        (status, response.text().await.expect("a body"))
    }

    async fn follow(&self, replica: usize, path: &str, last: Option<u64>) -> Follower {
        let mut request = client()
            .get(format!(
                "http://{}{path}?follow=true",
                self.addresses[replica]
            ))
            .bearer_auth(&self.owner);
        if let Some(last) = last {
            request = request.header("last-event-id", last.to_string());
        }
        let response = request
            .send()
            .await
            .unwrap_or_else(|error| panic!("GET {path}?follow=true: {error}"));
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()["content-type"],
            "text/event-stream",
            "a followed log is an event stream"
        );
        Follower {
            response,
            buffer: String::new(),
        }
    }
}

impl Shared {
    fn directory() -> Shared {
        Shared::Directory(tempfile::tempdir().expect("a directory for the shared store"))
    }
}

impl Job {
    async fn log(&mut self, step: u32, seq: u64, from: u64, to: u64) {
        self.connection.send(Report::Log(Log {
            execution_id: self.execution.clone(),
            step_index: step,
            seq,
            lines: lines(from, to).into_bytes(),
        }));
        let ack = self.connection.log_ack().await;
        assert_eq!(
            (ack.execution_id.as_str(), ack.seq),
            (self.execution.as_str(), seq)
        );
    }

    fn finish_step(&self, step: u32) {
        self.connection.send(Report::StepFinished(StepFinished {
            execution_id: self.execution.clone(),
            step_index: step,
            completed_at: None,
            outcome: Conclusion::Success.into(),
            conclusion: Conclusion::Success.into(),
        }));
    }

    fn start_step(&self, step: u32) {
        self.connection.send(Report::StepStarted(StepStarted {
            execution_id: self.execution.clone(),
            step_index: step,
            started_at: None,
        }));
    }

    fn finish(&self) {
        self.connection.send(Report::JobFinished(JobFinished {
            execution_id: self.execution.clone(),
            conclusion: Conclusion::Success.into(),
            outputs: Default::default(),
        }));
    }
}

fn line(number: u64) -> String {
    format!("2026-10-25T10:00:00.{number:09}Z line {number}")
}

fn lines(from: u64, to: u64) -> String {
    (from..=to)
        .map(|number| format!("{}\n", line(number)))
        .collect()
}

fn numbered(from: u64, to: u64) -> Vec<(u64, String)> {
    (from..=to).map(|number| (number, line(number))).collect()
}

struct Follower {
    response: reqwest::Response,
    buffer: String,
}

#[derive(Debug, PartialEq)]
enum Heard {
    Line(u64, String),
    End(u64),
}

impl Follower {
    async fn next(&mut self) -> Option<Heard> {
        let deadline = Instant::now() + WAIT;
        loop {
            while let Some(end) = self.buffer.find("\n\n") {
                let event: String = self.buffer.drain(..end + 2).collect();
                let (mut id, mut kind, mut data) = (None, None, None);
                for field in event.lines() {
                    if let Some(value) = field.strip_prefix("id: ") {
                        id = Some(value.to_owned());
                    } else if let Some(value) = field.strip_prefix("event: ") {
                        kind = Some(value.to_owned());
                    } else if let Some(value) = field.strip_prefix("data: ") {
                        data = Some(value.to_owned());
                    }
                }
                let Some(data) = data else {
                    continue;
                };
                let data: Value = serde_json::from_str(&data).expect("JSON data");
                if kind.as_deref() == Some("end") {
                    return Some(Heard::End(data["lines"].as_u64().expect("a count")));
                }
                let number = data["number"].as_u64().expect("a line number");
                assert_eq!(
                    id,
                    Some(number.to_string()),
                    "the event id is the line number"
                );
                return Some(Heard::Line(
                    number,
                    data["line"].as_str().expect("a line").to_owned(),
                ));
            }
            match timeout_at(deadline, self.response.chunk()).await {
                Ok(Ok(Some(bytes))) => self.buffer.push_str(&String::from_utf8_lossy(&bytes)),
                Ok(Ok(None)) => return None,
                Ok(Err(error)) => panic!("the followed log failed: {error}"),
                Err(_) => panic!("the followed log sent nothing for {WAIT:?}"),
            }
        }
    }

    async fn lines(&mut self, count: usize) -> Vec<(u64, String)> {
        let mut heard = Vec::new();
        while heard.len() < count {
            match self.next().await {
                Some(Heard::Line(number, line)) => heard.push((number, line)),
                other => panic!("after {} lines the stream gave {other:?}", heard.len()),
            }
        }
        heard
    }

    async fn rest(mut self) -> (Vec<(u64, String)>, u64) {
        let mut heard = Vec::new();
        loop {
            match self.next().await {
                Some(Heard::Line(number, line)) => heard.push((number, line)),
                Some(Heard::End(total)) => {
                    assert_eq!(self.next().await, None, "the stream went on after its end");
                    return (heard, total);
                }
                None => panic!("the stream ended without an end event after {heard:?}"),
            }
        }
    }
}

#[tokio::test]
async fn a_follower_attached_in_the_middle_of_a_step_receives_every_line_once_before_and_after_compaction()
 {
    let bench = Bench::start(1).await;
    let mut job = bench.job(0).await;
    let path = bench.log_path(&job, 0);
    for (seq, from) in (1..=3).zip((1..).step_by(10)) {
        job.log(0, seq, from, from + 9).await;
    }
    assert_eq!(bench.read(0, &path).await, (200, lines(1, 30)));

    let mut from_start = bench.follow(0, &path, None).await;
    let resumed = bench.follow(0, &path, Some(25)).await;
    assert_eq!(from_start.lines(30).await, numbered(1, 30));
    for (seq, from) in (4..=6).zip((31..).step_by(10)) {
        job.log(0, seq, from, from + 9).await;
    }
    job.log(0, 4, 31, 40).await;
    job.finish_step(0);
    job.start_step(1);
    job.log(1, 7, 61, 62).await;
    job.finish_step(1);
    job.finish();

    assert_eq!(from_start.rest().await, (numbered(31, 60), 60));
    assert_eq!(resumed.rest().await, (numbered(26, 60), 60));
    let (status, before) = bench.read(0, &path).await;
    assert_eq!((status, before.as_str()), (200, lines(1, 60).as_str()));

    bench.until_compacted(&job.execution).await;
    assert_eq!(
        bench
            .values(&format!(
                "SELECT count(*)::text AS value FROM bosun.log_chunk WHERE execution_id = '{}'",
                job.execution
            ))
            .await,
        ["0"]
    );
    assert_eq!(bench.read(0, &path).await, (200, before));
    assert_eq!(
        bench.follow(0, &path, None).await.rest().await,
        (numbered(1, 60), 60)
    );
    assert_eq!(
        bench.follow(0, &path, Some(45)).await.rest().await,
        (numbered(46, 60), 60)
    );
    assert_eq!(
        bench.read(0, &format!("{path}?offset=10&limit=5")).await,
        (200, lines(11, 15))
    );
    let second = bench.log_path(&job, 1);
    assert_eq!(
        bench.follow(0, &second, None).await.rest().await,
        (vec![(1, line(61)), (2, line(62))], 2)
    );
}

#[tokio::test]
async fn a_server_configured_with_s3_compacts_logs_into_it_and_serves_them_by_range() {
    let bench = Bench::on_s3(2).await;
    let mut job = bench.job(0).await;
    let path = bench.log_path(&job, 0);
    for (seq, from) in (1..=5).zip((1..).step_by(500)) {
        job.log(0, seq, from, from + 499).await;
    }
    job.finish_step(0);
    job.start_step(1);
    job.log(1, 6, 2501, 2502).await;
    job.finish_step(1);
    job.finish();
    bench.until_compacted(&job.execution).await;

    let Shared::Bucket(bucket) = &bench.store else {
        panic!("the bench keeps its logs in a bucket");
    };
    let mut objects: Vec<String> = bucket
        .store()
        .list("logs/")
        .await
        .expect("a listing of the bucket")
        .iter()
        .map(ToString::to_string)
        .collect();
    objects.sort_by_key(|object| !object.ends_with(&format!("/{}.log.zst", job.steps[0])));
    assert_eq!(objects.len(), 2, "{objects:?}");
    for (object, step) in objects.iter().zip(&job.steps) {
        assert!(
            object.ends_with(&format!("/{}/6/{step}.log.zst", job.execution)),
            "{objects:?}"
        );
    }
    assert_eq!(
        bench
            .values(&format!(
                "SELECT array_length(frames, 1)::text AS value FROM bosun.log_object \
                 WHERE execution_id = '{}' ORDER BY lines DESC",
                job.execution
            ))
            .await,
        ["3", "1"]
    );

    for replica in 0..2 {
        assert_eq!(bench.read(replica, &path).await, (200, lines(1, 2500)));
        assert_eq!(
            bench
                .read(replica, &format!("{path}?offset=2100&limit=5"))
                .await,
            (200, lines(2101, 2105))
        );
        assert_eq!(
            bench
                .read(replica, &format!("{path}?offset=998&limit=4"))
                .await,
            (200, lines(999, 1002))
        );
    }
    assert_eq!(
        bench.follow(1, &path, Some(2497)).await.rest().await,
        (numbered(2498, 2500), 2500)
    );
    assert_eq!(
        bench.read(1, &bench.log_path(&job, 1)).await,
        (200, lines(2501, 2502))
    );
}

#[tokio::test]
async fn a_follower_connected_to_one_replica_sees_lines_ingested_by_another() {
    let bench = Bench::start(2).await;
    let mut job = bench.job(1).await;
    let path = bench.log_path(&job, 0);
    let mut follower = bench.follow(0, &path, None).await;
    job.log(0, 1, 1, 3).await;
    assert_eq!(follower.lines(3).await, numbered(1, 3));
    job.log(0, 2, 4, 5).await;
    assert_eq!(follower.lines(2).await, numbered(4, 5));
    job.finish_step(0);
    assert_eq!(follower.rest().await, (Vec::new(), 5));
    assert_eq!(bench.read(0, &path).await, (200, lines(1, 5)));
}

#[tokio::test]
async fn the_log_of_a_step_refuses_what_it_cannot_serve() {
    let bench = Bench::start(1).await;
    let job = bench.job(0).await;
    let path = bench.log_path(&job, 0);
    let other = uuid::Uuid::now_v7();
    for (asked, status, message) in [
        (
            format!("/api/v1/executions/{}/steps/{other}/log", job.execution),
            404,
            format!(
                "there is no step `{other}` in execution `{}`",
                job.execution
            ),
        ),
        (
            format!("/api/v1/executions/{other}/steps/{}/log", job.steps[0]),
            404,
            format!("there is no step `{}` in execution `{other}`", job.steps[0]),
        ),
        (
            format!("/api/v1/executions/{}/steps/0/log", job.execution),
            404,
            format!("there is no step `0` in execution `{}`", job.execution),
        ),
        (
            format!("{path}?offset=-1"),
            422,
            "`offset` is `-1`, which is not a number of lines".to_owned(),
        ),
        (
            format!("{path}?limit=0"),
            422,
            "`limit` is `0`, and a range holds at least one line".to_owned(),
        ),
        (
            format!("{path}?follow=yes"),
            422,
            "`follow` is `yes`, which is neither `true` nor `false`".to_owned(),
        ),
        (
            format!("{path}?search=error"),
            422,
            "a log has no query parameter `search`; it takes `offset`, `limit` and `follow`"
                .to_owned(),
        ),
        (
            format!("{path}?follow=true&limit=5"),
            422,
            "`limit` does not apply to a followed log, which ends with its step".to_owned(),
        ),
    ] {
        let (found, body) = bench.read(0, &asked).await;
        let body: Value = serde_json::from_str(&body).expect("an error envelope");
        assert_eq!(found, status, "{asked}: {body}");
        assert_eq!(body["error"]["message"], message, "{asked}");
    }

    let response = client()
        .get(format!("http://{}{path}?follow=true", bench.addresses[0]))
        .bearer_auth(&bench.owner)
        .header("last-event-id", "the tenth")
        .send()
        .await
        .expect("a refused follow");
    assert_eq!(response.status(), 422);
    let response = client()
        .get(format!("http://{}{path}", bench.addresses[0]))
        .send()
        .await
        .expect("a request without a token");
    assert_eq!(response.status(), 401);
}

#[tokio::test]
async fn a_followed_log_ends_when_the_server_shuts_down() {
    let mut bench = Bench::start(1).await;
    let mut job = bench.job(0).await;
    let path = bench.log_path(&job, 0);
    let mut follower = bench.follow(0, &path, None).await;
    job.log(0, 1, 1, 2).await;
    assert_eq!(follower.lines(2).await, numbered(1, 2));
    let server = bench.servers.remove(0);
    server.terminate();
    assert_eq!(follower.next().await, None, "the followed log went on");
    let (status, lines) = server.exit().await;
    assert!(status.success(), "{lines:#?}");
}

#[tokio::test]
async fn a_chunk_the_server_cannot_place_is_acknowledged_and_a_chunk_numbered_zero_ends_the_stream()
{
    let bench = Bench::start(1).await;
    let mut job = bench.job(0).await;
    job.log(5, 1, 1, 2).await;
    job.log(0, 2, 3, 3).await;
    assert_eq!(
        bench.read(0, &bench.log_path(&job, 0)).await,
        (200, lines(3, 3)),
        "a chunk of a step the execution does not have was stored"
    );
    job.connection.send(Report::Log(Log {
        execution_id: job.execution.clone(),
        step_index: 0,
        seq: 0,
        lines: lines(4, 4).into_bytes(),
    }));
    let status = job.connection.closed().await;
    assert_eq!(status.code(), tonic::Code::InvalidArgument, "{status:?}");
    assert_eq!(
        status.message(),
        "0 is not the sequence number of a log chunk, which counts from 1"
    );
}

#[tokio::test]
async fn a_chunk_the_database_cannot_store_is_not_acknowledged_and_is_stored_once_when_sent_again()
{
    let bench = Bench::behind_proxy().await;
    let mut job = bench.job(0).await;
    job.log(0, 1, 1, 2).await;
    let proxy = bench.proxy.as_ref().expect("a proxy");
    let chunk = Log {
        execution_id: job.execution.clone(),
        step_index: 0,
        seq: 2,
        lines: lines(3, 4).into_bytes(),
    };

    proxy.cut();
    job.connection.send(Report::Log(chunk.clone()));
    match job.connection.message().await {
        Err(status) => {
            assert_eq!(status.code(), tonic::Code::Unavailable, "{status:?}");
            assert_eq!(
                status.message(),
                "the server could not store a log chunk; send it again after reconnecting"
            );
        }
        other => panic!("the server answered a chunk it could not store with {other:?}"),
    }

    proxy.forward();
    eventually(bench.addresses[0], "/readyz", 200).await;
    let hello = Hello {
        executions: vec![RunningExecution {
            execution_id: job.execution.clone(),
            last_log_seq: 1,
        }],
        ..job.agent.hello()
    };
    let mut connection = job
        .agent
        .connect_with(bench.addresses[0], hello)
        .await
        .expect("the agent reconnected");
    connection.send(Report::Log(chunk));
    let ack = connection.log_ack().await;
    assert_eq!(
        (ack.execution_id.as_str(), ack.seq),
        (job.execution.as_str(), 2)
    );
    assert_eq!(
        bench
            .values(&format!(
                "SELECT seq::text AS value FROM bosun.log_chunk WHERE execution_id = '{}' \
                 ORDER BY seq",
                job.execution
            ))
            .await,
        ["1", "2"]
    );
    assert_eq!(
        bench.read(0, &bench.log_path(&job, 0)).await,
        (200, lines(1, 4))
    );
}
