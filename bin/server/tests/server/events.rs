use std::collections::BTreeSet;
use std::net::SocketAddr;

use bosun_proto::agent::v1::agent_message::Message as Report;
use bosun_proto::agent::v1::{Accept, Conclusion, StepFinished, StepStarted};
use bosun_testkit::TestDatabase;
use bosun_testkit::agent::{Connection, FakeAgent};
use serde_json::{Value, json};
use tokio::time::{Instant, timeout_at};

use crate::api::text;
use crate::{Launch, Server, WAIT, client, owner, strings};

const SOURCE: &str = "name: ci\non: workflow_dispatch\njobs:\n  build:\n    runs-on: [self-hosted]\n    \
                      steps:\n      - run: echo one\n  test:\n    needs: build\n    \
                      runs-on: [self-hosted]\n    steps:\n      - run: echo two\n";

struct Cluster {
    database: TestDatabase,
    owner: String,
    _servers: Vec<Server>,
    addresses: Vec<SocketAddr>,
}

impl Cluster {
    async fn start(replicas: usize) -> Cluster {
        let database = TestDatabase::migrated().await;
        let owner = owner(&database.url(), "roman").await;
        let mut servers = Vec::new();
        let mut addresses = Vec::new();
        for _ in 0..replicas {
            let mut server = Launch::serve(&database.url()).spawn();
            addresses.push(server.listening().await);
            servers.push(server);
        }
        Cluster {
            database,
            owner,
            _servers: servers,
            addresses,
        }
    }

    async fn post(&self, replica: usize, path: &str, body: &Value) -> Value {
        let response = client()
            .post(format!("http://{}{path}", self.addresses[replica]))
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

    async fn workflow(&self, replica: usize) -> String {
        self.post(
            replica,
            "/api/v1/organisations",
            &json!({ "slug": "globalart", "name": "GlobalArt" }),
        )
        .await;
        let project = self
            .post(
                replica,
                "/api/v1/projects",
                &json!({ "organisation": "globalart", "slug": "ops", "name": "Operations" }),
            )
            .await;
        let workflow = self
            .post(
                replica,
                &format!("/api/v1/projects/{}/workflows", text(&project["id"])),
                &json!({ "name": "ci", "source": SOURCE }),
            )
            .await;
        text(&workflow["id"]).to_owned()
    }

    async fn dispatch(&self, replica: usize, workflow: &str) -> String {
        let run = self
            .post(
                replica,
                &format!("/api/v1/workflows/{workflow}/dispatch"),
                &json!({ "ref": null, "inputs": {} }),
            )
            .await;
        text(&run["id"]).to_owned()
    }

    async fn agent(&self, replica: usize, name: &str) -> FakeAgent {
        let token = self
            .post(
                replica,
                "/api/v1/registration-tokens",
                &json!({ "pool": "default", "reusable": false, "expires_at": null }),
            )
            .await;
        FakeAgent::register(self.addresses[replica], text(&token["token"]), name, &[]).await
    }

    async fn subscribe(&self, replica: usize, query: &str, last: Option<&str>) -> Subscriber {
        let mut subscriber = Subscriber {
            address: self.addresses[replica],
            token: self.owner.clone(),
            query: query.to_owned(),
            last: last.map(str::to_owned),
            response: None,
            buffer: String::new(),
        };
        subscriber.connect().await;
        subscriber
    }

    async fn journal(&self, run: &str) -> Vec<u64> {
        strings(
            self.database.connection(),
            &format!(
                "SELECT seq::text AS value FROM bosun.run_event WHERE run_id = '{run}' \
                 ORDER BY seq"
            ),
        )
        .await
        .into_iter()
        .map(|seq| seq.parse().expect("a sequence number"))
        .collect()
    }
}

struct Subscriber {
    address: SocketAddr,
    token: String,
    query: String,
    last: Option<String>,
    response: Option<reqwest::Response>,
    buffer: String,
}

#[derive(Debug, Clone, PartialEq)]
struct Heard {
    id: String,
    event: Value,
}

impl Heard {
    fn journal(&self) -> bool {
        self.event.get("attempt_id").is_some()
    }

    fn identity(&self) -> (bool, u64) {
        (
            self.journal(),
            self.event["seq"].as_u64().expect("a sequence number"),
        )
    }

    fn is(&self, kind: &str, run: &str) -> bool {
        self.event["kind"] == kind && self.event["run_id"] == run
    }
}

impl Subscriber {
    async fn connect(&mut self) {
        let mut request = client()
            .get(format!(
                "http://{}/api/v1/events{}",
                self.address, self.query
            ))
            .bearer_auth(&self.token);
        if let Some(last) = &self.last {
            request = request.header("last-event-id", last);
        }
        let response = request.send().await.expect("the event stream");
        assert_eq!(response.status(), 200, "GET /api/v1/events{}", self.query);
        assert_eq!(response.headers()["content-type"], "text/event-stream");
        self.response = Some(response);
        self.buffer.clear();
    }

    async fn next(&mut self) -> Heard {
        let deadline = Instant::now() + WAIT;
        loop {
            while let Some(end) = self.buffer.find("\n\n") {
                let message: String = self.buffer.drain(..end + 2).collect();
                let mut id = None;
                let mut data = Vec::new();
                for field in message.lines() {
                    if let Some(value) = field.strip_prefix("id: ") {
                        id = Some(value.to_owned());
                    } else if let Some(value) = field.strip_prefix("data: ") {
                        data.push(value);
                    }
                }
                if id.is_some() {
                    self.last.clone_from(&id);
                }
                if data.is_empty() {
                    continue;
                }
                let id = id.expect("every event has an id");
                return Heard {
                    id,
                    event: serde_json::from_str(&data.join("\n")).expect("JSON data"),
                };
            }
            let response = self.response.as_mut().expect("a connected stream");
            match timeout_at(deadline, response.chunk()).await {
                Ok(Ok(Some(bytes))) => self.buffer.push_str(&String::from_utf8_lossy(&bytes)),
                Ok(Ok(None)) => self.connect().await,
                Ok(Err(error)) => panic!("the event stream failed: {error}"),
                Err(_) => panic!("the event stream sent nothing for {WAIT:?}"),
            }
        }
    }

    async fn until(&mut self, mut done: impl FnMut(&Heard) -> bool) -> Vec<Heard> {
        let mut heard = Vec::new();
        loop {
            let next = self.next().await;
            let finished = done(&next);
            heard.push(next);
            if finished {
                return heard;
            }
        }
    }
}

async fn run_step(connection: &mut Connection) -> String {
    connection.idle();
    let assignment = connection.assignment().await;
    let execution = assignment.execution_id.clone();
    connection.send(Report::Accept(Accept {
        execution_id: execution.clone(),
    }));
    connection.send(Report::StepStarted(StepStarted {
        execution_id: execution.clone(),
        step_index: 0,
        started_at: None,
    }));
    connection.send(Report::StepFinished(StepFinished {
        execution_id: execution.clone(),
        step_index: 0,
        completed_at: None,
        outcome: Conclusion::Success.into(),
        conclusion: Conclusion::Success.into(),
        ..Default::default()
    }));
    connection.finish_job(&execution, Conclusion::Success, &[]);
    execution
}

fn sequence(heard: &[Heard], journal: bool) -> Vec<(bool, u64)> {
    heard
        .iter()
        .filter(|heard| heard.journal() == journal)
        .map(Heard::identity)
        .collect()
}

fn after(all: &[(bool, u64)], last: (bool, u64)) -> Vec<(bool, u64)> {
    let place = all
        .iter()
        .position(|identity| *identity == last)
        .unwrap_or_else(|| panic!("{last:?} is not among {all:?}"));
    all[place + 1..].to_vec()
}

#[tokio::test]
async fn a_subscriber_on_one_replica_receives_the_events_of_a_run_driven_through_another_once_and_in_order()
 {
    let cluster = Cluster::start(2).await;
    let workflow = cluster.workflow(1).await;
    let mut subscriber = cluster.subscribe(0, "?project=globalart/ops", None).await;
    let fake = cluster.agent(1, "runner1").await;
    let mut connection = fake.connect(cluster.addresses[1]).await;
    let run = cluster.dispatch(1, &workflow).await;
    run_step(&mut connection).await;
    run_step(&mut connection).await;

    let heard = subscriber
        .until(|heard| heard.is("run.status", &run) && heard.event["data"]["to"] == "completed")
        .await;
    assert_eq!(
        heard.last().map(|heard| &heard.event["data"]["conclusion"]),
        Some(&json!("success"))
    );
    let delivered: Vec<u64> = heard
        .iter()
        .filter(|heard| heard.journal() && heard.event["run_id"] == run.as_str())
        .map(|heard| heard.identity().1)
        .collect();
    assert_eq!(delivered, cluster.journal(&run).await);
    let queue: Vec<(String, String)> = heard
        .iter()
        .filter(|heard| heard.is("queue.job", &run))
        .map(|heard| {
            (
                text(&heard.event["data"]["job"]["key"]).to_owned(),
                text(&heard.event["data"]["job"]["status"]).to_owned(),
            )
        })
        .collect();
    let pairs = |key: &str| -> Vec<String> {
        queue
            .iter()
            .filter(|(job, _)| job == key)
            .map(|(_, status)| status.clone())
            .collect()
    };
    assert_eq!(pairs("build"), ["queued", "assigned"]);
    assert_eq!(pairs("test"), ["queued", "assigned"]);
    let identities: BTreeSet<(bool, u64)> = heard.iter().map(Heard::identity).collect();
    assert_eq!(
        identities.len(),
        heard.len(),
        "an event was delivered twice"
    );
    let ids: BTreeSet<&str> = heard.iter().map(|heard| heard.id.as_str()).collect();
    assert_eq!(ids.len(), heard.len(), "two events share an id");
}

#[tokio::test]
async fn reconnecting_with_the_last_event_id_after_a_gap_delivers_exactly_the_missed_events_of_both_sequences()
 {
    let cluster = Cluster::start(2).await;
    let workflow = cluster.workflow(0).await;
    let mut continuous = cluster.subscribe(0, "", None).await;
    let mut gapped = cluster.subscribe(0, "", None).await;

    let first_agent = cluster.agent(0, "runner1").await;
    let first = cluster.dispatch(0, &workflow).await;
    let mut seen_queue = false;
    let mut seen_journal = false;
    let before = gapped
        .until(|heard| {
            seen_queue |= heard.is("queue.job", &first);
            seen_journal |= heard.is("job.status", &first);
            seen_queue && seen_journal
        })
        .await;
    let last = before.last().expect("an event").id.clone();
    let last_journal = *sequence(&before, true).last().expect("a journal event");
    let last_stream = *sequence(&before, false).last().expect("a stream event");
    drop(gapped);

    let _connection = first_agent.connect(cluster.addresses[1]).await;
    let second = cluster.dispatch(1, &workflow).await;
    let second_agent = cluster.agent(1, "runner2").await;
    let everything = continuous
        .until(|heard| {
            heard.event["kind"] == "agent.status"
                && heard.event["data"]["id"] == json!(second_agent.id())
        })
        .await;
    let journal = after(&sequence(&everything, true), last_journal);
    let stream = after(&sequence(&everything, false), last_stream);
    assert!(
        journal.len() > 1 && stream.len() > 1,
        "the gap holds events of both sequences: {journal:?} {stream:?}"
    );
    assert!(
        everything
            .iter()
            .any(|heard| heard.is("run.created", &second))
    );

    let mut resumed = cluster.subscribe(1, "", Some(&last)).await;
    let (final_journal, final_stream) = (
        *journal.last().expect("a journal event"),
        *stream.last().expect("a stream event"),
    );
    let mut reached = (false, false);
    let missed = resumed
        .until(|heard| {
            reached.0 |= heard.identity() == final_journal;
            reached.1 |= heard.identity() == final_stream;
            reached.0 && reached.1
        })
        .await;
    let upto = |all: Vec<(bool, u64)>, end: (bool, u64)| -> Vec<(bool, u64)> {
        let place = all
            .iter()
            .position(|identity| *identity == end)
            .expect("the last event of the gap");
        all[..=place].to_vec()
    };
    assert_eq!(upto(sequence(&missed, true), final_journal), journal);
    assert_eq!(upto(sequence(&missed, false), final_stream), stream);
    for heard in &missed {
        let original = everything
            .iter()
            .find(|seen| seen.identity() == heard.identity())
            .expect("an event the continuous subscriber received");
        assert_eq!(heard.event, original.event);
    }
}
