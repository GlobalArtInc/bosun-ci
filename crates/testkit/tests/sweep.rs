use std::env;

use bosun_testkit::TestDatabase;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};
use url::Url;
use uuid::Uuid;

fn url() -> String {
    env::var("BOSUN_TEST_DATABASE_URL").expect("BOSUN_TEST_DATABASE_URL")
}

async fn execute(server: &DatabaseConnection, sql: &str) {
    server
        .execute_unprepared(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

async fn exists(server: &DatabaseConnection, database: &str) -> bool {
    server
        .query_one(Statement::from_string(
            server.get_database_backend(),
            format!("SELECT count(*) AS count FROM pg_database WHERE datname = '{database}'"),
        ))
        .await
        .expect("a query")
        .expect("a row")
        .try_get::<i64>("", "count")
        .expect("a count")
        == 1
}

#[tokio::test]
async fn leftovers_of_ended_processes_are_swept_and_those_of_running_ones_kept() {
    let server = Database::connect(url()).await.expect("the server");
    let ended = Uuid::now_v7().simple().to_string();
    let running = Uuid::now_v7().simple().to_string();

    let mut session = Url::parse(&url()).expect("a URL");
    session
        .query_pairs_mut()
        .append_pair("application_name", &format!("bosun_test_{running}"));
    let holder = Database::connect(session.to_string())
        .await
        .expect("a session of a running process");
    let held = holder
        .get_postgres_connection_pool()
        .acquire()
        .await
        .expect("a held connection");

    let swept = [
        format!("bosun_test_{ended}_template"),
        format!("bosun_test_{ended}_3"),
    ];
    let kept = [
        format!("bosun_test_{running}_template"),
        format!("bosun_test_{running}_1"),
        format!("bosun_test_{ended}_spare"),
    ];
    for database in swept.iter().chain(kept.iter()) {
        execute(
            &server,
            &format!(r#"CREATE DATABASE "{database}" TEMPLATE template0"#),
        )
        .await;
    }

    let database = TestDatabase::migrated().await;
    assert!(exists(&server, database.name()).await);
    for name in &swept {
        assert!(!exists(&server, name).await, "{name} was not swept");
    }
    for name in &kept {
        assert!(exists(&server, name).await, "{name} was swept");
    }

    drop(held);
    holder.close().await.expect("the session ends");
    for name in &kept {
        execute(
            &server,
            &format!(r#"DROP DATABASE IF EXISTS "{name}" WITH (FORCE)"#),
        )
        .await;
    }
}
