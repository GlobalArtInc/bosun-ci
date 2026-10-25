use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, BodyDataStream};
use axum::http::{Method, StatusCode};
use bosun_db::entity::{api_token, role_binding};
use bosun_db::notify::Listener;
use bosun_db::roles::Subject;
use bosun_db::stream::Cursor;
use bosun_types::api::{Project, Workflow};
use bosun_types::id::RunId;
use sea_orm::prelude::{Expr, Uuid};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde_json::{Value, json};
use tokio::sync::Notify;
use tokio::time::{Instant, timeout, timeout_at};
use tokio_stream::StreamExt;
use tower::ServiceExt;

use super::access::{Credentials, Site, owner_credentials, request};
use super::queue::{connected, runs_on};
use super::{World, run_cancel, template, workflow};
use crate::agents::{self, Replica, Tasks};
use crate::api;
use crate::config::{Agents, Queue};
use crate::events::Hub;
use crate::logs::Followers;
use crate::queue::Dispatcher;
use crate::site::Site as Public;

const WAIT: Duration = Duration::from_secs(30);

const QUIET: Duration = Duration::from_millis(1500);

const HOUR: Duration = Duration::from_secs(3600);

const SHORT: Duration = Duration::from_secs(2);

const LABELS: [&str; 4] = ["self-hosted", "linux", "x64", "shell"];

async fn forward(world: &World, events: Arc<Hub>, followers: Arc<Followers>) -> Tasks {
    let db = world.db().clone();
    let replica = Replica::new();
    let listener = Listener::connect(&world.database.url(), replica.id())
        .await
        .expect("a listener");
    let dispatcher = Dispatcher::new(
        db.clone(),
        Arc::clone(&replica),
        Queue {
            lease: HOUR,
            max_queue_time: HOUR,
        },
        Public::default(),
    );
    agents::start(
        db,
        replica,
        listener,
        Agents {
            heartbeat_interval: HOUR,
            offline_after: HOUR,
            grace: HOUR,
        },
        dispatcher,
        agents::Listeners {
            followers,
            deliveries: Arc::new(Notify::new()),
            events,
        },
    )
}

struct Live {
    site: Site,
    _tasks: Tasks,
}

impl Live {
    async fn new() -> Live {
        Live::lasting(Hub::default()).await
    }

    async fn lasting(hub: Hub) -> Live {
        let mut site = Site::new().await;
        site.state.events = Arc::new(hub);
        site.router = api()
            .into_router(&site.state)
            .with_state(site.state.clone());
        let tasks = forward(
            &site.world,
            Arc::clone(&site.state.events),
            Arc::clone(&site.state.followers),
        )
        .await;
        Live {
            site,
            _tasks: tasks,
        }
    }

    async fn workflow_in(&self, project: &Project) -> Workflow {
        self.site.workflow(project.id, "deploy").await
    }

    async fn dispatch(&self, stored: &Workflow) -> RunId {
        self.site
            .world
            .dispatch_in(stored, &workflow(vec![template("build", &[], 1)]), &[])
            .await
    }

    async fn open<'a>(&'a self, query: &str, credentials: &'a Credentials) -> Subscriber<'a> {
        let mut subscriber = Subscriber {
            live: self,
            query: query.to_owned(),
            credentials,
            last: None,
            body: None,
            buffer: String::new(),
        };
        subscriber.connect().await;
        subscriber
    }

    async fn ask(
        &self,
        query: &str,
        credentials: &Credentials,
    ) -> (StatusCode, Option<BodyDataStream>, Value) {
        self.ask_after(query, credentials, None).await
    }

    async fn ask_after(
        &self,
        query: &str,
        credentials: &Credentials,
        last: Option<&str>,
    ) -> (StatusCode, Option<BodyDataStream>, Value) {
        let builder = request(Method::GET, &format!("/api/v1/events{query}"));
        let builder = match credentials {
            Credentials::Bearer(token) => {
                builder.header("authorization", format!("Bearer {token}"))
            }
            Credentials::Session { cookie, .. } => builder.header("cookie", cookie),
        };
        let builder = match last {
            Some(last) => builder.header("last-event-id", last),
            None => builder,
        };
        let response = self
            .site
            .router
            .clone()
            .oneshot(builder.body(Body::empty()).expect("a request"))
            .await
            .expect("an answer");
        let status = response.status();
        let streamed = response
            .headers()
            .get("content-type")
            .is_some_and(|value| value == "text/event-stream");
        if streamed {
            return (
                status,
                Some(response.into_body().into_data_stream()),
                Value::Null,
            );
        }
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("a body");
        (
            status,
            None,
            serde_json::from_slice(&bytes).expect("a JSON body"),
        )
    }
}

struct Subscriber<'a> {
    live: &'a Live,
    query: String,
    credentials: &'a Credentials,
    last: Option<String>,
    body: Option<BodyDataStream>,
    buffer: String,
}

impl Subscriber<'_> {
    async fn connect(&mut self) {
        let (status, body, answer) = self
            .live
            .ask_after(&self.query, self.credentials, self.last.as_deref())
            .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "GET /api/v1/events{} after {:?}: {answer}",
            self.query,
            self.last
        );
        self.body = Some(body.expect("an event stream"));
        self.buffer.clear();
    }

    fn parsed(&mut self) -> Option<Value> {
        while let Some(end) = self.buffer.find("\n\n") {
            let message: String = self.buffer.drain(..end + 2).collect();
            let mut data = Vec::new();
            for field in message.lines() {
                if let Some(id) = field.strip_prefix("id: ") {
                    self.last = Some(id.to_owned());
                } else if let Some(value) = field.strip_prefix("data: ") {
                    data.push(value);
                }
            }
            if !data.is_empty() {
                return Some(serde_json::from_str(&data.join("\n")).expect("JSON data"));
            }
        }
        None
    }

    async fn read_by(&mut self, deadline: Instant) -> bool {
        let body = self.body.as_mut().expect("a connected stream");
        match timeout_at(deadline, body.next()).await {
            Ok(Some(Ok(bytes))) => self.buffer.push_str(&String::from_utf8_lossy(&bytes)),
            Ok(Some(Err(error))) => panic!("the event stream failed: {error}"),
            Ok(None) => self.body = None,
            Err(_) => return false,
        }
        true
    }

    async fn next_by(&mut self, deadline: Instant) -> Option<Value> {
        loop {
            if let Some(event) = self.parsed() {
                return Some(event);
            }
            if self.body.is_none() {
                self.connect().await;
            }
            if !self.read_by(deadline).await {
                return None;
            }
        }
    }

    async fn until(&mut self, mut done: impl FnMut(&Value) -> bool) -> Vec<Value> {
        let deadline = Instant::now() + WAIT;
        let mut heard = Vec::new();
        loop {
            let Some(event) = self.next_by(deadline).await else {
                panic!(
                    "the stream did not deliver what the test waits for within {WAIT:?}: {heard:#?}"
                );
            };
            let finished = done(&event);
            heard.push(event);
            if finished {
                return heard;
            }
        }
    }

    async fn quiet(&mut self) -> Vec<Value> {
        let deadline = Instant::now() + QUIET;
        let mut heard = Vec::new();
        while let Some(event) = self.next_by(deadline).await {
            heard.push(event);
        }
        heard
    }

    async fn until_end(&mut self) -> Vec<Value> {
        let deadline = Instant::now() + WAIT;
        let mut heard = Vec::new();
        loop {
            while let Some(event) = self.parsed() {
                heard.push(event);
            }
            if self.body.is_none() {
                return heard;
            }
            assert!(
                self.read_by(deadline).await,
                "the event stream did not end within {WAIT:?}: {heard:#?}"
            );
        }
    }
}

fn of_run(event: &Value, run: RunId, kind: &str) -> bool {
    event["kind"] == kind && event["run_id"] == json!(run)
}

fn every(wanted: &[(RunId, &str)]) -> impl FnMut(&Value) -> bool {
    let mut missing: Vec<(RunId, String)> = wanted
        .iter()
        .map(|(run, kind)| (*run, (*kind).to_owned()))
        .collect();
    move |event| {
        missing.retain(|(run, kind)| !of_run(event, *run, kind));
        missing.is_empty()
    }
}

#[tokio::test]
async fn a_viewer_of_one_project_receives_no_event_of_another_through_any_filter() {
    let live = Live::new().await;
    let site = &live.site;
    let seen = site.project().await;
    let hidden = site.second_project("hidden").await;
    let seen_workflow = live.workflow_in(&seen).await;
    let hidden_workflow = live.workflow_in(&hidden).await;
    let earlier = live.dispatch(&seen_workflow).await;
    let (viewer, viewing) = site.user("vera").await;
    site.bind(
        "viewer",
        Subject::User(viewer),
        seen.organisation_id,
        Some(seen.id),
    )
    .await;
    let owner = owner_credentials(site).await;

    let hidden_earlier = live.dispatch(&hidden_workflow).await;
    for (query, message) in [
        (
            "?project=globalart/hidden".to_owned(),
            "there is no project `globalart/hidden`".to_owned(),
        ),
        (
            format!("?project={}", hidden.id),
            format!("there is no project `{}`", hidden.id),
        ),
        (
            "?project=globalart/nowhere".to_owned(),
            "there is no project `globalart/nowhere`".to_owned(),
        ),
        (
            format!("?run={hidden_earlier}"),
            format!("there is no run `{hidden_earlier}`"),
        ),
    ] {
        let (status, _, body) = live.ask(&query, &viewing).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{query}: {body}");
        assert_eq!(body["error"]["code"], "invalid_input", "{query}");
        assert_eq!(body["error"]["message"], message, "{query}");
    }
    let nothing = Uuid::now_v7();
    let (status, _, body) = live.ask(&format!("?run={nothing}"), &viewing).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(
        body["error"]["message"],
        format!("there is no run `{nothing}`")
    );
    let (status, _, body) = live.ask("?kind=run.everything", &viewing).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    let mut everything = live.open("", &viewing).await;
    let mut in_project = live.open("?project=globalart/keel", &viewing).await;
    let mut by_id = live.open(&format!("?project={}", seen.id), &viewing).await;
    let mut statuses = live.open("?kind=job.status", &viewing).await;
    let mut queue = live.open("?kind=queue.job", &viewing).await;
    let mut agents = live.open("?kind=agent.status", &viewing).await;
    let mut of_earlier = live.open(&format!("?run={earlier}"), &viewing).await;
    let mut both = live
        .open(
            &format!("?project=globalart/keel&kind=queue.job&kind=run.status&run={earlier}"),
            &viewing,
        )
        .await;
    let mut watching = live.open("?kind=agent.status", &owner).await;

    let hidden_run = live.dispatch(&hidden_workflow).await;
    connected(&site.world, "runner1-2", &LABELS, Uuid::now_v7()).await;
    let heard = watching
        .until(|event| event["kind"] == "agent.status" && event["data"]["online"] == true)
        .await;
    assert!(heard.iter().all(|event| event["kind"] == "agent.status"));
    site.world.command(earlier, &run_cancel()).await;
    let later = live.dispatch(&seen_workflow).await;

    let mut received = Vec::new();
    received.extend(
        everything
            .until(every(&[(later, "job.status"), (later, "queue.job")]))
            .await,
    );
    received.extend(
        in_project
            .until(every(&[(later, "job.status"), (later, "queue.job")]))
            .await,
    );
    received.extend(
        by_id
            .until(every(&[(later, "job.status"), (later, "queue.job")]))
            .await,
    );
    received.extend(statuses.until(every(&[(later, "job.status")])).await);
    received.extend(queue.until(every(&[(later, "queue.job")])).await);
    received.extend(of_earlier.until(every(&[(earlier, "run.status")])).await);
    received.extend(
        both.until(every(&[(earlier, "run.status"), (earlier, "queue.job")]))
            .await,
    );
    assert_eq!(agents.quiet().await, Vec::<Value>::new());

    assert!(
        received
            .iter()
            .any(|event| of_run(event, later, "run.created"))
    );
    for event in &received {
        assert_ne!(event["kind"], "agent.status", "{event}");
        assert!(
            event["run_id"] == json!(earlier) || event["run_id"] == json!(later),
            "the viewer received an event of another run: {event}"
        );
        if event["kind"] == "queue.job" {
            assert_eq!(event["project_id"], json!(seen.id), "{event}");
        }
        assert_ne!(event["run_id"], json!(hidden_run), "{event}");
        assert_ne!(event["run_id"], json!(hidden_earlier), "{event}");
    }
}

#[tokio::test]
async fn the_queue_names_the_labels_no_online_agent_offers() {
    let live = Live::new().await;
    let site = &live.site;
    let world = &site.world;
    connected(world, "runner1-2", &LABELS, Uuid::now_v7()).await;
    let run = world
        .dispatch(&workflow(vec![
            runs_on(
                template("gpu", &[], 1),
                None,
                &["self-hosted", "GPU", "linux"],
            ),
            runs_on(template("plain", &[], 1), None, &["self-hosted", "linux"]),
        ]))
        .await;
    let waiting = world
        .dispatch(
            &bosun_workflow::parse(
                "ci",
                "on: workflow_dispatch\njobs:\n  test:\n    runs-on: self-hosted\n    \
                 strategy:\n      max-parallel: 1\n      matrix:\n        shard: [1, 2]\n    \
                 steps:\n      - run: make\n",
            )
            .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"))
            .workflow,
        )
        .await;
    let owner = owner_credentials(site).await;
    let (hidden, hidden_credentials) = site.user("hana").await;
    let other = site.second_project("other").await;
    site.bind(
        "viewer",
        Subject::User(hidden),
        other.organisation_id,
        Some(other.id),
    )
    .await;

    let answer = site.get("/api/v1/queue", &owner).await;
    assert_eq!(answer.status, StatusCode::OK, "{answer:?}");
    let entries = answer.body["items"].as_array().expect("a page").clone();
    let entry = |key: &str| -> Value {
        entries
            .iter()
            .find(|entry| entry["job"]["key"] == key && entry["job"]["status"] != "pending")
            .cloned()
            .unwrap_or_else(|| panic!("no entry of `{key}` in {entries:#?}"))
    };
    let projection = world.project(run).await;
    let gpu = entry("gpu");
    assert_eq!(gpu["missing_labels"], json!(["gpu"]));
    assert_eq!(gpu["job"]["status"], "queued");
    assert_eq!(gpu["job"]["run_id"], json!(run));
    assert_eq!(gpu["project_id"], json!(site.project().await.id));
    assert_eq!(gpu["since"], gpu["job"]["queued_at"]);
    assert_eq!(
        gpu["job"]["id"],
        json!(projection.job("gpu").0),
        "the entry is the job of the run"
    );
    assert_eq!(entry("plain")["missing_labels"], json!([]));
    let members: Vec<&Value> = entries
        .iter()
        .filter(|entry| entry["job"]["run_id"] == json!(waiting))
        .collect();
    assert_eq!(members.len(), 2, "{entries:#?}");
    let held = members
        .iter()
        .find(|entry| entry["job"]["status"] == "waiting")
        .expect("a waiting member");
    assert_eq!(held["job"]["waiting_reason"], "max_parallel");
    assert!(held["since"].is_string(), "{held}");
    assert_eq!(held["job"]["queued_at"], Value::Null);
    assert_eq!(entries.len(), 4, "{entries:#?}");

    connected(world, "runner1-3", &["self-hosted", "gpu"], Uuid::now_v7()).await;
    let answer = site.get("/api/v1/queue", &owner).await;
    let entries = answer.body["items"].as_array().expect("a page");
    assert!(
        entries
            .iter()
            .all(|entry| entry["missing_labels"] == json!([])),
        "{entries:#?}"
    );

    let answer = site.get("/api/v1/queue", &hidden_credentials).await;
    assert_eq!(answer.status, StatusCode::OK, "{answer:?}");
    assert_eq!(answer.body["items"], json!([]));
    let first = site.get("/api/v1/queue?limit=1", &owner).await;
    let next = first.body["next"].as_str().expect("a next page");
    let second = site
        .get(&format!("/api/v1/queue?limit=1&cursor={next}"), &owner)
        .await;
    assert_ne!(first.body["items"], second.body["items"]);
}

#[tokio::test]
async fn a_job_entering_and_leaving_the_queue_writes_its_entry_to_the_stream() {
    let live = Live::new().await;
    let site = &live.site;
    let world = &site.world;
    let owner = owner_credentials(site).await;
    let mut subscriber = live.open("?kind=queue.job&kind=agent.status", &owner).await;
    let agent = connected(world, "runner1-2", &LABELS, Uuid::now_v7()).await;
    let run = world
        .dispatch(&workflow(vec![runs_on(
            template("build", &[], 1),
            None,
            &["self-hosted", "gpu"],
        )]))
        .await;
    let heard = subscriber
        .until(|event| event["kind"] == "queue.job" && event["run_id"] == json!(run))
        .await;
    let registered: Vec<&Value> = heard
        .iter()
        .filter(|event| event["kind"] == "agent.status" && event["data"]["id"] == json!(agent.id))
        .collect();
    assert_eq!(registered.len(), 2, "{heard:#?}");
    assert_eq!(registered[0]["data"]["online"], false);
    assert_eq!(registered[1]["data"]["online"], true);
    assert_eq!(registered[1]["project_id"], Value::Null);
    let queued = heard.last().expect("the entry");
    assert_eq!(queued["data"]["job"]["status"], "queued");
    assert_eq!(queued["data"]["missing_labels"], json!(["gpu"]));
    assert_eq!(queued["project_id"], json!(site.project().await.id));

    world.command(run, &run_cancel()).await;
    let left = subscriber.until(|event| event["kind"] == "queue.job").await;
    let left = left.last().expect("the entry");
    assert_eq!(left["data"]["job"]["status"], "completed");
    assert_eq!(left["data"]["since"], Value::Null);
    assert_eq!(left["data"]["missing_labels"], json!([]));
}

#[tokio::test]
async fn a_change_of_an_agent_and_a_journal_event_wake_the_streams_through_bosun_events() {
    let world = World::new().await;
    let hub = Arc::new(Hub::default());
    let _tasks = forward(&world, Arc::clone(&hub), Arc::new(Followers::default())).await;
    let mut everything = hub.subscribe(Vec::new());
    connected(&world, "runner1-2", &LABELS, Uuid::now_v7()).await;
    timeout(WAIT, everything.announced())
        .await
        .expect("the change of the agent woke the stream of everything");

    let run = world
        .dispatch(&workflow(vec![template("build", &[], 1)]))
        .await;
    let mut of_run = hub.subscribe(vec![run]);
    world.command(run, &run_cancel()).await;
    timeout(WAIT, of_run.announced())
        .await
        .expect("the journal event woke the stream of its run");
}

#[tokio::test]
async fn a_stream_that_delivered_nothing_resumes_where_it_began_once_the_server_ends_it() {
    let live = Live::lasting(Hub::lasting(SHORT)).await;
    let site = &live.site;
    let stored = live.workflow_in(&site.project().await).await;
    let owner = owner_credentials(site).await;
    let mut subscriber = live.open("?kind=run.created", &owner).await;
    assert_eq!(subscriber.until_end().await, Vec::<Value>::new());
    let begun = subscriber
        .last
        .clone()
        .expect("the first message of a stream has an id");
    begun
        .parse::<Cursor>()
        .expect("the id is a position of the stream");

    let run = live.dispatch(&stored).await;
    subscriber
        .until(|event| of_run(event, run, "run.created"))
        .await;
}

#[tokio::test]
async fn a_removed_binding_or_a_revoked_token_stops_the_events_once_the_stream_ends() {
    let live = Live::lasting(Hub::lasting(SHORT)).await;
    let site = &live.site;
    let db = site.world.db();
    let project = site.project().await;
    let stored = live.workflow_in(&project).await;
    let (viewer, viewing) = site.user("vera").await;
    site.bind(
        "viewer",
        Subject::User(viewer),
        project.organisation_id,
        Some(project.id),
    )
    .await;
    let mut subscriber = live.open("", &viewing).await;
    let seen = live.dispatch(&stored).await;
    subscriber
        .until(|event| of_run(event, seen, "run.created"))
        .await;

    role_binding::Entity::delete_many()
        .filter(role_binding::Column::UserId.eq(Uuid::from(viewer)))
        .exec(db)
        .await
        .expect("the binding removed");
    subscriber.until_end().await;
    live.dispatch(&stored).await;
    assert_eq!(subscriber.quiet().await, Vec::<Value>::new());

    api_token::Entity::update_many()
        .col_expr(
            api_token::Column::RevokedAt,
            Expr::current_timestamp().into(),
        )
        .filter(api_token::Column::UserId.eq(Uuid::from(viewer)))
        .exec(db)
        .await
        .expect("the token revoked");
    subscriber.until_end().await;
    let (status, _, body) = live
        .ask_after("", &viewing, subscriber.last.as_deref())
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
}
