use bosun_db::entity::{
    action_ref, agent, annotation, api_token, api_token_scope, attempt, control_message, execution,
    forge_check_run, forge_installation, forge_repository, idempotency_key, job, label_alias,
    log_chunk, log_object, oidc_state, organisation, outbox, pool_access, project,
    registration_token, role, role_binding, run, run_event, secret, session, sign_in_failure, step,
    step_summary, stream_event, team, team_member, tool_object, user, user_identity, variable,
    webhook_delivery, workflow, workflow_revision,
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
pub const TEAM: u8 = 0x13;
pub const ROLE: u8 = 0x14;
pub const BINDING: u8 = 0x15;
pub const ACCESS: u8 = 0x16;
pub const ALIAS: u8 = 0x17;
pub const DELIVERY: u8 = 0x18;
pub const SCOPE: u8 = 0x19;
pub const SECRET: u8 = 0x1a;
pub const VARIABLE: u8 = 0x1b;
pub const ACTION: u8 = 0x1c;
pub const TOOL: u8 = 0x1d;
pub const IDENTITY: u8 = 0x1e;

pub const OUTBOX: i64 = 41;
pub const CHECK_RUN: i64 = 7_301_115_026;

pub const INSTALLATION: i64 = 52_814_339;
pub const REPOSITORY: i64 = 871_264_503;

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
    pub api_token_scope: api_token_scope::Model,
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
    pub stream_event: stream_event::Model,
    pub log_chunk: log_chunk::Model,
    pub log_object: log_object::Model,
    pub annotation: annotation::Model,
    pub step_summary: step_summary::Model,
    pub control_message: control_message::Model,
    pub idempotency_key: idempotency_key::Model,
    pub session: session::Model,
    pub sign_in_failure: sign_in_failure::Model,
    pub team: team::Model,
    pub team_member: team_member::Model,
    pub role: role::Model,
    pub role_binding: role_binding::Model,
    pub pool_access: pool_access::Model,
    pub label_alias: label_alias::Model,
    pub forge_installation: forge_installation::Model,
    pub forge_repository: forge_repository::Model,
    pub webhook_delivery: webhook_delivery::Model,
    pub secret: secret::Model,
    pub variable: variable::Model,
    pub outbox: outbox::Model,
    pub forge_check_run: forge_check_run::Model,
    pub action_ref: action_ref::Model,
    pub tool_object: tool_object::Model,
    pub user_identity: user_identity::Model,
    pub oidc_state: oidc_state::Model,
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

    let forge_installation = forge_installation::ActiveModel {
        id: ActiveValue::Set(INSTALLATION),
        account_id: ActiveValue::Set(43_265_880),
        account_login: ActiveValue::Set(format!("GlobalArtInc-{label}")),
        organisation_id: ActiveValue::Set(Some(id(ORGANISATION))),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("an installation");

    let forge_repository = forge_repository::ActiveModel {
        id: ActiveValue::Set(REPOSITORY),
        installation_id: ActiveValue::Set(Some(INSTALLATION)),
        owner: ActiveValue::Set("GlobalArtInc".to_owned()),
        name: ActiveValue::Set(format!("keel-{label}")),
        default_branch: ActiveValue::Set("main".to_owned()),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a repository");

    let project = project::ActiveModel {
        id: ActiveValue::Set(id(PROJECT)),
        organisation_id: ActiveValue::Set(id(ORGANISATION)),
        slug: ActiveValue::Set("keel".to_owned()),
        name: ActiveValue::Set(format!("Keel {label}")),
        repository_id: ActiveValue::Set(Some(REPOSITORY)),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a project");

    let webhook_delivery = webhook_delivery::ActiveModel {
        id: ActiveValue::Set(id(DELIVERY)),
        delivery: ActiveValue::Set(format!("72d3162e-cc78-11e3-81ab-{label}")),
        event: ActiveValue::Set("installation".to_owned()),
        payload: ActiveValue::Set(json!({ "action": "created", "label": label })),
        attempts: ActiveValue::Set(2),
        next_attempt_at: ActiveValue::Set(at("2026-10-24T18:34:52Z")),
        last_error: ActiveValue::Set(Some("the forge could not be reached".to_owned())),
        processed_at: ActiveValue::Set(Some(at("2026-10-24T18:34:53.5Z"))),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a webhook delivery");

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
        permissions: ActiveValue::Set(Some(vec!["project.view".to_owned()])),
        restricted: ActiveValue::Set(true),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a personal token");

    let api_token_scope = api_token_scope::ActiveModel {
        id: ActiveValue::Set(id(SCOPE)),
        token_id: ActiveValue::Set(id(TOKEN)),
        organisation_id: ActiveValue::Set(id(ORGANISATION)),
        project_id: ActiveValue::Set(Some(id(PROJECT))),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a scope of a personal token");

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
        trusted: ActiveValue::Set(false),
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
    for (number, key, needs, status, actions) in [
        (
            BUILD,
            "build",
            vec![],
            "in_progress",
            Some(json!([{
                "owner": "actions",
                "repository": "checkout",
                "ref": "v4",
                "commit": format!("{label:0>40}"),
            }])),
        ),
        (TEST, "test", vec!["build".to_owned()], "pending", None),
    ] {
        let job = job::ActiveModel {
            id: ActiveValue::Set(id(number)),
            attempt_id: ActiveValue::Set(id(ATTEMPT)),
            key: ActiveValue::Set(key.to_owned()),
            name: ActiveValue::Set(format!("{key} {label}")),
            group_id: ActiveValue::Set(None),
            matrix: ActiveValue::Set(None),
            strategy: ActiveValue::Set(None),
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
            capabilities: ActiveValue::Set(vec!["podman".to_owned()]),
            queued_at: ActiveValue::Set(Some(at("2026-10-24T18:34:50.078Z"))),
            started_at: ActiveValue::Set(None),
            completed_at: ActiveValue::Set(None),
            actions: ActiveValue::Set(actions),
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
        stages: ActiveValue::Set(Some(
            json!([{ "phase": "pre", "step": 0 }, { "phase": "main", "step": 0 }]),
        )),
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

    let stream_event = stream_event::ActiveModel {
        xact: ActiveValue::Set(812),
        subject: ActiveValue::Set(id(AGENT)),
        project_id: ActiveValue::Set(Some(id(PROJECT))),
        run_id: ActiveValue::Set(Some(id(RUN))),
        kind: ActiveValue::Set("agent.status".to_owned()),
        data: ActiveValue::Set(fixture("events/agent-status.json")["data"].clone()),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a stream event");

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

    let annotation = annotation::ActiveModel {
        step_id: ActiveValue::Set(id(STEP)),
        ordinal: ActiveValue::Set(0),
        kind: ActiveValue::Set("error".to_owned()),
        message: ActiveValue::Set(format!("expected `;` {label}")),
        title: ActiveValue::Set(Some("rustc".to_owned())),
        file: ActiveValue::Set(Some("crates/types/src/api.rs".to_owned())),
        line: ActiveValue::Set(Some(41)),
        end_line: ActiveValue::Set(Some(41)),
        start_column: ActiveValue::Set(Some(9)),
        end_column: ActiveValue::Set(Some(4_294_967_295)),
    }
    .insert(db)
    .await
    .expect("an annotation");

    let step_summary = step_summary::ActiveModel {
        step_id: ActiveValue::Set(id(STEP)),
        key: ActiveValue::Set(format!("summaries/{}/{}.md", id(EXECUTION), id(STEP))),
        size: ActiveValue::Set(2048),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a step summary");

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

    let user_identity = user_identity::ActiveModel {
        id: ActiveValue::Set(id(IDENTITY)),
        user_id: ActiveValue::Set(id(USER)),
        issuer: ActiveValue::Set("https://sso.example.org/realms/main".to_owned()),
        subject: ActiveValue::Set(format!("f81d4fae-7dec-11d0-a765-{label}")),
        created_at: ActiveValue::Set(at("2026-10-24T18:34:50.078Z")),
    }
    .insert(db)
    .await
    .expect("a linked identity");

    let oidc_state = oidc_state::ActiveModel {
        sha256: ActiveValue::Set(vec![0x77; 32]),
        expires_at: ActiveValue::Set(at("2026-10-24T18:44:50Z")),
    }
    .insert(db)
    .await
    .expect("a used state");

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

    let team = team::ActiveModel {
        id: ActiveValue::Set(id(TEAM)),
        organisation_id: ActiveValue::Set(id(ORGANISATION)),
        slug: ActiveValue::Set("platform".to_owned()),
        name: ActiveValue::Set(format!("Platform {label}")),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a team");

    let team_member = team_member::ActiveModel {
        team_id: ActiveValue::Set(id(TEAM)),
        user_id: ActiveValue::Set(id(USER)),
        created_at: ActiveValue::Set(at("2026-10-24T18:34:50.078Z")),
    }
    .insert(db)
    .await
    .expect("a team member");

    let role = role::ActiveModel {
        id: ActiveValue::Set(id(ROLE)),
        organisation_id: ActiveValue::Set(Some(id(ORGANISATION))),
        name: ActiveValue::Set(format!("releaser {label}")),
        permissions: ActiveValue::Set(vec!["run.dispatch".to_owned(), "run.cancel".to_owned()]),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a role");

    let role_binding = role_binding::ActiveModel {
        id: ActiveValue::Set(id(BINDING)),
        role_id: ActiveValue::Set(id(ROLE)),
        organisation_id: ActiveValue::Set(id(ORGANISATION)),
        project_id: ActiveValue::Set(Some(id(PROJECT))),
        user_id: ActiveValue::Set(None),
        team_id: ActiveValue::Set(Some(id(TEAM))),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a role binding");

    let pool_access = pool_access::ActiveModel {
        id: ActiveValue::Set(id(ACCESS)),
        pool_id: ActiveValue::Set(pool_id),
        organisation_id: ActiveValue::Set(Some(id(ORGANISATION))),
        project_id: ActiveValue::Set(Some(id(PROJECT))),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("an entry of an access list");

    let label_alias = label_alias::ActiveModel {
        id: ActiveValue::Set(id(ALIAS)),
        organisation_id: ActiveValue::Set(Some(id(ORGANISATION))),
        label: ActiveValue::Set(format!("ubuntu-{label}")),
        pool_id: ActiveValue::Set(Some(pool_id)),
        capability: ActiveValue::Set(Some("podman".to_owned())),
        image: ActiveValue::Set(Some("docker.io/library/debian:13".to_owned())),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a label alias");

    let secret = secret::ActiveModel {
        id: ActiveValue::Set(id(SECRET)),
        organisation_id: ActiveValue::Set(id(ORGANISATION)),
        project_id: ActiveValue::Set(Some(id(PROJECT))),
        name: ActiveValue::Set(format!("TOKEN_{}", label.to_uppercase())),
        ciphertext: ActiveValue::Set(vec![0x5c; 44]),
        data_key: ActiveValue::Set(vec![0xa7; 60]),
        kek_id: ActiveValue::Set("0f1e2d3c4b5a6978".to_owned()),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a secret");

    let variable = variable::ActiveModel {
        id: ActiveValue::Set(id(VARIABLE)),
        organisation_id: ActiveValue::Set(id(ORGANISATION)),
        project_id: ActiveValue::Set(None),
        name: ActiveValue::Set(format!("REGION_{}", label.to_uppercase())),
        value: ActiveValue::Set("eu-central-1".to_owned()),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a variable");

    let outbox = outbox::ActiveModel {
        id: ActiveValue::Set(OUTBOX),
        kind: ActiveValue::Set("check_run".to_owned()),
        collapse_key: ActiveValue::Set(format!("check_run/job/{}", id(BUILD))),
        payload: ActiveValue::Set(json!({ "name": "build", "label": label })),
        created_at: ActiveValue::Set(at("2026-10-24T18:34:50.078Z")),
        attempts: ActiveValue::Set(3),
        next_attempt_at: ActiveValue::Set(at("2026-10-24T18:34:57.078Z")),
        last_error: ActiveValue::Set(Some("the forge failed: 502".to_owned())),
        failing_since: ActiveValue::Set(Some(at("2026-10-24T18:34:50.5Z"))),
        dead_at: ActiveValue::Set(None),
    }
    .insert(db)
    .await
    .expect("an outbox row");

    let forge_check_run = forge_check_run::ActiveModel {
        job_id: ActiveValue::Set(id(BUILD)),
        id: ActiveValue::Set(CHECK_RUN),
        created_at: ActiveValue::Set(at("2026-10-24T18:34:51.25Z")),
        annotations: ActiveValue::Set(50),
    }
    .insert(db)
    .await
    .expect("a check run");

    let action_ref = action_ref::ActiveModel {
        id: ActiveValue::Set(id(ACTION)),
        owner: ActiveValue::Set("actions".to_owned()),
        repository: ActiveValue::Set("checkout".to_owned()),
        sha: ActiveValue::Set(format!("{label:0>40}")),
        key: ActiveValue::Set(format!("actions/actions/checkout/{label:0>40}.tar.gz")),
        size: ActiveValue::Set(184_320),
        sha256: ActiveValue::Set(vec![0x44; 32]),
        metadata: ActiveValue::Set(json!({ "action.yml": format!("name: Checkout {label}\n") })),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a stored action");

    let tool_object = tool_object::ActiveModel {
        id: ActiveValue::Set(id(TOOL)),
        tool: ActiveValue::Set("node".to_owned()),
        version: ActiveValue::Set("24.22.1".to_owned()),
        platform: ActiveValue::Set(format!("linux-x64-{label}")),
        key: ActiveValue::Set(format!("tools/node/24.22.1/linux-x64-{label}.tar.xz")),
        size: ActiveValue::Set(31_457_280),
        sha256: ActiveValue::Set(vec![0x55; 32]),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("a stored tool");

    Rows {
        organisation,
        project,
        user,
        api_token,
        api_token_scope,
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
        stream_event,
        log_chunk,
        log_object,
        annotation,
        step_summary,
        control_message,
        idempotency_key,
        session,
        sign_in_failure,
        team,
        team_member,
        role,
        role_binding,
        pool_access,
        label_alias,
        forge_installation,
        forge_repository,
        webhook_delivery,
        secret,
        variable,
        outbox,
        forge_check_run,
        action_ref,
        tool_object,
        user_identity,
        oidc_state,
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
        api_token_scope: one::<_, api_token_scope::Entity>(db, id(SCOPE)).await,
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
        stream_event: stream_event::Entity::find()
            .filter(stream_event::Column::Subject.eq(id(AGENT)))
            .one(db)
            .await
            .expect("a query")
            .expect("a stream event"),
        log_chunk: one::<_, log_chunk::Entity>(db, (id(EXECUTION), 1)).await,
        log_object: one::<_, log_object::Entity>(db, id(STEP)).await,
        annotation: one::<_, annotation::Entity>(db, (id(STEP), 0)).await,
        step_summary: one::<_, step_summary::Entity>(db, id(STEP)).await,
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
        team: one::<_, team::Entity>(db, id(TEAM)).await,
        team_member: one::<_, team_member::Entity>(db, (id(TEAM), id(USER))).await,
        role: one::<_, role::Entity>(db, id(ROLE)).await,
        role_binding: one::<_, role_binding::Entity>(db, id(BINDING)).await,
        pool_access: one::<_, pool_access::Entity>(db, id(ACCESS)).await,
        label_alias: one::<_, label_alias::Entity>(db, id(ALIAS)).await,
        forge_installation: one::<_, forge_installation::Entity>(db, INSTALLATION).await,
        forge_repository: one::<_, forge_repository::Entity>(db, REPOSITORY).await,
        webhook_delivery: one::<_, webhook_delivery::Entity>(db, id(DELIVERY)).await,
        secret: one::<_, secret::Entity>(db, id(SECRET)).await,
        variable: one::<_, variable::Entity>(db, id(VARIABLE)).await,
        outbox: one::<_, outbox::Entity>(db, OUTBOX).await,
        forge_check_run: one::<_, forge_check_run::Entity>(db, id(BUILD)).await,
        action_ref: one::<_, action_ref::Entity>(db, id(ACTION)).await,
        tool_object: one::<_, tool_object::Entity>(db, id(TOOL)).await,
        user_identity: one::<_, user_identity::Entity>(db, id(IDENTITY)).await,
        oidc_state: one::<_, oidc_state::Entity>(db, vec![0x77; 32]).await,
    }
}
