use bosun_db::entity::{
    agent, api_token, attempt, control_message, execution, idempotency_key, job, log_chunk,
    log_object, organisation, project, registration_token, run, run_event, session,
    sign_in_failure, step, user, workflow, workflow_revision,
};
use sea_orm::prelude::{DateTimeUtc, Uuid};
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder,
};
use serde_json::{Value, json};

use crate::fixture;

pub const ORGANISATION: u8 = 0x01;
pub const PROJECT: u8 = 0x02;
pub const USER: u8 = 0x03;
pub const TOKEN: u8 = 0x04;
pub const WORKFLOW: u8 = 0x05;
pub const REVISION: u8 = 0x06;
pub const RUN: u8 = 0x07;
pub const ATTEMPT: u8 = 0x08;
pub const BUILD: u8 = 0x09;
pub const TEST: u8 = 0x0a;
pub const EXECUTION: u8 = 0x0b;
pub const STEP: u8 = 0x0c;
pub const AGENT: u8 = 0x0e;
pub const REGISTRATION: u8 = 0x0f;
pub const CONTROL: u8 = 0x10;
pub const REPLICA: u8 = 0x11;
pub const SESSION: u8 = 0x12;

pub fn id(number: u8) -> Uuid {
    Uuid::from_bytes([
        0x01, 0x92, 0xf3, 0xa4, 0x5b, 0x6c, 0x7d, 0x8e, 0x9f, 0x01, 0, 0, 0, 0, 0, number,
    ])
}

pub fn at(text: &str) -> DateTimeUtc {
    text.parse().expect("an RFC 3339 time")
}

fn actor() -> Value {
    json!({ "kind": "user", "id": id(USER), "login": "roman" })
}

#[derive(Debug, PartialEq)]
pub struct Rows {
    pub organisation: organisation::Model,
    pub project: project::Model,
    pub user: user::Model,
    pub api_token: api_token::Model,
    pub workflow: workflow::Model,
    pub workflow_revision: workflow_revision::Model,
    pub run: run::Model,
    pub attempt: attempt::Model,
    pub jobs: Vec<job::Model>,
    pub agent: agent::Model,
    pub registration_token: registration_token::Model,
    pub execution: execution::Model,
    pub step: step::Model,
    pub run_event: run_event::Model,
    pub log_chunk: log_chunk::Model,
    pub log_object: log_object::Model,
    pub control_message: control_message::Model,
    pub idempotency_key: idempotency_key::Model,
    pub session: session::Model,
    pub sign_in_failure: sign_in_failure::Model,
}

pub async fn write<C: ConnectionTrait>(db: &C, label: &str) -> Rows {
    let pool = bosun_db::pools::default_pool(db)
        .await
        .expect("the default pool");
    let pool_id = Uuid::from(pool.id);

    let organisation = organisation::ActiveModel {
        id: ActiveValue::Set(id(ORGANISATION)),
        slug: ActiveValue::Set("globalart".to_owned()),
        name: ActiveValue::Set(format!("GlobalArt {label}")),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("an organisation");

    let project = project::ActiveModel {
        id: ActiveValue::Set(id(PROJECT)),
        organisation_id: ActiveValue::Set(id(ORGANISATION)),
        slug: ActiveValue::Set("keel".to_owned()),
        name: ActiveValue::Set(format!("Keel {label}")),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a project");

    let user = user::ActiveModel {
        id: ActiveValue::Set(id(USER)),
        login: ActiveValue::Set("roman".to_owned()),
        name: ActiveValue::Set(format!("Roman {label}")),
        email: ActiveValue::Set(Some("roman@example.org".to_owned())),
        owner: ActiveValue::Set(true),
        password_hash: ActiveValue::Set(Some(format!("$argon2id$v=19$m=19456,t=2,p=1${label}"))),
        password_change_required: ActiveValue::Set(true),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a user");

    let api_token = api_token::ActiveModel {
        id: ActiveValue::Set(id(TOKEN)),
        user_id: ActiveValue::Set(id(USER)),
        name: ActiveValue::Set(format!("cli {label}")),
        sha256: ActiveValue::Set(vec![0x11; 32]),
        expires_at: ActiveValue::Set(at("2027-10-24T18:34:50.078Z")),
        revoked_at: ActiveValue::Set(None),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a personal token");

    let workflow = workflow::ActiveModel {
        id: ActiveValue::Set(id(WORKFLOW)),
        project_id: ActiveValue::Set(id(PROJECT)),
        name: ActiveValue::Set(format!("ci {label}")),
        source_kind: ActiveValue::Set("server".to_owned()),
        enabled: ActiveValue::Set(true),
        revision_id: ActiveValue::Set(None),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a workflow");

    let workflow_revision = workflow_revision::ActiveModel {
        id: ActiveValue::Set(id(REVISION)),
        workflow_id: ActiveValue::Set(id(WORKFLOW)),
        sha256: ActiveValue::Set(vec![0x22; 32]),
        source: ActiveValue::Set(format!("name: ci {label}\n")),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a revision");

    let mut current: workflow::ActiveModel = workflow.into();
    current.revision_id = ActiveValue::Set(Some(id(REVISION)));
    let workflow = current.update(db).await.expect("the current revision");

    let run = run::ActiveModel {
        id: ActiveValue::Set(id(RUN)),
        project_id: ActiveValue::Set(id(PROJECT)),
        workflow_id: ActiveValue::Set(id(WORKFLOW)),
        revision_id: ActiveValue::Set(id(REVISION)),
        ir: ActiveValue::Set(fixture("ir/two-jobs.json")),
        number: ActiveValue::Set(1),
        event: ActiveValue::Set("workflow_dispatch".to_owned()),
        trigger: ActiveValue::Set(json!({ "actor": actor(), "label": label })),
        git_ref: ActiveValue::Set(None),
        sha: ActiveValue::Set(None),
        inputs: ActiveValue::Set(json!({})),
        status: ActiveValue::Set("in_progress".to_owned()),
        conclusion: ActiveValue::Set(None),
        started_at: ActiveValue::Set(Some(at("2026-10-24T18:34:51Z"))),
        completed_at: ActiveValue::Set(None),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a run");

    let attempt = attempt::ActiveModel {
        id: ActiveValue::Set(id(ATTEMPT)),
        run_id: ActiveValue::Set(id(RUN)),
        number: ActiveValue::Set(1),
        actor: ActiveValue::Set(actor()),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("an attempt");

    let mut jobs = Vec::new();
    for (number, key, needs, status) in [
        (BUILD, "build", vec![], "in_progress"),
        (TEST, "test", vec!["build".to_owned()], "pending"),
    ] {
        let job = job::ActiveModel {
            id: ActiveValue::Set(id(number)),
            attempt_id: ActiveValue::Set(id(ATTEMPT)),
            key: ActiveValue::Set(key.to_owned()),
            name: ActiveValue::Set(format!("{key} {label}")),
            needs: ActiveValue::Set(needs),
            status: ActiveValue::Set(status.to_owned()),
            waiting_reason: ActiveValue::Set(None),
            conclusion: ActiveValue::Set(None),
            conclusion_source: ActiveValue::Set(None),
            continue_on_error: ActiveValue::Set(false),
            evaluated: ActiveValue::Set(None),
            outputs: ActiveValue::Set(json!({})),
            priority: ActiveValue::Set(0),
            labels: ActiveValue::Set(vec!["shell".to_owned()]),
            pool_ids: ActiveValue::Set(vec![pool_id]),
            queued_at: ActiveValue::Set(Some(at("2026-10-24T18:34:50.078Z"))),
            started_at: ActiveValue::Set(None),
            completed_at: ActiveValue::Set(None),
        }
        .insert(db)
        .await
        .expect("a job");
        jobs.push(job);
    }

    let agent = agent::ActiveModel {
        id: ActiveValue::Set(id(AGENT)),
        pool_id: ActiveValue::Set(pool_id),
        name: ActiveValue::Set(format!("runner1-1 {label}")),
        credential_sha256: ActiveValue::Set(vec![0x33; 32]),
        labels: ActiveValue::Set(vec![
            "self-hosted".to_owned(),
            "linux".to_owned(),
            "x64".to_owned(),
            "shell".to_owned(),
        ]),
        capabilities: ActiveValue::Set(vec!["shell".to_owned()]),
        version: ActiveValue::Set(Some("0.1.0".to_owned())),
        online: ActiveValue::Set(true),
        last_seen_at: ActiveValue::Set(Some(at("2026-10-24T18:35:07.512Z"))),
        replica_id: ActiveValue::Set(Some(id(REPLICA))),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("an agent");

    let registration_token = registration_token::ActiveModel {
        id: ActiveValue::Set(id(REGISTRATION)),
        pool_id: ActiveValue::Set(pool_id),
        sha256: ActiveValue::Set(vec![0x44; 32]),
        reusable: ActiveValue::Set(false),
        expires_at: ActiveValue::Set(at("2026-10-25T18:34:50.078Z")),
        used_at: ActiveValue::Set(Some(at("2026-10-24T18:34:50.078Z"))),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a registration token");

    let execution = execution::ActiveModel {
        id: ActiveValue::Set(id(EXECUTION)),
        job_id: ActiveValue::Set(id(BUILD)),
        number: ActiveValue::Set(1),
        agent_id: ActiveValue::Set(id(AGENT)),
        lease_until: ActiveValue::Set(Some(at("2026-10-24T18:35:20.078Z"))),
        status: ActiveValue::Set("in_progress".to_owned()),
        conclusion: ActiveValue::Set(None),
        reason: ActiveValue::Set(None),
        started_at: ActiveValue::Set(Some(at("2026-10-24T18:34:51Z"))),
        completed_at: ActiveValue::Set(None),
        log_bytes: ActiveValue::Set(40),
        log_compacted_at: ActiveValue::Set(None),
    }
    .insert(db)
    .await
    .expect("an execution");

    let step = step::ActiveModel {
        id: ActiveValue::Set(id(STEP)),
        execution_id: ActiveValue::Set(id(EXECUTION)),
        index: ActiveValue::Set(0),
        name: ActiveValue::Set(format!("Build {label}")),
        status: ActiveValue::Set("completed".to_owned()),
        outcome: ActiveValue::Set(Some("success".to_owned())),
        conclusion: ActiveValue::Set(Some("success".to_owned())),
        conclusion_source: ActiveValue::Set(Some("natural".to_owned())),
        outputs: ActiveValue::Set(json!({ "digest": "sha256:0f1e" })),
        started_at: ActiveValue::Set(Some(at("2026-10-24T18:34:51Z"))),
        completed_at: ActiveValue::Set(Some(at("2026-10-24T18:35:07.512Z"))),
    }
    .insert(db)
    .await
    .expect("a step");

    let run_event = run_event::ActiveModel {
        run_id: ActiveValue::Set(id(RUN)),
        attempt_id: ActiveValue::Set(Some(id(ATTEMPT))),
        job_id: ActiveValue::Set(Some(id(BUILD))),
        execution_id: ActiveValue::Set(Some(id(EXECUTION))),
        step_id: ActiveValue::Set(Some(id(STEP))),
        kind: ActiveValue::Set("step.created".to_owned()),
        actor: ActiveValue::Set(json!({ "kind": "system" })),
        data: ActiveValue::Set(json!({ "index": 0, "name": format!("Build {label}") })),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a journal event");

    let log_chunk = log_chunk::ActiveModel {
        execution_id: ActiveValue::Set(id(EXECUTION)),
        seq: ActiveValue::Set(1),
        step_id: ActiveValue::Set(id(STEP)),
        data: ActiveValue::Set(format!("2026-10-24T18:34:51.000000000Z {label}\n").into_bytes()),
        dropped: ActiveValue::Set(0),
    }
    .insert(db)
    .await
    .expect("a log chunk");

    let log_object = log_object::ActiveModel {
        step_id: ActiveValue::Set(id(STEP)),
        execution_id: ActiveValue::Set(id(EXECUTION)),
        key: ActiveValue::Set(format!(
            "logs/{}/{}/{}/{}.log.zst",
            id(RUN),
            id(ATTEMPT),
            id(EXECUTION),
            id(STEP)
        )),
        size: ActiveValue::Set(42),
        lines: ActiveValue::Set(1),
        frames: ActiveValue::Set(vec![0]),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a log object");

    let control_message = control_message::ActiveModel {
        id: ActiveValue::Set(id(CONTROL)),
        execution_id: ActiveValue::Set(id(EXECUTION)),
        kind: ActiveValue::Set("cancel".to_owned()),
        acknowledged_at: ActiveValue::Set(Some(at("2026-10-24T18:35:08Z"))),
        took_effect: ActiveValue::Set(Some(true)),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a control message");

    let idempotency_key = idempotency_key::ActiveModel {
        principal: ActiveValue::Set(format!("user:{}", id(USER))),
        key: ActiveValue::Set("dispatch-1".to_owned()),
        request_sha256: ActiveValue::Set(vec![0x55; 32]),
        status: ActiveValue::Set(201),
        response: ActiveValue::Set(json!({ "id": id(RUN), "label": label })),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("an idempotency key");

    let session = session::ActiveModel {
        id: ActiveValue::Set(id(SESSION)),
        user_id: ActiveValue::Set(id(USER)),
        sha256: ActiveValue::Set(vec![0x66; 32]),
        created_at: ActiveValue::Set(at("2026-10-24T18:34:50.078Z")),
        last_seen_at: ActiveValue::Set(at("2026-10-24T18:40:12.345Z")),
    }
    .insert(db)
    .await
    .expect("a session");

    let sign_in_failure = sign_in_failure::ActiveModel {
        kind: ActiveValue::Set("account".to_owned()),
        key: ActiveValue::Set(format!("roman-{label}")),
        failures: ActiveValue::Set(3),
        last_failed_at: ActiveValue::Set(at("2026-10-24T18:34:50.078Z")),
        locked_until: ActiveValue::Set(Some(at("2026-10-24T18:35:50.078Z"))),
    }
    .insert(db)
    .await
    .expect("a failed sign-in");

    Rows {
        organisation,
        project,
        user,
        api_token,
        workflow,
        workflow_revision,
        run,
        attempt,
        jobs,
        agent,
        registration_token,
        execution,
        step,
        run_event,
        log_chunk,
        log_object,
        control_message,
        idempotency_key,
        session,
        sign_in_failure,
    }
}

async fn one<C, E>(
    db: &C,
    key: impl Into<<E::PrimaryKey as sea_orm::PrimaryKeyTrait>::ValueType>,
) -> E::Model
where
    C: ConnectionTrait,
    E: EntityTrait,
{
    E::find_by_id(key)
        .one(db)
        .await
        .expect("a query")
        .unwrap_or_else(|| panic!("no row in {}", E::default().table_name()))
}

pub async fn read<C: ConnectionTrait>(db: &C) -> Rows {
    Rows {
        organisation: one::<_, organisation::Entity>(db, id(ORGANISATION)).await,
        project: one::<_, project::Entity>(db, id(PROJECT)).await,
        user: one::<_, user::Entity>(db, id(USER)).await,
        api_token: one::<_, api_token::Entity>(db, id(TOKEN)).await,
        workflow: one::<_, workflow::Entity>(db, id(WORKFLOW)).await,
        workflow_revision: one::<_, workflow_revision::Entity>(db, id(REVISION)).await,
        run: one::<_, run::Entity>(db, id(RUN)).await,
        attempt: one::<_, attempt::Entity>(db, id(ATTEMPT)).await,
        jobs: job::Entity::find()
            .filter(job::Column::AttemptId.eq(id(ATTEMPT)))
            .order_by_asc(job::Column::Key)
            .all(db)
            .await
            .expect("the jobs"),
        agent: one::<_, agent::Entity>(db, id(AGENT)).await,
        registration_token: one::<_, registration_token::Entity>(db, id(REGISTRATION)).await,
        execution: one::<_, execution::Entity>(db, id(EXECUTION)).await,
        step: one::<_, step::Entity>(db, id(STEP)).await,
        run_event: run_event::Entity::find()
            .filter(run_event::Column::RunId.eq(id(RUN)))
            .one(db)
            .await
            .expect("a query")
            .expect("a journal event"),
        log_chunk: one::<_, log_chunk::Entity>(db, (id(EXECUTION), 1)).await,
        log_object: one::<_, log_object::Entity>(db, id(STEP)).await,
        control_message: one::<_, control_message::Entity>(db, id(CONTROL)).await,
        idempotency_key: one::<_, idempotency_key::Entity>(
            db,
            (format!("user:{}", id(USER)), "dispatch-1".to_owned()),
        )
        .await,
        session: one::<_, session::Entity>(db, id(SESSION)).await,
        sign_in_failure: sign_in_failure::Entity::find()
            .one(db)
            .await
            .expect("a query")
            .expect("a failed sign-in"),
    }
}
