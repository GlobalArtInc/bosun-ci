use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use bosun_testkit::TestDatabase;
use sea_orm::ConnectionTrait;

use crate::strings;

const TABLES: [&str; 28] = [
    "organisation",
    "project",
    "user",
    "api_token",
    "workflow",
    "workflow_revision",
    "run",
    "attempt",
    "job",
    "execution",
    "step",
    "run_event",
    "pool",
    "agent",
    "registration_token",
    "control_message",
    "log_chunk",
    "log_object",
    "idempotency_key",
    "schema_version",
    "session",
    "sign_in_failure",
    "team",
    "team_member",
    "role",
    "role_binding",
    "pool_access",
    "label_alias",
];

const LEDGER: [&str; 2] = ["migrations", "migrations_checksum"];

fn files() -> Vec<(String, String)> {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut files: Vec<(String, String)> = fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        .map(|entry| {
            let path = entry.expect("a directory entry").path();
            let name = path
                .file_name()
                .expect("a file name")
                .to_string_lossy()
                .into_owned();
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            (name, text)
        })
        .collect();
    files.sort();
    files
}

async fn tables<C: ConnectionTrait>(db: &C) -> BTreeSet<String> {
    strings(
        db,
        "SELECT table_name::text AS value FROM information_schema.tables \
         WHERE table_schema = 'bosun'",
    )
    .await
    .into_iter()
    .collect()
}

async fn snapshot<C: ConnectionTrait>(db: &C) -> Vec<String> {
    let mut snapshot = Vec::new();
    for sql in [
        "SELECT concat_ws(' ', table_name, column_name, data_type, is_nullable, column_default) \
         AS value FROM information_schema.columns WHERE table_schema = 'bosun' \
         ORDER BY table_name, column_name",
        "SELECT indexdef AS value FROM pg_indexes WHERE schemaname = 'bosun' ORDER BY indexdef",
        "SELECT conrelid::regclass::text || ' ' || pg_get_constraintdef(oid) AS value \
         FROM pg_constraint WHERE connamespace = 'bosun'::regnamespace ORDER BY 1",
        "SELECT name::text AS value FROM bosun.migrations ORDER BY id",
        "SELECT concat_ws(' ', id, name, trust) AS value FROM bosun.pool ORDER BY id",
        "SELECT concat_ws(' ', pool_id, organisation_id, project_id) AS value \
         FROM bosun.pool_access ORDER BY id",
    ] {
        snapshot.extend(strings(db, sql).await);
    }
    snapshot
}

fn destructive(sql: &str) -> Vec<&'static str> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    for character in sql.chars().chain([' ']) {
        if character.is_ascii_alphanumeric() || character == '_' {
            word.push(character.to_ascii_uppercase());
            continue;
        }
        if !word.is_empty() {
            tokens.push(std::mem::take(&mut word));
        }
        if character == ';' {
            tokens.push(";".to_owned());
        }
    }
    let mut found = Vec::new();
    let mut altering = false;
    for token in &tokens {
        match token.as_str() {
            ";" => altering = false,
            "ALTER" => altering = true,
            "DROP" if altering => found.push("ALTER … DROP"),
            "DROP" => found.push("DROP"),
            "TYPE" if altering => found.push("ALTER … TYPE"),
            _ => {}
        }
    }
    found
}

#[tokio::test]
async fn migrations_apply_to_an_empty_database_and_a_second_application_is_a_no_op() {
    let database = TestDatabase::empty().await;
    let db = database.connection();
    assert!(tables(db).await.is_empty(), "the database is not empty");

    let applied = bosun_db::migrate(db).await.expect("the first application");
    let carried = files()
        .iter()
        .filter(|(name, _)| name.ends_with(".up.sql"))
        .count();
    assert_eq!(applied.len(), carried, "applied {applied:?}");

    let schema = snapshot(db).await;
    let again = bosun_db::migrate(db).await.expect("the second application");
    assert_eq!(again, Vec::<String>::new());
    assert_eq!(snapshot(db).await, schema);
}

#[tokio::test]
async fn the_schema_holds_the_tables_of_m0_and_the_ledger() {
    let database = TestDatabase::migrated().await;
    let expected: BTreeSet<String> = TABLES
        .iter()
        .chain(LEDGER.iter())
        .map(|table| table.to_string())
        .collect();
    assert_eq!(tables(database.connection()).await, expected);
}

#[tokio::test]
async fn every_foreign_key_column_leads_an_index() {
    let database = TestDatabase::migrated().await;
    let unindexed = strings(
        database.connection(),
        "SELECT c.conrelid::regclass::text || '.' || a.attname AS value \
         FROM pg_constraint c \
         JOIN pg_attribute a ON a.attrelid = c.conrelid AND a.attnum = c.conkey[1] \
         WHERE c.contype = 'f' AND c.connamespace = 'bosun'::regnamespace \
         AND NOT EXISTS (SELECT 1 FROM pg_index i \
                         WHERE i.indrelid = c.conrelid AND i.indkey[0] = c.conkey[1]) \
         ORDER BY 1",
    )
    .await;
    assert!(
        unindexed.is_empty(),
        "foreign keys without an index: {unindexed:?}"
    );
}

#[test]
fn no_migration_file_drops_or_retypes_anything() {
    let files = files();
    assert!(!files.is_empty(), "no migration files");
    for (name, text) in files {
        assert!(name.ends_with(".up.sql"), "{name} is not an up migration");
        let found = destructive(&text);
        assert!(found.is_empty(), "{name} contains {found:?}");
    }
}

#[test]
fn the_additivity_check_finds_drop_and_retyping() {
    for (sql, expected) in [
        ("DROP TABLE job;", vec!["DROP"]),
        ("drop index concurrently job_attempt_id;", vec!["DROP"]),
        ("ALTER TABLE job DROP COLUMN name;", vec!["ALTER … DROP"]),
        (
            "ALTER TABLE job ALTER COLUMN priority TYPE bigint;",
            vec!["ALTER … TYPE"],
        ),
        (
            "alter table job alter column priority set data type bigint;",
            vec!["ALTER … TYPE"],
        ),
        (
            "ALTER TABLE job ADD COLUMN note text; CREATE TYPE size AS ENUM ('s');",
            vec![],
        ),
        (
            "ALTER TABLE workflow ADD FOREIGN KEY (revision_id) REFERENCES workflow_revision (id);",
            vec![],
        ),
    ] {
        assert_eq!(destructive(sql), expected, "{sql}");
    }
}

fn major() -> i32 {
    env!("CARGO_PKG_VERSION_MAJOR")
        .parse()
        .expect("a numeric major version")
}

#[tokio::test]
async fn two_migrations_started_at_once_on_an_empty_database_apply_each_file_once() {
    let database = TestDatabase::empty().await;
    let other = bosun_db::connect(&database.url(), 2)
        .await
        .expect("a second pool");
    let (left, right) = tokio::join!(
        bosun_db::migrate(database.connection()),
        bosun_db::migrate(&other)
    );
    let left = left.expect("the first migration");
    let right = right.expect("the second migration");
    let carried = files().len();
    assert!(
        (left.len(), right.len()) == (carried, 0) || (left.len(), right.len()) == (0, carried),
        "applied {left:?} and {right:?}"
    );
    let ledger = strings(
        database.connection(),
        "SELECT name::text AS value FROM bosun.migrations ORDER BY id",
    )
    .await;
    assert_eq!(ledger.len(), carried, "ledger {ledger:?}");
    other.close().await.expect("the second pool closes");
}

#[tokio::test]
async fn a_migrated_schema_records_the_major_version_that_migrated_it() {
    let database = TestDatabase::migrated().await;
    let majors = strings(
        database.connection(),
        "SELECT major::text AS value FROM bosun.schema_version",
    )
    .await;
    assert_eq!(majors, vec![major().to_string()]);
}

#[tokio::test]
async fn migrate_refuses_a_schema_of_a_newer_major_version() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let newer = major() + 1;
    db.execute_unprepared(&format!(
        "INSERT INTO bosun.schema_version (major) VALUES ({newer})"
    ))
    .await
    .expect("a newer major");
    match bosun_db::migrate(db).await {
        Err(bosun_db::Error::NewerSchema { found, supported }) => {
            assert_eq!((found, supported), (newer, major()));
        }
        other => panic!("migrate returned {other:?}"),
    }
}

#[tokio::test]
async fn migrate_accepts_a_schema_that_a_newer_minor_version_migrated() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    db.execute_unprepared(
        "CREATE TABLE bosun.from_a_newer_minor (id uuid PRIMARY KEY); \
         INSERT INTO bosun.migrations (\"timestamp\", name) \
         VALUES (4102444800000, 'FromANewerMinor4102444800000')",
    )
    .await
    .expect("the migration of a newer minor version");
    assert_eq!(
        bosun_db::migrate(db).await.expect("the migration"),
        Vec::<String>::new()
    );
}
