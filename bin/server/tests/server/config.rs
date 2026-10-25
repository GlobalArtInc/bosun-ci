use bosun_testkit::TestDatabase;

use crate::{CONFIG, Launch, errors, eventually};

type Refused = (
    &'static str,
    &'static [(&'static str, Option<&'static str>)],
    &'static str,
);

const S3_WITHOUT_CREDENTIALS: &str =
    "[store]\nkind = \"s3\"\nendpoint = \"http://127.0.0.1:1\"\nbucket = \"bosun\"\n";

#[tokio::test]
async fn variables_override_the_configuration_file() {
    let database = TestDatabase::migrated().await;
    let mut server = Launch::serve(&database.url())
        .config(
            "[server]\nlisten = \"127.0.0.1:1\"\n\n[database]\n\
             url = \"postgres://nobody@127.0.0.1:1/nowhere\"\nmax_connections = 2\n",
        )
        .env("BOSUN_SERVER_LISTEN", "127.0.0.1:0")
        .spawn();
    let address = server.listening().await;
    let body = eventually(address, "/readyz", 200).await;
    assert_eq!(body["status"], "ready");
}

#[tokio::test]
async fn a_configuration_the_server_cannot_use_is_refused_with_the_setting_at_fault() {
    let cases: [Refused; 21] = [
        (
            "[database]\nurll = \"postgres://bosun@db/bosun\"\n",
            &[],
            "unknown field `urll`",
        ),
        (
            "[server]\nlisten = \"127.0.0.1:0\"\n",
            &[("BOSUN_DATABASE_URL", None)],
            "the setting database.url (BOSUN_DATABASE_URL) is not set",
        ),
        (
            "[server]\nlisten = \"everywhere\"\n",
            &[],
            "the setting server.listen (BOSUN_SERVER_LISTEN) is `everywhere`",
        ),
        (
            "[database]\nmax_connections = 1\n",
            &[],
            "the setting database.max_connections (BOSUN_DATABASE_MAX_CONNECTIONS) is 1",
        ),
        (
            "[server]\nlisten = \"127.0.0.1:0\"\n",
            &[("BOSUN_DATABASE_MAX_CONNECTIONS", Some("many"))],
            "the setting database.max_connections (BOSUN_DATABASE_MAX_CONNECTIONS) is `many`",
        ),
        (
            "[server]\nlisten = \"127.0.0.1:0\"\n",
            &[("BOSUN_BOOTSTRAP_ADMIN", Some("Roman X"))],
            "the setting bootstrap.admin (BOSUN_BOOTSTRAP_ADMIN) is `Roman X`, which is not a \
             login: a login is 1 to 64",
        ),
        (
            "[bootstrap]\nadmin = \"\"\n",
            &[],
            "the setting bootstrap.admin (BOSUN_BOOTSTRAP_ADMIN) is ``, which is not a login",
        ),
        (
            "[agents]\nheartbeat_interval = \"soon\"\n",
            &[],
            "the setting agents.heartbeat_interval (BOSUN_AGENTS_HEARTBEAT_INTERVAL) is `soon`, \
             which is not a duration: a duration is a whole number followed by ms, s, m or h",
        ),
        (
            "[agents]\nheartbeat_interval = \"1m\"\n",
            &[],
            "the setting agents.offline_after (BOSUN_AGENTS_OFFLINE_AFTER) is 60s, and an agent \
             that sends a heartbeat every 60s must not be offline before its next one",
        ),
        (
            "[server]\nlisten = \"127.0.0.1:0\"\n",
            &[("BOSUN_LIMITS_AGENT_GRACE", Some("0s"))],
            "the setting limits.agent_grace (BOSUN_LIMITS_AGENT_GRACE) is `0s`, which is not a \
             duration",
        ),
        (
            "[agents]\nassignment_lease = \"0s\"\n",
            &[],
            "the setting agents.assignment_lease (BOSUN_AGENTS_ASSIGNMENT_LEASE) is `0s`, which \
             is not a duration",
        ),
        (
            "[server]\nlisten = \"127.0.0.1:0\"\n",
            &[("BOSUN_LIMITS_MAX_QUEUE_TIME", Some("forever"))],
            "the setting limits.max_queue_time (BOSUN_LIMITS_MAX_QUEUE_TIME) is `forever`, \
             which is not a duration",
        ),
        (
            "[store]\nkind = \"gcs\"\n",
            &[],
            "the setting store.kind (BOSUN_STORE_KIND) is `gcs`, and the kind of object store is \
             `directory` or `s3`",
        ),
        (
            "[store]\nkind = \"s3\"\nbucket = \"bosun\"\n",
            &[("BOSUN_STORE_PATH", None)],
            "the setting store.endpoint (BOSUN_STORE_ENDPOINT) is not set, and the kind `s3` \
             needs it",
        ),
        (
            "[store]\nkind = \"s3\"\nendpoint = \"http://127.0.0.1:1\"\n",
            &[("BOSUN_STORE_PATH", None)],
            "the setting store.bucket (BOSUN_STORE_BUCKET) is not set, and the kind `s3` needs it",
        ),
        (
            S3_WITHOUT_CREDENTIALS,
            &[("BOSUN_STORE_PATH", None)],
            "the setting store.access_key_id (BOSUN_STORE_ACCESS_KEY_ID) is not set, and neither \
             are AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, so the kind `s3` has no credentials",
        ),
        (
            S3_WITHOUT_CREDENTIALS,
            &[
                ("BOSUN_STORE_PATH", None),
                ("AWS_ACCESS_KEY_ID", Some("bosun")),
            ],
            "the setting store.access_key_id (BOSUN_STORE_ACCESS_KEY_ID) is not set, and \
             AWS_ACCESS_KEY_ID is set without AWS_SECRET_ACCESS_KEY",
        ),
        (
            S3_WITHOUT_CREDENTIALS,
            &[
                ("BOSUN_STORE_PATH", None),
                ("BOSUN_STORE_ACCESS_KEY_ID", Some("bosun")),
            ],
            "the setting store.secret_access_key (BOSUN_STORE_SECRET_ACCESS_KEY) is not set, and \
             store.access_key_id is: the two are set together",
        ),
        (
            S3_WITHOUT_CREDENTIALS,
            &[
                ("BOSUN_STORE_ACCESS_KEY_ID", Some("bosun")),
                ("BOSUN_STORE_SECRET_ACCESS_KEY", Some("bosun-secret")),
            ],
            "the setting store.path (BOSUN_STORE_PATH) applies to the kind `directory`, and \
             store.kind is `s3`",
        ),
        (
            "[store]\nbucket = \"bosun\"\n",
            &[],
            "the setting store.bucket (BOSUN_STORE_BUCKET) applies to the kind `s3`, and \
             store.kind is `directory`",
        ),
        (
            "[server]\nlisten = \"127.0.0.1:0\"\n",
            &[("BOSUN_STORE_PATH_STYLE", Some("yes"))],
            "the setting store.path_style (BOSUN_STORE_PATH_STYLE) is `yes`, which is neither \
             `true` nor `false`",
        ),
    ];
    for (config, variables, expected) in cases {
        let mut launch = Launch::serve("postgres://bosun@127.0.0.1:1/bosun").config(config);
        for (name, value) in variables {
            launch = match value {
                Some(value) => launch.env(name, value),
                None => launch.env_remove(name),
            };
        }
        let path = launch.config_path();
        let (status, lines) = launch.spawn().exit().await;
        assert!(!status.success(), "{config}: {lines:#?}");
        let errors = errors(&lines);
        assert!(
            errors.iter().any(|error| error.contains(expected)),
            "expected an error containing `{expected}`: {lines:#?}"
        );
        if expected.starts_with("unknown field") {
            assert!(
                errors
                    .iter()
                    .any(|error| error.contains(&path.display().to_string())),
                "the error does not name the file: {lines:#?}"
            );
        }
    }
}

#[tokio::test]
async fn an_explicit_configuration_file_that_does_not_exist_is_refused() {
    let launch = Launch::serve("postgres://bosun@127.0.0.1:1/bosun");
    let path = launch.config_path();
    std::fs::remove_file(&path).expect("the configuration file goes away");
    let (status, lines) = launch.spawn().exit().await;
    assert!(!status.success(), "{lines:#?}");
    assert!(
        errors(&lines).iter().any(|error| error.contains(&format!(
            "could not read the configuration {}",
            path.display()
        ))),
        "{lines:#?}"
    );
}

#[tokio::test]
async fn the_object_store_is_the_directory_the_configuration_names() {
    let database = TestDatabase::migrated().await;
    let directory = tempfile::tempdir().expect("a directory for the store");
    let store = directory.path().join("objects-here");
    let mut server = Launch::serve(&database.url())
        .config(&format!(
            "{CONFIG}\n[store]\nkind = \"directory\"\npath = \"{}\"\n",
            store.display()
        ))
        .env_remove("BOSUN_STORE_PATH")
        .spawn();
    let address = server.listening().await;
    eventually(address, "/readyz", 200).await;
    assert!(
        store.join("objects").is_dir(),
        "the store was not opened there"
    );
}
