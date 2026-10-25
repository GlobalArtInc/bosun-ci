use bosun_blob::S3Settings;
use bosun_testkit::{TestBucket, TestDatabase};
use sea_orm::ConnectionTrait;

use crate::proxy::Proxy;
use crate::{KEK, Launch, errors, eventually, major, message, strings};

#[tokio::test]
async fn two_servers_started_at_once_against_an_empty_database_both_come_up_and_migrate_once() {
    let database = TestDatabase::empty().await;
    let mut first = Launch::serve(&database.url()).spawn();
    let mut second = Launch::serve(&database.url()).spawn();
    let (one, other) = tokio::join!(first.listening(), second.listening());
    eventually(one, "/readyz", 200).await;
    eventually(other, "/readyz", 200).await;

    let mut ledger = strings(
        database.connection(),
        "SELECT name::text AS value FROM bosun.migrations",
    )
    .await;
    assert!(!ledger.is_empty(), "no migration ran");

    first.terminate();
    second.terminate();
    let (first_status, first_lines) = first.exit().await;
    let (second_status, second_lines) = second.exit().await;
    assert!(first_status.success(), "{first_lines:#?}");
    assert!(second_status.success(), "{second_lines:#?}");

    let mut applied: Vec<String> = first_lines
        .iter()
        .chain(&second_lines)
        .filter(|line| message(line) == "applied a migration")
        .map(|line| line["migration"].as_str().unwrap_or_default().to_owned())
        .collect();
    applied.sort();
    ledger.sort();
    assert_eq!(
        applied, ledger,
        "each migration ran once, by one of the two"
    );
}

#[tokio::test]
async fn a_server_starts_against_a_schema_that_a_newer_minor_version_migrated() {
    let database = TestDatabase::migrated().await;
    database
        .connection()
        .execute_unprepared(
            "CREATE TABLE bosun.from_a_newer_minor (id uuid PRIMARY KEY); \
             INSERT INTO bosun.migrations (\"timestamp\", name) \
             VALUES (4102444800000, 'FromANewerMinor4102444800000')",
        )
        .await
        .expect("the migration of a newer minor version");

    let mut server = Launch::serve(&database.url()).spawn();
    let address = server.listening().await;
    let body = eventually(address, "/readyz", 200).await;
    assert_eq!(body["status"], "ready");
}

#[tokio::test]
async fn a_server_refuses_a_schema_of_a_newer_major_version_and_says_so() {
    let database = TestDatabase::migrated().await;
    let newer = major() + 1;
    database
        .connection()
        .execute_unprepared(&format!(
            "INSERT INTO bosun.schema_version (major) VALUES ({newer})"
        ))
        .await
        .expect("a newer major version");

    let (status, lines) = Launch::serve(&database.url()).spawn().exit().await;
    assert!(!status.success(), "{lines:#?}");
    let errors = errors(&lines);
    assert!(
        errors.iter().any(|error| error.contains(&format!(
            "the schema belongs to major version {newer} of Bosun and this server is major \
             version {}",
            major()
        ))),
        "{lines:#?}"
    );
    assert!(!lines.iter().any(|line| message(line) == "listening"));
}

#[tokio::test]
async fn a_server_refuses_to_start_without_a_well_formed_kek_and_says_why() {
    let generated =
        Launch::serve("postgres://bosun@127.0.0.1:1/bosun").env_remove("BOSUN_KEYS_KEK");
    let kek = generated.file("keys/kek", "a1b2\n");
    for (launch, expected) in [
        (
            generated,
            format!(
                "the KEK in {} is not 32 bytes written as 64 hexadecimal digits",
                kek.display()
            ),
        ),
        (
            Launch::serve("postgres://bosun@127.0.0.1:1/bosun").env("BOSUN_KEYS_KEK", "a1b2"),
            "the KEK in keys.kek (BOSUN_KEYS_KEK) is not 32 bytes written as 64 hexadecimal \
             digits"
                .to_owned(),
        ),
        (
            Launch::serve("postgres://bosun@127.0.0.1:1/bosun")
                .env("BOSUN_KEYS_KEK_FILE", "/nonexistent/bosun/kek"),
            "both keys.kek and keys.kek_file are set".to_owned(),
        ),
        (
            Launch::serve("postgres://bosun@127.0.0.1:1/bosun")
                .env_remove("BOSUN_KEYS_KEK")
                .env("BOSUN_KEYS_KEK_FILE", "/nonexistent/bosun/kek"),
            "could not read the KEK from /nonexistent/bosun/kek".to_owned(),
        ),
    ] {
        let (status, lines) = launch.spawn().exit().await;
        assert!(!status.success(), "{lines:#?}");
        assert!(
            errors(&lines).iter().any(|error| error.contains(&expected)),
            "expected an error containing `{expected}`: {lines:#?}"
        );
    }
}

#[tokio::test]
async fn a_server_takes_its_kek_from_a_file() {
    let database = TestDatabase::migrated().await;
    let launch = Launch::serve(&database.url()).env_remove("BOSUN_KEYS_KEK");
    let kek = launch.file("kek", &format!("{KEK}\n"));
    let mut server = launch
        .env("BOSUN_KEYS_KEK_FILE", &kek.to_string_lossy())
        .spawn();
    let address = server.listening().await;
    eventually(address, "/readyz", 200).await;
}

#[tokio::test]
async fn migrate_brings_an_empty_database_up_to_date_and_exits_without_a_kek() {
    let database = TestDatabase::empty().await;
    let (status, lines) = Launch::migrate(&database.url())
        .env_remove("BOSUN_KEYS_KEK")
        .spawn()
        .exit()
        .await;
    assert!(status.success(), "{lines:#?}");
    assert!(!lines.iter().any(|line| message(line) == "listening"));
    let ledger = strings(
        database.connection(),
        "SELECT name::text AS value FROM bosun.migrations",
    )
    .await;
    let applied = lines
        .iter()
        .filter(|line| message(line) == "applied a migration")
        .count();
    assert_eq!(applied, ledger.len());
    assert!(applied > 0);

    let (status, lines) = Launch::migrate(&database.url()).spawn().exit().await;
    assert!(status.success(), "{lines:#?}");
    assert!(
        !lines
            .iter()
            .any(|line| message(line) == "applied a migration"),
        "{lines:#?}"
    );
}

#[tokio::test]
async fn a_server_refuses_to_start_when_its_object_store_cannot_be_used() {
    let database = TestDatabase::migrated().await;
    let launch = Launch::serve(&database.url());
    let blocked = launch.file("store", "a file where the store should be");
    let (status, lines) = launch.spawn().exit().await;
    assert!(!status.success(), "{lines:#?}");
    let expected = format!("the object store in {} cannot be used", blocked.display());
    assert!(
        errors(&lines)
            .iter()
            .any(|error| error.starts_with(&expected)),
        "expected an error starting with `{expected}`: {lines:#?}"
    );
    assert!(!lines.iter().any(|line| message(line) == "listening"));
}

#[tokio::test]
async fn a_server_refuses_to_start_while_its_s3_store_is_unreachable_and_starts_once_it_answers() {
    let database = TestDatabase::migrated().await;
    let bucket = TestBucket::create().await;
    let proxy = Proxy::start(&bucket.settings().endpoint).await;
    let settings = S3Settings {
        endpoint: proxy.url(&bucket.settings().endpoint),
        ..bucket.settings().clone()
    };
    proxy.cut();
    let (status, lines) = Launch::serve(&database.url())
        .s3(&settings)
        .spawn()
        .exit()
        .await;
    assert!(!status.success(), "{lines:#?}");
    let expected = format!(
        "the object store in the bucket `{}` at {} cannot be used: the object store request on \
         `probes/start` failed",
        settings.bucket, settings.endpoint
    );
    assert!(
        errors(&lines)
            .iter()
            .any(|error| error.starts_with(&expected)),
        "expected an error starting with `{expected}`: {lines:#?}"
    );
    assert!(!lines.iter().any(|line| message(line) == "listening"));

    proxy.forward();
    let mut server = Launch::serve(&database.url()).s3(&settings).spawn();
    let address = server.listening().await;
    assert_eq!(eventually(address, "/readyz", 200).await["status"], "ready");
}

#[tokio::test]
async fn a_server_refuses_an_s3_store_it_cannot_use_and_says_why() {
    let database = TestDatabase::migrated().await;
    let bucket = TestBucket::create().await;
    let missing = format!("{}-missing", bucket.settings().bucket);
    for (settings, problem) in [
        (
            S3Settings {
                bucket: missing.clone(),
                ..bucket.settings().clone()
            },
            "the object store request on `probes/start` failed: the store answered 404 Not Found \
             with NoSuchBucket"
                .to_owned(),
        ),
        (
            S3Settings {
                secret_access_key: "not-the-secret".to_owned(),
                ..bucket.settings().clone()
            },
            "the object store request on `probes/start` failed: the store answered 403 Forbidden \
             with SignatureDoesNotMatch"
                .to_owned(),
        ),
        (
            S3Settings {
                path_style: false,
                ..bucket.settings().clone()
            },
            format!(
                "the S3 settings cannot be used: the endpoint `{}` is an IP address",
                bucket.settings().endpoint
            ),
        ),
    ] {
        let expected = format!(
            "the object store in the bucket `{}` at {} cannot be used: {problem}",
            settings.bucket, settings.endpoint
        );
        let (status, lines) = Launch::serve(&database.url())
            .s3(&settings)
            .spawn()
            .exit()
            .await;
        assert!(!status.success(), "{lines:#?}");
        assert!(
            errors(&lines)
                .iter()
                .any(|error| error.starts_with(&expected)),
            "expected an error starting with `{expected}`: {lines:#?}"
        );
    }
}

#[tokio::test]
async fn a_server_takes_the_credentials_of_its_s3_store_from_the_environment() {
    let database = TestDatabase::migrated().await;
    let bucket = TestBucket::create().await;
    let settings = bucket.settings();
    let mut server = Launch::serve(&database.url())
        .s3(settings)
        .env_remove("BOSUN_STORE_ACCESS_KEY_ID")
        .env_remove("BOSUN_STORE_SECRET_ACCESS_KEY")
        .env("AWS_ACCESS_KEY_ID", &settings.access_key_id)
        .env("AWS_SECRET_ACCESS_KEY", &settings.secret_access_key)
        .spawn();
    let address = server.listening().await;
    assert_eq!(eventually(address, "/readyz", 200).await["status"], "ready");
}
