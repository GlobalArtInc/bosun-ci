use std::collections::BTreeSet;

use bosun_db::entity::{
    agent, api_token, api_token_scope, attempt, control_message, execution, forge_installation,
    forge_repository, idempotency_key, job, label_alias, log_chunk, log_object, organisation, pool,
    pool_access, project, registration_token, role, role_binding, run, run_event, schema_version,
    secret, session, sign_in_failure, step, stream_event, team, team_member, user, variable,
    webhook_delivery, workflow, workflow_revision,
};
use bosun_testkit::TestDatabase;
use sea_orm::EntityName;

use crate::{rows, strings};

#[tokio::test]
async fn every_entity_round_trips_a_row() {
    let database = TestDatabase::migrated().await;
    let written = rows::write(database.connection(), "alone").await;
    assert_eq!(rows::read(database.connection()).await, written);
}

#[tokio::test]
async fn every_table_has_an_entity() {
    let database = TestDatabase::migrated().await;
    let entities: BTreeSet<String> = [
        agent::Entity.table_name(),
        api_token::Entity.table_name(),
        api_token_scope::Entity.table_name(),
        attempt::Entity.table_name(),
        control_message::Entity.table_name(),
        execution::Entity.table_name(),
        forge_installation::Entity.table_name(),
        forge_repository::Entity.table_name(),
        idempotency_key::Entity.table_name(),
        job::Entity.table_name(),
        label_alias::Entity.table_name(),
        log_chunk::Entity.table_name(),
        log_object::Entity.table_name(),
        organisation::Entity.table_name(),
        pool::Entity.table_name(),
        pool_access::Entity.table_name(),
        project::Entity.table_name(),
        registration_token::Entity.table_name(),
        role::Entity.table_name(),
        role_binding::Entity.table_name(),
        run::Entity.table_name(),
        run_event::Entity.table_name(),
        schema_version::Entity.table_name(),
        secret::Entity.table_name(),
        session::Entity.table_name(),
        sign_in_failure::Entity.table_name(),
        step::Entity.table_name(),
        stream_event::Entity.table_name(),
        team::Entity.table_name(),
        team_member::Entity.table_name(),
        user::Entity.table_name(),
        variable::Entity.table_name(),
        webhook_delivery::Entity.table_name(),
        workflow::Entity.table_name(),
        workflow_revision::Entity.table_name(),
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let tables: BTreeSet<String> = strings(
        database.connection(),
        "SELECT table_name::text AS value FROM information_schema.tables \
         WHERE table_schema = 'bosun' AND table_name NOT LIKE 'migrations%'",
    )
    .await
    .into_iter()
    .collect();
    assert_eq!(entities, tables);
}
