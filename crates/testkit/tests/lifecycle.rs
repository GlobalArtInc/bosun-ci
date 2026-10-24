use std::env;

use bosun_testkit::TestDatabase;
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, Statement};

async fn server() -> DatabaseConnection {
    let url = env::var("BOSUN_TEST_DATABASE_URL").expect("BOSUN_TEST_DATABASE_URL");
    Database::connect(url).await.expect("the server")
}

async fn count<C: ConnectionTrait>(db: &C, sql: &str) -> i64 {
    db.query_one(Statement::from_string(db.get_database_backend(), sql))
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .expect("a row")
        .try_get::<i64>("", "count")
        .expect("a count")
}

async fn exists(server: &DatabaseConnection, database: &str) -> bool {
    count(
        server,
        &format!("SELECT count(*) AS count FROM pg_database WHERE datname = '{database}'"),
    )
    .await
        == 1
}

fn template_of(database: &str) -> String {
    let (owner, _) = database.rsplit_once('_').expect("a numbered database");
    format!("{owner}_template")
}

#[tokio::test]
async fn each_test_gets_its_own_database_and_the_template_goes_with_the_last_one() {
    let server = server().await;
    let (first, second, empty) = tokio::join!(
        TestDatabase::migrated(),
        TestDatabase::migrated(),
        TestDatabase::empty()
    );
    let names = [
        first.name().to_owned(),
        second.name().to_owned(),
        empty.name().to_owned(),
    ];
    assert_ne!(names[0], names[1]);
    assert_ne!(names[0], names[2]);
    assert_ne!(names[1], names[2]);
    let template = template_of(&names[0]);
    assert!(names.iter().all(|name| template_of(name) == template));
    assert!(exists(&server, &template).await);
    for name in &names {
        assert!(exists(&server, name).await, "{name} does not exist");
    }

    for database in [&first, &second] {
        let pools = count(
            database.connection(),
            "SELECT count(*) AS count FROM pool WHERE name = 'default'",
        )
        .await;
        assert_eq!(pools, 1, "{} is not migrated", database.name());
    }
    let tables = count(
        empty.connection(),
        "SELECT count(*) AS count FROM information_schema.tables \
         WHERE table_schema NOT IN ('pg_catalog', 'information_schema')",
    )
    .await;
    assert_eq!(tables, 0, "{} is not empty", empty.name());

    drop(first);
    assert!(!exists(&server, &names[0]).await);
    assert!(exists(&server, &template).await);
    drop(second);
    assert!(!exists(&server, &names[1]).await);
    assert!(!exists(&server, &template).await);
    drop(empty);
    assert!(!exists(&server, &names[2]).await);

    let again = TestDatabase::migrated().await;
    assert!(exists(&server, &template).await);
    let name = again.name().to_owned();
    drop(again);
    assert!(!exists(&server, &name).await);
    assert!(!exists(&server, &template).await);
}
