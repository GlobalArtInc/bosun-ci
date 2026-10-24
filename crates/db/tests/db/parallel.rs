use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bosun_db::entity::{organisation, run};
use bosun_testkit::TestDatabase;
use sea_orm::{
    ConnectionTrait, Database, EntityTrait, PaginatorTrait, Statement, TransactionTrait,
};

use crate::rows;

const WRITER: &str = "BOSUN_TEST_WRITER";
const MEETING: &str = "BOSUN_TEST_MEETING";
const WRITERS: [&str; 2] = ["left", "right"];

fn template_of(database: &str) -> String {
    let (owner, _) = database.rsplit_once('_').expect("a numbered database");
    format!("{owner}_template")
}

async fn existing(names: &[String]) -> Vec<String> {
    let url = env::var("BOSUN_TEST_DATABASE_URL").expect("BOSUN_TEST_DATABASE_URL");
    let server = Database::connect(url).await.expect("the server");
    let rows = server
        .query_all(Statement::from_string(
            server.get_database_backend(),
            "SELECT datname AS name FROM pg_database",
        ))
        .await
        .expect("the databases");
    let present: Vec<String> = rows
        .into_iter()
        .map(|row| row.try_get::<String>("", "name").expect("a name"))
        .collect();
    names
        .iter()
        .filter(|name| present.contains(name))
        .cloned()
        .collect()
}

async fn meet(meeting: &Path) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !WRITERS.iter().all(|writer| meeting.join(writer).exists()) {
        assert!(
            Instant::now() < deadline,
            "the other writer did not arrive within a minute"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn two_tests_that_write_the_same_rows_run_in_parallel_without_interfering() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("a clock after 1970")
        .as_nanos();
    let meeting = env::temp_dir().join(format!("bosun-db-meeting-{}-{stamp}", std::process::id()));
    fs::create_dir_all(&meeting).expect("the meeting directory");

    let binary = env::current_exe().expect("the test binary");
    let children: Vec<_> = WRITERS
        .iter()
        .map(|writer| {
            Command::new(&binary)
                .args([
                    "parallel::writer",
                    "--exact",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(WRITER, writer)
                .env(MEETING, &meeting)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("a writer process")
        })
        .collect();

    let mut databases = Vec::new();
    for (writer, child) in WRITERS.iter().zip(children) {
        let output = child.wait_with_output().expect("the writer's output");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "the {writer} writer failed:\n{stdout}\n{stderr}"
        );
        assert!(
            stdout.contains("1 passed"),
            "the {writer} writer ran no test:\n{stdout}"
        );
        let database = stdout
            .split("database ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap_or_else(|| panic!("the {writer} writer named no database:\n{stdout}"));
        databases.push(database.to_owned());
    }
    fs::remove_dir_all(&meeting).expect("the meeting directory is removed");

    assert_ne!(databases[0], databases[1]);
    assert_ne!(template_of(&databases[0]), template_of(&databases[1]));
    let mut created = databases.clone();
    created.extend(databases.iter().map(|database| template_of(database)));
    assert_eq!(existing(&created).await, Vec::<String>::new());
}

#[tokio::test]
#[ignore = "two_tests_that_write_the_same_rows_run_in_parallel_without_interfering starts it"]
async fn writer() {
    let (Ok(writer), Ok(meeting)) = (env::var(WRITER), env::var(MEETING)) else {
        return;
    };
    let meeting = PathBuf::from(meeting);
    let database = TestDatabase::migrated().await;
    println!("database {}", database.name());

    let transaction = database.connection().begin().await.expect("a transaction");
    let written = rows::write(&transaction, &writer).await;
    fs::write(meeting.join(&writer), b"").expect("the arrival");
    meet(&meeting).await;
    transaction.commit().await.expect("a commit");

    let db = database.connection();
    assert_eq!(rows::read(db).await, written);
    assert_eq!(
        organisation::Entity::find()
            .count(db)
            .await
            .expect("a count"),
        1
    );
    assert_eq!(run::Entity::find().count(db).await.expect("a count"), 1);
    assert_eq!(written.run.global_number, 1);
}
