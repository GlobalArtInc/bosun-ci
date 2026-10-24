use std::net::SocketAddr;
use std::time::Duration;

use bosun_db::control::{self, Kind};
use bosun_proto::agent::v1::{ControlKind, Hello};
use bosun_testkit::TestDatabase;
use bosun_testkit::agent::{FakeAgent, VERSION};
use bosun_types::id::{AgentId, ExecutionId};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use serde_json::{Value, json};
use tokio::time::{Instant, sleep};
use tonic::Code;
use uuid::Uuid;

use crate::{CONFIG, Launch, Server, WAIT, client, message, owner, strings};

const AGENTS: &str = "\n[agents]\nheartbeat_interval = \"200ms\"\noffline_after = \"1s\"\n";

const BEAT: Duration = Duration::from_millis(200);

fn launch(database: &str) -> Launch {
    Launch::serve(database).config(&format!("{CONFIG}{AGENTS}"))
}

async fn post(address: SocketAddr, path: &str, token: Option<&str>, body: Value) -> (u16, Value) {
    let mut request = client().post(format!("http://{address}{path}")).json(&body);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .unwrap_or_else(|error| panic!("POST {path}: {error}"));
    let status = response.status().as_u16();
    (status, response.json().await.expect("a JSON body"))
}

async fn registration_token(address: SocketAddr, owner: &str, reusable: bool) -> String {
    let (status, body) = post(
        address,
        "/api/v1/registration-tokens",
        Some(owner),
        json!({ "pool": "default", "reusable": reusable, "expires_at": null }),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    body["token"]
        .as_str()
        .unwrap_or_else(|| panic!("a token in {body}"))
        .to_owned()
}

async fn agents(address: SocketAddr, owner: &str) -> Vec<Value> {
    let response = client()
        .get(format!("http://{address}/api/v1/agents"))
        .bearer_auth(owner)
        .send()
        .await
        .expect("GET /api/v1/agents");
    assert_eq!(response.status(), 200);
    let page: Value = response.json().await.expect("a JSON body");
    assert_eq!(page["next"], Value::Null, "{page}");
    page["items"].as_array().cloned().unwrap_or_default()
}

async fn agent(address: SocketAddr, owner: &str, id: AgentId) -> Value {
    agents(address, owner)
        .await
        .into_iter()
        .find(|agent| agent["id"] == id.to_string())
        .unwrap_or_else(|| panic!("no agent {id}"))
}

async fn until_online(address: SocketAddr, owner: &str, id: AgentId, online: bool) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let found = agent(address, owner, id).await;
        if found["online"] == online {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "the agent was not online = {online} within {WAIT:?}: {found}"
        );
        sleep(Duration::from_millis(50)).await;
    }
}

async fn execute<C: ConnectionTrait>(db: &C, sql: &str) {
    db.execute_unprepared(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

async fn executions(db: &DatabaseConnection, agents: &[AgentId]) -> Vec<ExecutionId> {
    let organisation = Uuid::now_v7();
    let project = Uuid::now_v7();
    let workflow = Uuid::now_v7();
    let revision = Uuid::now_v7();
    let run = Uuid::now_v7();
    let attempt = Uuid::now_v7();
    execute(
        db,
        &format!(
            "INSERT INTO bosun.organisation (id, slug, name) VALUES ('{organisation}', 'globalart', 'GlobalArt'); \
             INSERT INTO bosun.project (id, organisation_id, slug, name) \
             VALUES ('{project}', '{organisation}', 'keel', 'Keel'); \
             INSERT INTO bosun.workflow (id, project_id, name, source_kind) \
             VALUES ('{workflow}', '{project}', 'ci', 'server'); \
             INSERT INTO bosun.workflow_revision (id, workflow_id, sha256, source) \
             VALUES ('{revision}', '{workflow}', '\\x00', 'name: ci'); \
             INSERT INTO bosun.run (id, project_id, workflow_id, revision_id, ir, number, event, trigger, status) \
             VALUES ('{run}', '{project}', '{workflow}', '{revision}', '{{}}', 1, 'workflow_dispatch', '{{}}', 'in_progress'); \
             INSERT INTO bosun.attempt (id, run_id, number, actor) \
             VALUES ('{attempt}', '{run}', 1, '{{\"kind\": \"system\"}}')"
        ),
    )
    .await;
    let mut executions = Vec::new();
    for (number, agent) in agents.iter().enumerate() {
        let job = Uuid::now_v7();
        let execution = Uuid::now_v7();
        execute(
            db,
            &format!(
                "INSERT INTO bosun.job (id, attempt_id, key, name, status) \
                 VALUES ('{job}', '{attempt}', 'job{number}', 'job{number}', 'in_progress'); \
                 INSERT INTO bosun.execution (id, job_id, number, agent_id, status) \
                 VALUES ('{execution}', '{job}', 1, '{agent}', 'in_progress')"
            ),
        )
        .await;
        executions.push(execution.try_into().expect("a version 7 UUID"));
    }
    executions
}

fn misspelt(token: &str) -> String {
    let (kept, last) = token.split_at(token.len() - 1);
    format!("{kept}{}", if last == "x" { "y" } else { "x" })
}

fn with(agent: &FakeAgent, change: impl FnOnce(&mut Hello)) -> Hello {
    let mut hello = agent.hello();
    change(&mut hello);
    hello
}

#[tokio::test]
async fn a_fake_agent_registers_connects_goes_offline_without_heartbeats_and_back_online_on_reconnect()
 {
    let database = TestDatabase::migrated().await;
    let owner = owner(&database.url(), "roman").await;
    let mut server = launch(&database.url()).spawn();
    let address = server.listening().await;

    let token = registration_token(address, &owner, false).await;
    assert!(token.starts_with("brt_"), "{token}");
    let fake = FakeAgent::register(address, &token, "runner1-1", &["Rust", "rust"]).await;
    assert!(fake.credential().starts_with("bat_"));
    let registered = agent(address, &owner, fake.id()).await;
    assert_eq!(registered["name"], "runner1-1");
    assert_eq!(registered["labels"], json!(["rust"]));
    assert_eq!(registered["online"], false);
    assert_eq!(registered["last_seen_at"], Value::Null);
    assert_eq!(registered["version"], Value::Null);

    let mut connection = fake.connect(address).await;
    assert_eq!(connection.welcome().agent_id, fake.id().to_string());
    assert_eq!(connection.welcome().server_version, VERSION);
    let interval = connection
        .welcome()
        .settings
        .and_then(|settings| settings.heartbeat_interval)
        .expect("a heartbeat interval");
    assert_eq!((interval.seconds, interval.nanos), (0, 200_000_000));
    connection.beat_every(BEAT);
    let online = until_online(address, &owner, fake.id(), true).await;
    assert_eq!(
        online["labels"],
        json!(["self-hosted", "linux", "x64", "shell", "rust"])
    );
    assert_eq!(online["capabilities"], json!(["shell"]));
    assert_eq!(online["version"], VERSION);
    assert_eq!(online["execution_id"], Value::Null);
    sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        agent(address, &owner, fake.id()).await["online"],
        true,
        "an agent that keeps beating went offline"
    );

    connection.stop_beating();
    let closed = connection.closed().await;
    assert_eq!(closed.code(), Code::Unavailable, "{closed:?}");
    assert_eq!(closed.message(), "the agent sent no message within 1s");
    let offline = until_online(address, &owner, fake.id(), false).await;
    assert!(offline["last_seen_at"].is_string(), "{offline}");
    let line = server
        .expect("that the agent went offline", |line| {
            message(line) == "an agent went offline without a heartbeat"
        })
        .await;
    assert_eq!(line["agent_id"], fake.id().to_string());
    assert_eq!(line["name"], "runner1-1");

    let mut again = fake.connect(address).await;
    again.beat_every(BEAT);
    until_online(address, &owner, fake.id(), true).await;
}

#[tokio::test]
async fn registration_takes_a_valid_token_once_and_refuses_what_it_cannot_use() {
    let database = TestDatabase::migrated().await;
    let owner = owner(&database.url(), "roman").await;
    let mut server = launch(&database.url()).spawn();
    let address = server.listening().await;
    let db = database.connection();
    let register = |token: &str, name: &str| {
        let body = json!({ "token": token, "name": name, "labels": [] });
        async move { post(address, "/api/v1/agents/register", None, body).await }
    };

    let single = registration_token(address, &owner, false).await;
    let (status, first) = register(&single, "runner1-1").await;
    assert_eq!(status, 201, "{first}");
    assert!(first["agent_id"].is_string(), "{first}");
    let (status, second) = register(&single, "runner1-2").await;
    assert_eq!(status, 401, "{second}");
    assert_eq!(second["error"]["code"], "unauthenticated");
    assert_eq!(
        second["error"]["message"],
        "the registration token was used already, and it registers one agent only"
    );

    let reusable = registration_token(address, &owner, true).await;
    for name in ["runner2-1", "runner2-2"] {
        let (status, body) = register(&reusable, name).await;
        assert_eq!(status, 201, "{body}");
    }
    let (status, invalid) = register(&reusable, "runner 2").await;
    assert_eq!(status, 422, "{invalid}");
    assert_eq!(invalid["error"]["code"], "invalid_input");
    assert_eq!(invalid["error"]["fields"][0]["field"], "name");

    let expired = registration_token(address, &owner, true).await;
    execute(
        db,
        "UPDATE bosun.registration_token SET expires_at = now() - interval '1 second' \
         WHERE reusable AND used_at IS NULL",
    )
    .await;
    let (status, body) = register(&expired, "runner3-1").await;
    assert_eq!(status, 401, "{body}");
    assert_eq!(body["error"]["message"], "the registration token expired");

    let unknown = registration_token(address, &owner, false).await;
    execute(
        db,
        "DELETE FROM bosun.registration_token WHERE NOT reusable AND used_at IS NULL",
    )
    .await;
    let (status, body) = register(&unknown, "runner4-1").await;
    assert_eq!(status, 401, "{body}");
    assert_eq!(
        body["error"]["message"],
        "the registration token is not known"
    );

    let (status, body) = register(&misspelt(&single), "runner5-1").await;
    assert_eq!(status, 401, "{body}");
    assert_eq!(
        body["error"]["message"],
        "the token is not a well-formed registration token"
    );

    let names = strings(db, "SELECT name AS value FROM bosun.agent ORDER BY name").await;
    assert_eq!(names, ["runner1-1", "runner2-1", "runner2-2"]);

    let page = |query: String| {
        let request = client()
            .get(format!("http://{address}/api/v1/agents?{query}"))
            .bearer_auth(&owner);
        async move {
            let response = request.send().await.expect("GET /api/v1/agents");
            let status = response.status().as_u16();
            (status, response.json::<Value>().await.expect("a JSON body"))
        }
    };
    let (status, first) = page("limit=2".to_owned()).await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["items"].as_array().map(Vec::len), Some(2), "{first}");
    let next = first["next"].as_str().expect("a next page").to_owned();
    assert_eq!(first["items"][1]["id"], next.as_str());
    let (status, second) = page(format!("limit=2&cursor={next}")).await;
    assert_eq!(status, 200, "{second}");
    assert_eq!(
        second["items"].as_array().map(Vec::len),
        Some(1),
        "{second}"
    );
    assert_eq!(second["next"], Value::Null);
    for (query, field) in [
        ("limit=0", "limit"),
        ("limit=501", "limit"),
        ("cursor=x", "cursor"),
    ] {
        let (status, body) = page(query.to_owned()).await;
        assert_eq!(status, 422, "{query}: {body}");
        assert_eq!(
            body["error"]["fields"][0]["field"], field,
            "{query}: {body}"
        );
    }

    for (request, field) in [
        (
            json!({ "pool": "nowhere", "reusable": false, "expires_at": null }),
            "pool",
        ),
        (
            json!({ "pool": "default", "reusable": false, "expires_at": "2020-01-01T00:00:00Z" }),
            "expires_at",
        ),
        (
            json!({ "pool": "default", "reusable": false, "expires_at": "2099-01-01T00:00:00Z" }),
            "expires_at",
        ),
    ] {
        let (status, body) = post(
            address,
            "/api/v1/registration-tokens",
            Some(&owner),
            request,
        )
        .await;
        assert_eq!(status, 422, "{body}");
        assert_eq!(body["error"]["fields"][0]["field"], field, "{body}");
    }
    let (status, body) = post(
        address,
        "/api/v1/registration-tokens",
        None,
        json!({ "pool": "default", "reusable": false, "expires_at": null }),
    )
    .await;
    assert_eq!(status, 401, "{body}");
}

#[tokio::test]
async fn an_agent_of_an_unsupported_protocol_or_version_is_refused_naming_both_versions() {
    let database = TestDatabase::migrated().await;
    let owner = owner(&database.url(), "roman").await;
    let mut server = launch(&database.url()).spawn();
    let address = server.listening().await;
    let token = registration_token(address, &owner, true).await;
    let fake = FakeAgent::register(address, &token, "runner1-1", &[]).await;

    let refused = fake
        .connect_with(
            address,
            with(&fake, |hello| hello.protocol = "bosun.agent.v2".to_owned()),
        )
        .await
        .err()
        .expect("a refusal");
    assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
    assert_eq!(
        refused.message(),
        "the agent speaks the protocol `bosun.agent.v2`, and this server speaks \
         `bosun.agent.v1` only"
    );

    let major: u64 = env!("CARGO_PKG_VERSION_MAJOR").parse().expect("a major");
    let minor: u64 = env!("CARGO_PKG_VERSION_MINOR").parse().expect("a minor");
    let newer = format!("{major}.{}.0", minor + 1);
    let refused = fake
        .connect_with(address, with(&fake, |hello| hello.version = newer.clone()))
        .await
        .err()
        .expect("a refusal");
    assert_eq!(refused.code(), Code::FailedPrecondition, "{refused:?}");
    assert!(
        refused
            .message()
            .starts_with(&format!("the agent is version {newer}, and this server, version {VERSION}, accepts agents of version {major}.{minor}")),
        "{refused:?}"
    );
    server
        .expect("the refusal in the log", |line| {
            message(line) == "refused an agent"
        })
        .await;
    assert_eq!(agent(address, &owner, fake.id()).await["online"], false);

    let refused = fake
        .with_credential(&misspelt(fake.credential()))
        .connect_with(address, fake.hello())
        .await
        .err()
        .expect("a refusal");
    assert_eq!(refused.code(), Code::Unauthenticated, "{refused:?}");
    assert_eq!(
        refused.message(),
        "the bearer token is not a well-formed agent credential"
    );

    let removed = FakeAgent::register(address, &token, "runner1-2", &[]).await;
    execute(
        database.connection(),
        &format!("DELETE FROM bosun.agent WHERE id = '{}'", removed.id()),
    )
    .await;
    let refused = removed
        .connect_with(address, removed.hello())
        .await
        .err()
        .expect("a refusal");
    assert_eq!(refused.code(), Code::Unauthenticated, "{refused:?}");
    assert_eq!(refused.message(), "the agent credential is not known");
}

#[tokio::test]
async fn a_control_message_reaches_the_agent_through_the_replica_that_holds_its_stream() {
    let database = TestDatabase::migrated().await;
    let owner = owner(&database.url(), "roman").await;
    let mut a = launch(&database.url()).spawn();
    let mut b = launch(&database.url()).spawn();
    let (on_a, on_b) = (a.listening().await, b.listening().await);
    let token = registration_token(on_a, &owner, true).await;
    let first = FakeAgent::register(on_a, &token, "runner1-1", &[]).await;
    let second = FakeAgent::register(on_a, &token, "runner1-2", &[]).await;
    let db = database.connection();
    let executions = executions(db, &[first.id(), second.id()]).await;

    let mut first_on_b = first.connect(on_b).await;
    let mut second_on_a = second.connect(on_a).await;
    first_on_b.beat_every(BEAT);
    second_on_a.beat_every(BEAT);
    let replica = |server: &Server| {
        let line = server
            .seen
            .iter()
            .find(|line| message(line) == "listening")
            .cloned()
            .expect("a listening line");
        line["replica_id"]
            .as_str()
            .expect("a replica id")
            .to_owned()
    };
    let expected = [replica(&b), replica(&a)];
    let recorded = strings(
        db,
        &format!(
            "SELECT replica_id::text AS value FROM bosun.agent WHERE id IN ('{}', '{}') \
             ORDER BY name",
            first.id(),
            second.id()
        ),
    )
    .await;
    assert_eq!(recorded, expected);

    let to_first = control::publish(db, executions[0], Kind::Cancel)
        .await
        .expect("a control message for the agent on replica B");
    let received = first_on_b.control().await;
    assert_eq!(received.id, to_first.id.to_string());
    assert_eq!(received.execution_id, executions[0].to_string());
    assert_eq!(received.kind(), ControlKind::Cancel);
    first_on_b.acknowledge(&received, true);

    let to_second = control::publish(db, executions[1], Kind::Cancel)
        .await
        .expect("a control message for the agent on replica A");
    let received = second_on_a.control().await;
    assert_eq!(received.id, to_second.id.to_string());
    second_on_a.acknowledge(&received, false);

    let acknowledged = |id: Uuid| {
        let sql = format!(
            "SELECT concat_ws(' ', acknowledged_at IS NOT NULL, took_effect) AS value \
             FROM bosun.control_message WHERE id = '{id}'"
        );
        async move {
            let deadline = Instant::now() + WAIT;
            loop {
                let found = strings(db, &sql).await;
                if found != ["f"] {
                    return found;
                }
                assert!(Instant::now() < deadline, "{id} was not acknowledged");
                sleep(Duration::from_millis(50)).await;
            }
        }
    };
    assert_eq!(acknowledged(to_first.id).await, ["t t"]);
    assert_eq!(acknowledged(to_second.id).await, ["t f"]);

    drop(first_on_b);
    let deadline = Instant::now() + WAIT;
    while strings(
        db,
        &format!(
            "SELECT coalesce(replica_id::text, 'none') AS value FROM bosun.agent WHERE id = '{}'",
            first.id()
        ),
    )
    .await
        != ["none"]
    {
        assert!(Instant::now() < deadline, "replica B kept the agent");
        sleep(Duration::from_millis(50)).await;
    }
    let while_away = control::publish(db, executions[0], Kind::Cancel)
        .await
        .expect("a control message for an agent without a stream");
    let mut first_on_a = first.connect(on_a).await;
    let received = first_on_a.control().await;
    assert_eq!(received.id, while_away.id.to_string());
    first_on_a.acknowledge(&received, true);
    assert_eq!(acknowledged(while_away.id).await, ["t t"]);

    let mut newer = first.connect(on_a).await;
    let replaced = first_on_a.closed().await;
    assert_eq!(replaced.code(), Code::Aborted, "{replaced:?}");
    assert_eq!(
        replaced.message(),
        "a newer connection of this agent replaced this stream"
    );
    a.expect("that the replaced stream ended", |line| {
        message(line) == "a replaced stream of an agent ended"
    })
    .await;
    let after_replacement = control::publish(db, executions[0], Kind::Cancel)
        .await
        .expect("a control message for the newer stream");
    assert_eq!(newer.control().await.id, after_replacement.id.to_string());
}

#[tokio::test]
async fn an_agent_stream_ends_with_unavailable_and_the_reason_when_the_server_stops() {
    let database = TestDatabase::migrated().await;
    let owner = owner(&database.url(), "roman").await;
    let mut server = launch(&database.url()).spawn();
    let address = server.listening().await;
    let token = registration_token(address, &owner, false).await;
    let fake = FakeAgent::register(address, &token, "runner1-1", &[]).await;
    let mut connection = fake.connect(address).await;
    connection.beat_every(BEAT);
    until_online(address, &owner, fake.id(), true).await;

    server.terminate();
    let closed = connection.closed().await;
    assert_eq!(closed.code(), Code::Unavailable, "{closed:?}");
    assert_eq!(
        closed.message(),
        "the server is shutting down: received SIGTERM"
    );
    let (status, lines) = server.exit().await;
    assert!(status.success(), "{lines:#?}");
}
