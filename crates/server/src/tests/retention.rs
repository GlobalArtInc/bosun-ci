use std::collections::BTreeMap;
use std::num::NonZeroU16;
use std::ops::RangeBounds;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::http::{Method, StatusCode};
use bosun_blob::{Key, Object, Store};
use bosun_db::deliveries::NewDelivery;
use bosun_db::logs::{Chunk, Compacted};
use bosun_db::runs::{NewRun, Trust};
use bosun_db::tokens::NewToken;
use bosun_types::api::{Annotation, AnnotationKind, ProjectSettings, Workflow};
use bosun_types::id::{ExecutionId, ProjectId, RunId, StepId};
use bosun_types::status::Conclusion;
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde_json::{Map, Value, json};
use tokio::io::AsyncRead;
use tokio::sync::Notify;

use super::access::{Answer, Credentials, Site, owner_credentials};
use super::queue::parse;
use super::{World, finished, operator, template, workflow};
use crate::agents::progress::{self, StepEnd};
use crate::clock::Clock;
use crate::logs::LIMIT;
use crate::logs::compaction::compact;
use crate::planner;
use crate::retention::{Removed, round};
use crate::site::{Retention, Site as Public};
use crate::store::Backend;
use crate::token::{PERSONAL, Token};

const SUMMARY: &str = "## Built\n\nin 4 minutes\n";

const LINE: &[u8] = b"2026-10-25T10:00:00.000000000Z compiling bosun-server\n";

const DAY: TimeDelta = TimeDelta::days(1);

fn days(count: u16) -> NonZeroU16 {
    NonZeroU16::new(count).expect("a number of days")
}

fn annotation() -> Annotation {
    Annotation {
        kind: AnnotationKind::Warning,
        message: "unused variable `x`".to_owned(),
        title: None,
        file: Some("src/main.rs".to_owned()),
        line: Some(3),
        end_line: None,
        column: None,
        end_column: None,
    }
}

async fn lasting_owner(site: &Site) -> Credentials {
    let token = Token::generate(PERSONAL).expect("random bytes");
    bosun_db::users::create_owner(
        site.db(),
        "keeper",
        NewToken {
            name: "cli".to_owned(),
            sha256: token.sha256(),
            expires_at: Utc::now() + DAY * 3650,
        },
    )
    .await
    .expect("an owner");
    Credentials::Bearer(token.into_string())
}

fn manual() -> Clock {
    Clock::manual(Utc::now())
}

fn retention() -> Retention {
    Retention::default()
}

struct Executed {
    run: RunId,
    execution: ExecutionId,
    step: StepId,
}

impl Executed {
    fn log(&self) -> String {
        format!(
            "/api/v1/executions/{}/steps/{}/log",
            self.execution, self.step
        )
    }

    fn summary(&self) -> String {
        format!(
            "/api/v1/executions/{}/steps/{}/summary",
            self.execution, self.step
        )
    }
}

async fn executed(site: &Site, stored: &Workflow, compacted: bool) -> Executed {
    let world = &site.world;
    let run = world
        .dispatch_in(stored, &workflow(vec![template("build", &[], 1)]), &[])
        .await;
    let (job, _) = world.project(run).await.job("build");
    let execution = world.start(run, job).await;
    progress::step_started(site.db(), world.agent(), execution, 0, None)
        .await
        .expect("a started step");
    bosun_db::logs::ingest(
        site.db(),
        &Chunk {
            agent: world.agent().id,
            execution,
            step: 0,
            seq: 1,
            lines: LINE,
        },
        LIMIT,
    )
    .await
    .expect("an ingestion");
    progress::step_finished(
        site.db(),
        &site.state.store,
        world.agent(),
        execution,
        StepEnd {
            index: 0,
            at: None,
            outcome: Conclusion::Success,
            conclusion: Conclusion::Success,
            outputs: BTreeMap::new(),
            annotations: vec![annotation()],
            summary: Some(SUMMARY.to_owned()),
        },
    )
    .await
    .expect("a finished step");
    world.end(execution, finished(Conclusion::Success)).await;
    if compacted {
        assert_eq!(
            compact(site.db(), site.state.store.as_ref(), execution)
                .await
                .expect("a compaction"),
            Compacted::Done
        );
    }
    let (step, _) = world
        .project(run)
        .await
        .steps
        .into_iter()
        .find(|(_, found)| found.execution == execution)
        .expect("the step");
    Executed {
        run,
        execution,
        step,
    }
}

async fn settle(site: &Site, project: ProjectId, change: impl FnOnce(&mut ProjectSettings)) {
    let stored = bosun_db::projects::settings(site.db(), project)
        .await
        .expect("a read")
        .expect("the settings");
    let mut settings = stored.settings;
    change(&mut settings);
    bosun_db::projects::change_settings(site.db(), project, stored.version, &settings)
        .await
        .expect("a change")
        .expect("the change applied");
}

async fn count<C: ConnectionTrait>(db: &C, sql: &str, values: Vec<sea_orm::Value>) -> i64 {
    db.query_one(sea_orm::Statement::from_sql_and_values(
        sea_orm::DbBackend::Postgres,
        sql,
        values,
    ))
    .await
    .unwrap_or_else(|error| panic!("{sql}: {error}"))
    .expect("a row")
    .try_get::<i64>("", "count")
    .expect("a count")
}

async fn execute<C: ConnectionTrait>(db: &C, sql: &str, values: Vec<sea_orm::Value>) {
    db.execute(sea_orm::Statement::from_sql_and_values(
        sea_orm::DbBackend::Postgres,
        sql,
        values,
    ))
    .await
    .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

async fn objects(site: &Site, prefix: &str) -> Vec<String> {
    site.state
        .store
        .list(prefix)
        .await
        .expect("a listing")
        .into_iter()
        .map(|key| key.to_string())
        .collect()
}

async fn swept(site: &Site, retention: Retention) -> Removed {
    round(
        site.db(),
        site.state.store.as_ref(),
        site.state.clock.now(),
        retention,
    )
    .await
    .expect("a round")
    .expect("the lock of retention")
}

async fn swept_besides_agents(site: &Site, retention: Retention) -> Removed {
    Removed {
        agent_events: 0,
        ..swept(site, retention).await
    }
}

fn expired(answer: &Answer, what: &str, executed: &Executed) {
    assert_eq!(answer.status, StatusCode::GONE, "{answer:?}");
    assert_eq!(answer.body["error"]["code"], "expired", "{answer:?}");
    assert_eq!(
        answer.body["error"]["message"],
        format!(
            "retention removed the {what} of step `{}` of execution `{}`; the execution's \
             `logs_expired_at` says when",
            executed.step, executed.execution
        )
    );
}

fn time(at: DateTime<Utc>) -> Value {
    json!(at.to_rfc3339_opts(SecondsFormat::AutoSi, true))
}

#[tokio::test]
async fn logs_and_summaries_past_their_projects_setting_leave_the_store_and_the_database_and_answer_gone()
 {
    let site = Site::with_clock(manual()).await;
    let owner = lasting_owner(&site).await;
    let keel = site.project().await;
    settle(&site, keel.id, |settings| settings.log_retention = days(30)).await;
    let kept = site.second_project("kept").await;
    let elsewhere = site.workflow(kept.id, "deploy").await;
    let short = executed(&site, &site.world.workflow, true).await;
    let long = executed(&site, &elsewhere, false).await;
    assert_eq!(objects(&site, "logs/").await.len(), 1);
    assert_eq!(objects(&site, "summaries/").await.len(), 2);

    site.state.clock.advance(DAY * 29);
    assert_eq!(
        swept_besides_agents(&site, retention()).await,
        Removed::default()
    );
    let read = site.get(&short.log(), &owner).await;
    assert_eq!(read.status, StatusCode::OK, "{read:?}");

    site.state.clock.advance(DAY * 2);
    let expired_at = site.state.clock.now();
    assert_eq!(
        swept_besides_agents(&site, retention()).await,
        Removed {
            logs: 1,
            objects: 2,
            ..Removed::default()
        }
    );
    assert_eq!(
        objects(&site, &format!("logs/{}/", short.run)).await,
        Vec::<String>::new()
    );
    assert_eq!(
        objects(&site, &format!("summaries/{}/", short.execution)).await,
        Vec::<String>::new()
    );
    assert_eq!(
        objects(&site, &format!("summaries/{}/", long.execution))
            .await
            .len(),
        1
    );
    for (table, column) in [
        ("log_object", "step_id"),
        ("step_summary", "step_id"),
        ("expired_object", "key"),
    ] {
        let left = count(
            site.db(),
            &format!("SELECT count(*) AS count FROM {table} WHERE {column}::text LIKE $1"),
            vec![format!("%{}%", short.step).into()],
        )
        .await;
        assert_eq!(left, 0, "{table}");
    }
    assert_eq!(
        count(
            site.db(),
            "SELECT count(*) AS count FROM log_chunk WHERE execution_id = $1",
            vec![sea_orm::prelude::Uuid::from(short.execution).into()],
        )
        .await,
        0
    );

    let shown = site
        .get(&format!("/api/v1/executions/{}", short.execution), &owner)
        .await;
    assert_eq!(shown.status, StatusCode::OK, "{shown:?}");
    assert_eq!(shown.body["logs_expired_at"], time(expired_at));
    assert_eq!(shown.body["steps"][0]["summary_size"], Value::Null);
    assert_eq!(shown.body["steps"][0]["annotations"], json!([annotation()]));
    for path in [
        short.log(),
        format!("{}?follow=true", short.log()),
        format!("{}?search=compiling", short.log()),
    ] {
        expired(&site.get(&path, &owner).await, "log", &short);
    }
    expired(&site.get(&short.summary(), &owner).await, "summary", &short);
    let run = site
        .get(&format!("/api/v1/runs/{}", short.run), &owner)
        .await;
    assert_eq!(run.status, StatusCode::OK, "{run:?}");

    let other = site
        .get(&format!("/api/v1/executions/{}", long.execution), &owner)
        .await;
    assert_eq!(other.body["logs_expired_at"], Value::Null);
    let summary = site.get(&long.summary(), &owner).await;
    assert_eq!(
        (summary.status, summary.body),
        (StatusCode::OK, json!(SUMMARY))
    );
    let log = site.get(&long.log(), &owner).await;
    assert_eq!(log.status, StatusCode::OK, "{log:?}");
    assert_eq!(log.body, json!(String::from_utf8_lossy(LINE)));

    site.state.clock.advance(DAY * 60);
    assert_eq!(
        swept_besides_agents(&site, retention()).await,
        Removed {
            logs: 1,
            objects: 1,
            ..Removed::default()
        }
    );
    assert_eq!(objects(&site, "logs/").await, Vec::<String>::new());
    assert_eq!(objects(&site, "summaries/").await, Vec::<String>::new());
    assert_eq!(
        count(
            site.db(),
            "SELECT count(*) AS count FROM log_chunk WHERE execution_id = $1",
            vec![sea_orm::prelude::Uuid::from(long.execution).into()],
        )
        .await,
        0
    );
    expired(&site.get(&long.log(), &owner).await, "log", &long);
    assert_eq!(
        swept_besides_agents(&site, retention()).await,
        Removed::default()
    );
}

#[tokio::test]
async fn runs_older_than_run_retention_disappear_with_everything_they_own_and_newer_runs_stay() {
    let site = Site::with_clock(manual()).await;
    let owner = lasting_owner(&site).await;
    let keel = site.project().await;
    settle(&site, keel.id, |settings| {
        settings.run_retention = Some(days(10))
    })
    .await;
    let kept = site.second_project("kept").await;
    let elsewhere = site.workflow(kept.id, "deploy").await;
    let old = executed(&site, &site.world.workflow, true).await;
    let unbounded = executed(&site, &elsewhere, true).await;
    for run in [old.run, unbounded.run] {
        execute(
            site.db(),
            "UPDATE run SET created_at = created_at - interval '11 days', \
             completed_at = completed_at - interval '11 days' WHERE id = $1",
            vec![sea_orm::prelude::Uuid::from(run).into()],
        )
        .await;
    }
    let newer = executed(&site, &site.world.workflow, true).await;
    let running = site
        .world
        .dispatch(&workflow(vec![template("build", &[], 1)]))
        .await;
    execute(
        site.db(),
        "UPDATE run SET created_at = created_at - interval '30 days' WHERE id = $1",
        vec![sea_orm::prelude::Uuid::from(running).into()],
    )
    .await;

    let run = sea_orm::prelude::Uuid::from(old.run);
    let execution = sea_orm::prelude::Uuid::from(old.execution);
    let step = sea_orm::prelude::Uuid::from(old.step);
    let owned = [
        ("SELECT count(*) AS count FROM run WHERE id = $1", run),
        (
            "SELECT count(*) AS count FROM attempt WHERE run_id = $1",
            run,
        ),
        (
            "SELECT count(*) AS count FROM run_event WHERE run_id = $1",
            run,
        ),
        (
            "SELECT count(*) AS count FROM stream_event WHERE run_id = $1",
            run,
        ),
        (
            "SELECT count(*) AS count FROM execution WHERE id = $1",
            execution,
        ),
        (
            "SELECT count(*) AS count FROM step WHERE execution_id = $1",
            execution,
        ),
        (
            "SELECT count(*) AS count FROM log_object WHERE execution_id = $1",
            execution,
        ),
        (
            "SELECT count(*) AS count FROM annotation WHERE step_id = $1",
            step,
        ),
        (
            "SELECT count(*) AS count FROM step_summary WHERE step_id = $1",
            step,
        ),
    ];
    for (sql, value) in owned {
        assert!(count(site.db(), sql, vec![value.into()]).await > 0, "{sql}");
    }

    assert_eq!(
        swept_besides_agents(&site, retention()).await,
        Removed {
            runs: 1,
            objects: 2,
            ..Removed::default()
        }
    );
    for (sql, value) in owned {
        assert_eq!(count(site.db(), sql, vec![value.into()]).await, 0, "{sql}");
    }
    assert_eq!(
        count(
            site.db(),
            "SELECT count(*) AS count FROM expired_object",
            Vec::new()
        )
        .await,
        0
    );
    assert_eq!(
        objects(&site, &format!("logs/{}/", old.run)).await,
        Vec::<String>::new()
    );
    assert_eq!(
        objects(&site, &format!("summaries/{}/", old.execution)).await,
        Vec::<String>::new()
    );
    let gone = site.get(&format!("/api/v1/runs/{}", old.run), &owner).await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND, "{gone:?}");
    for kept in [newer.run, unbounded.run, running] {
        let shown = site.get(&format!("/api/v1/runs/{kept}"), &owner).await;
        assert_eq!(shown.status, StatusCode::OK, "{shown:?}");
    }
    let log = site.get(&newer.log(), &owner).await;
    assert_eq!(log.status, StatusCode::OK, "{log:?}");

    site.state.clock.advance(DAY * 11);
    assert_eq!(
        swept_besides_agents(&site, retention()).await,
        Removed {
            runs: 1,
            objects: 2,
            ..Removed::default()
        }
    );
    let gone = site
        .get(&format!("/api/v1/runs/{}", newer.run), &owner)
        .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND, "{gone:?}");
    for kept in [unbounded.run, running] {
        let shown = site.get(&format!("/api/v1/runs/{kept}"), &owner).await;
        assert_eq!(shown.status, StatusCode::OK, "{shown:?}");
    }

    let capped = Retention {
        max_runs: Some(days(30)),
        ..retention()
    };
    assert_eq!(
        swept_besides_agents(&site, capped).await,
        Removed::default()
    );
    site.state.clock.advance(DAY * 10);
    assert_eq!(
        swept_besides_agents(&site, capped).await,
        Removed {
            runs: 1,
            objects: 2,
            ..Removed::default()
        }
    );
    let gone = site
        .get(&format!("/api/v1/runs/{}", unbounded.run), &owner)
        .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND, "{gone:?}");
    let shown = site.get(&format!("/api/v1/runs/{running}"), &owner).await;
    assert_eq!(shown.status, StatusCode::OK, "{shown:?}");
}

#[derive(Debug, PartialEq)]
struct Delivery {
    delivery: String,
    payload: Value,
    processed_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
}

#[tokio::test]
async fn webhook_payloads_older_than_thirty_days_are_removed_and_their_delivery_ids_still_deduplicate()
 {
    let site = Site::with_clock(manual()).await;
    let now = site.state.clock.now();
    let payload = json!({ "action": "created", "installation": { "id": 7 } });
    for (delivery, age, processed) in [
        ("old-processed", 31, true),
        ("old-pending", 31, false),
        ("young", 29, false),
    ] {
        let stored = bosun_db::deliveries::store(
            site.db(),
            NewDelivery {
                delivery,
                event: "installation",
                payload: &payload,
            },
        )
        .await
        .expect("a stored delivery");
        assert!(stored, "{delivery}");
        execute(
            site.db(),
            "UPDATE webhook_delivery SET received_at = $2, \
             processed_at = CASE WHEN $3 THEN $2 END WHERE delivery = $1",
            vec![delivery.into(), (now - DAY * age).into(), processed.into()],
        )
        .await;
    }

    assert_eq!(
        swept(&site, retention()).await,
        Removed {
            payloads: 2,
            ..Removed::default()
        }
    );
    let rows = site
        .db()
        .query_all(sea_orm::Statement::from_string(
            sea_orm::DbBackend::Postgres,
            "SELECT delivery, payload, processed_at, last_error FROM webhook_delivery \
             ORDER BY delivery",
        ))
        .await
        .expect("the deliveries");
    let found: Vec<Delivery> = rows
        .iter()
        .map(|row| Delivery {
            delivery: row.try_get("", "delivery").expect("a delivery"),
            payload: row.try_get("", "payload").expect("a payload"),
            processed_at: row.try_get("", "processed_at").expect("a time"),
            last_error: row.try_get("", "last_error").expect("an error"),
        })
        .collect();
    assert_eq!(
        found,
        vec![
            Delivery {
                delivery: "old-pending".to_owned(),
                payload: Value::Null,
                processed_at: Some(now),
                last_error: Some(
                    "retention removed the payload of the delivery before the delivery could \
                     be processed"
                        .to_owned()
                ),
            },
            Delivery {
                delivery: "old-processed".to_owned(),
                payload: Value::Null,
                processed_at: Some(now - DAY * 31),
                last_error: None,
            },
            Delivery {
                delivery: "young".to_owned(),
                payload: payload.clone(),
                processed_at: None,
                last_error: None,
            },
        ]
    );
    for delivery in ["old-processed", "old-pending", "young"] {
        let stored = bosun_db::deliveries::store(
            site.db(),
            NewDelivery {
                delivery,
                event: "installation",
                payload: &payload,
            },
        )
        .await
        .expect("a redelivery");
        assert!(!stored, "the redelivery of {delivery} was stored again");
    }
    assert_eq!(
        count(
            site.db(),
            "SELECT count(*) AS count FROM webhook_delivery",
            Vec::new()
        )
        .await,
        3
    );

    site.state.clock.advance(DAY * 2);
    assert_eq!(
        swept(&site, retention()).await,
        Removed {
            payloads: 1,
            ..Removed::default()
        }
    );
}

#[tokio::test]
async fn agent_status_events_older_than_seven_days_are_removed_and_newer_ones_stay() {
    let site = Site::with_clock(manual()).await;
    let agent = site.world.agent().id;
    for drain in [true, false, true] {
        bosun_db::agents::drain(site.db(), agent, drain)
            .await
            .expect("a drain")
            .expect("the agent");
    }
    site.world
        .dispatch(&workflow(vec![template("build", &[], 1)]))
        .await;
    let total = |kind: &'static str| {
        count(
            site.db(),
            "SELECT count(*) AS count FROM stream_event WHERE kind = $1",
            vec![kind.into()],
        )
    };
    let statuses = total("agent.status").await;
    let queued = total("queue.job").await;
    assert!(statuses >= 4, "{statuses} agent.status events");
    assert!(queued >= 1, "{queued} queue.job events");
    execute(
        site.db(),
        "UPDATE stream_event SET at = $1 WHERE seq IN (SELECT seq FROM stream_event \
         WHERE kind = 'agent.status' ORDER BY seq LIMIT 3)",
        vec![(site.state.clock.now() - DAY * 7 - TimeDelta::seconds(1)).into()],
    )
    .await;
    execute(
        site.db(),
        "UPDATE stream_event SET at = $1 WHERE kind = 'queue.job'",
        vec![(site.state.clock.now() - DAY * 30).into()],
    )
    .await;

    assert_eq!(
        swept(&site, retention()).await,
        Removed {
            agent_events: 3,
            ..Removed::default()
        }
    );
    assert_eq!(total("agent.status").await, statuses - 3);
    assert_eq!(total("queue.job").await, queued);

    site.state.clock.advance(DAY * 8);
    assert_eq!(
        swept(&site, retention()).await,
        Removed {
            agent_events: u64::try_from(statuses - 3).expect("a count"),
            ..Removed::default()
        }
    );
    assert_eq!(total("agent.status").await, 0);
}

fn fields(answer: &Answer) -> Vec<(String, String)> {
    assert_eq!(
        answer.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{answer:?}"
    );
    assert_eq!(answer.body["error"]["code"], "invalid_input", "{answer:?}");
    answer.body["error"]["fields"]
        .as_array()
        .expect("the fields")
        .iter()
        .map(|field| {
            (
                field["field"].as_str().expect("a field").to_owned(),
                field["message"].as_str().expect("a message").to_owned(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_retention_setting_above_the_installations_maximum_is_invalid_input() {
    let limits = Retention {
        max_logs: days(30),
        max_runs: Some(days(60)),
    };
    let site = Site::configured(manual(), Public::default().with_retention(limits)).await;
    let owner = owner_credentials(&site).await;
    let path = "/api/v1/projects/globalart/keel/settings";
    let read = site.get(path, &owner).await;
    assert_eq!(read.body["log_retention"], 90);
    let mut tag = read.headers["etag"].to_str().expect("an ETag").to_owned();
    let change = |tag: String, body: Value| {
        let site = &site;
        let owner = &owner;
        async move {
            site.send_with(Method::PATCH, path, owner, &[("if-match", tag)], Some(body))
                .await
        }
    };

    let logs = "`log_retention` is 31 days, and this installation keeps logs and step summaries \
                for at most 30 days";
    let runs = "`run_retention` is 61 days, and this installation keeps runs for at most 60 days";
    for (body, expected) in [
        (
            json!({ "log_retention": 31 }),
            vec![("log_retention", logs)],
        ),
        (
            json!({ "run_retention": 61 }),
            vec![("run_retention", runs)],
        ),
        (
            json!({ "log_retention": 31, "run_retention": 61, "report": "statuses" }),
            vec![("log_retention", logs), ("run_retention", runs)],
        ),
    ] {
        let answer = change(tag.clone(), body.clone()).await;
        let expected: Vec<(String, String)> = expected
            .into_iter()
            .map(|(field, message)| (field.to_owned(), message.to_owned()))
            .collect();
        assert_eq!(fields(&answer), expected, "{body}");
        assert_eq!(
            answer.body["error"]["message"],
            "the settings are not valid"
        );
    }
    for body in [
        json!({ "log_retention": 0 }),
        json!({ "log_retention": 65_536 }),
        json!({ "run_retention": 0 }),
        json!({ "log_retention": "30d" }),
    ] {
        let answer = change(tag.clone(), body.clone()).await;
        let named: Vec<String> = fields(&answer)
            .into_iter()
            .map(|(field, _)| field)
            .collect();
        assert_eq!(named.len(), 1, "{body}");
    }
    assert_eq!(site.get(path, &owner).await.body, read.body);

    let unchanged = change(
        tag.clone(),
        json!({ "fork_policy": "approval", "log_retention": 90 }),
    )
    .await;
    assert_eq!(unchanged.status, StatusCode::OK, "{unchanged:?}");
    tag = unchanged.headers["etag"]
        .to_str()
        .expect("an ETag")
        .to_owned();
    let changed = change(
        tag.clone(),
        json!({ "log_retention": 30, "run_retention": 60 }),
    )
    .await;
    assert_eq!(changed.status, StatusCode::OK, "{changed:?}");
    assert_eq!(
        (
            &changed.body["log_retention"],
            &changed.body["run_retention"]
        ),
        (&json!(30), &json!(60))
    );
    tag = changed.headers["etag"]
        .to_str()
        .expect("an ETag")
        .to_owned();
    let cleared = change(tag, json!({ "run_retention": null })).await;
    assert_eq!(cleared.status, StatusCode::OK, "{cleared:?}");
    assert_eq!(cleared.body["run_retention"], Value::Null);
}

async fn named_after_retention(world: &World, site: &Public) -> String {
    let transaction = world.db().begin().await.expect("a transaction");
    let stored = &world.workflow;
    let payload = json!({ "inputs": null, "ref": null, "workflow": stored.name });
    let run = planner::dispatch(
        &transaction,
        NewRun {
            workflow: stored,
            revision_id: stored.revision_id.expect("a current revision"),
            ir: &parse(
                "run-name: kept ${{ github.retention_days }} days\non: workflow_dispatch\n\
                 jobs:\n  build:\n    runs-on: self-hosted\n    steps:\n      - run: make\n",
            ),
            event: "workflow_dispatch",
            git_ref: None,
            sha: None,
            payload: &payload,
            inputs: &Map::new(),
            actor: &operator(),
            trust: Trust::Trusted,
        },
        &[],
        site,
    )
    .await
    .expect("a dispatch");
    transaction.commit().await.expect("a commit");
    world.run(run.id).await.display_name
}

#[tokio::test]
async fn github_retention_days_is_the_projects_log_retention_capped_by_the_installation() {
    let world = World::new().await;
    let project = world.workflow.project_id;
    assert_eq!(
        named_after_retention(&world, &Public::default()).await,
        "kept 90 days"
    );
    let stored = bosun_db::projects::settings(world.db(), project)
        .await
        .expect("a read")
        .expect("the settings");
    bosun_db::projects::change_settings(
        world.db(),
        project,
        stored.version,
        &ProjectSettings {
            log_retention: days(30),
            ..stored.settings
        },
    )
    .await
    .expect("a change")
    .expect("the change applied");
    assert_eq!(
        named_after_retention(&world, &Public::default()).await,
        "kept 30 days"
    );
    let capped = Public::default().with_retention(Retention {
        max_logs: days(14),
        max_runs: None,
    });
    assert_eq!(named_after_retention(&world, &capped).await, "kept 14 days");
}

struct Gated {
    store: Arc<Backend>,
    gated: AtomicBool,
    reached: Notify,
    open: Notify,
    deleted: Mutex<BTreeMap<String, u32>>,
}

impl Store for Gated {
    async fn put<R>(&self, key: &Key, body: R) -> Result<(), bosun_blob::Error>
    where
        R: AsyncRead + Send + Unpin,
    {
        self.store.put(key, body).await
    }

    async fn get<B>(&self, key: &Key, range: B) -> Result<Object, bosun_blob::Error>
    where
        B: RangeBounds<u64> + Send,
    {
        self.store.get(key, range).await
    }

    async fn delete(&self, key: &Key) -> Result<(), bosun_blob::Error> {
        if self.gated.swap(false, Ordering::SeqCst) {
            self.reached.notify_one();
            self.open.notified().await;
        }
        *self
            .deleted
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .entry(key.to_string())
            .or_default() += 1;
        self.store.delete(key).await
    }

    async fn list(&self, prefix: &str) -> Result<Vec<Key>, bosun_blob::Error> {
        self.store.list(prefix).await
    }
}

#[tokio::test]
async fn with_two_replicas_one_round_runs_at_a_time_and_each_expired_object_is_removed_once() {
    let site = Site::with_clock(manual()).await;
    let keel = site.project().await;
    settle(&site, keel.id, |settings| settings.log_retention = days(1)).await;
    for _ in 0..2 {
        executed(&site, &site.world.workflow, true).await;
    }
    let mut expected: Vec<String> = objects(&site, "logs/").await;
    expected.extend(objects(&site, "summaries/").await);
    expected.sort();
    assert_eq!(expected.len(), 4);
    site.state.clock.advance(DAY * 2);
    let now = site.state.clock.now();

    let gated = Arc::new(Gated {
        store: Arc::clone(&site.state.store),
        gated: AtomicBool::new(true),
        reached: Notify::new(),
        open: Notify::new(),
        deleted: Mutex::new(BTreeMap::new()),
    });
    let replica = site.db().clone();
    let held = Arc::clone(&gated);
    let one = tokio::spawn(async move {
        round(&replica, held.as_ref(), now, retention())
            .await
            .expect("a round")
    });
    gated.reached.notified().await;
    let other = round(site.db(), gated.as_ref(), now, retention())
        .await
        .expect("a round");
    assert_eq!(other, None, "a second replica ran a round at the same time");
    gated.open.notify_one();
    assert_eq!(
        one.await.expect("the first replica's round"),
        Some(Removed {
            logs: 2,
            objects: 4,
            ..Removed::default()
        })
    );
    assert_eq!(
        round(site.db(), gated.as_ref(), now, retention())
            .await
            .expect("a round"),
        Some(Removed::default())
    );
    let deleted = gated
        .deleted
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    assert_eq!(
        deleted,
        expected
            .iter()
            .map(|key| (key.clone(), 1))
            .collect::<BTreeMap<String, u32>>()
    );
    assert_eq!(objects(&site, "logs/").await, Vec::<String>::new());
    assert_eq!(objects(&site, "summaries/").await, Vec::<String>::new());
}
