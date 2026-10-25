use std::net::SocketAddr;

use bosun_testkit::TestDatabase;
use bosun_testkit::agent::FakeAgent;
use serde_json::{Value, json};
use url::Url;

use crate::{CONFIG, Launch, client, errors, eventually, owner};

type Refused = (
    &'static str,
    &'static [(&'static str, Option<&'static str>)],
    &'static str,
);

const S3_WITHOUT_CREDENTIALS: &str =
    "[store]\nkind = \"s3\"\nendpoint = \"http://127.0.0.1:1\"\nbucket = \"bosun\"\n";

const NODE_ARCHIVE: &str = "the packed Node runtime for linux-x64";

const NODE_ARCHIVE_SHA256: &str =
    "07112f82d32127fcbf02e40859adb8137d01c340ba0097ff1f16b2ce63aae091";

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
    let cases: [Refused; 37] = [
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
            "[server]\npublic_url = \"ci.example.org\"\n",
            &[],
            "the setting server.public_url (BOSUN_SERVER_PUBLIC_URL) is `ci.example.org`, which \
             is not an address",
        ),
        (
            "[server]\nlisten = \"127.0.0.1:0\"\n",
            &[(
                "BOSUN_SERVER_PUBLIC_URL",
                Some("https://ci.example.org/?next=1"),
            )],
            "the setting server.public_url (BOSUN_SERVER_PUBLIC_URL) is \
             `https://ci.example.org/?next=1`, which is not an address without a user, a query or \
             a fragment",
        ),
        (
            "[server]\nlisten = \"127.0.0.1:0\"\n",
            &[("BOSUN_STORE_PATH_STYLE", Some("yes"))],
            "the setting store.path_style (BOSUN_STORE_PATH_STYLE) is `yes`, which is neither \
             `true` nor `false`",
        ),
        (
            "[server]\nlisten = \"127.0.0.1:0\"\n",
            &[("BOSUN_SERVER_DATA_DIR", Some(""))],
            "the setting server.data_dir (BOSUN_SERVER_DATA_DIR) is empty",
        ),
        (
            "[server]\ntrusted_proxies = [\"10.0.0.0/8\", \"proxy.example.org\"]\n",
            &[],
            "the setting server.trusted_proxies (BOSUN_SERVER_TRUSTED_PROXIES) holds \
             `proxy.example.org`, which is neither an address nor a network such as 10.0.0.0/8",
        ),
        (
            "[server]\nlisten = \"127.0.0.1:0\"\n",
            &[(
                "BOSUN_SERVER_TRUSTED_PROXIES",
                Some("127.0.0.1, 10.0.0.0/33"),
            )],
            "the setting server.trusted_proxies (BOSUN_SERVER_TRUSTED_PROXIES) holds \
             `10.0.0.0/33`, which is neither an address nor a network",
        ),
        (
            "[github]\napp_id = 4242\n",
            &[],
            "the setting github.private_key_file (BOSUN_GITHUB_PRIVATE_KEY_FILE) is not set, and \
             github.app_id is: the GitHub App needs app_id, private_key_file and \
             webhook_secret_file together",
        ),
        (
            "[github]\napi_url = \"https://api.github.com\"\n",
            &[],
            "the setting github.app_id (BOSUN_GITHUB_APP_ID) is not set, and github.api_url is",
        ),
        (
            "[server]\nlisten = \"127.0.0.1:0\"\n",
            &[("BOSUN_GITHUB_APP_ID", Some("bosun"))],
            "the setting github.app_id (BOSUN_GITHUB_APP_ID) is `bosun`, which is not the number \
             of a GitHub App",
        ),
        (
            "[github]\nserver_url = \"github.com\"\n",
            &[],
            "the setting github.server_url (BOSUN_GITHUB_SERVER_URL) is `github.com`, which is \
             not an address",
        ),
        (
            "[server]\nlisten = \"127.0.0.1:0\"\n",
            &[("BOSUN_GITHUB_API_URL", Some("ftp://api.github.com"))],
            "the setting github.api_url (BOSUN_GITHUB_API_URL) is `ftp://api.github.com`, which \
             is not an http or https address with a host",
        ),
        (
            "[limits]\nfailed_sign_ins = 0\n",
            &[],
            "the setting limits.failed_sign_ins (BOSUN_LIMITS_FAILED_SIGN_INS) is 0, and a \
             sign-in is locked only after at least one failure",
        ),
        (
            "[server]\nlisten = \"127.0.0.1:0\"\n",
            &[("BOSUN_LIMITS_FAILED_SIGN_INS", Some("many"))],
            "the setting limits.failed_sign_ins (BOSUN_LIMITS_FAILED_SIGN_INS) is `many`, which \
             is not a number of failed sign-ins",
        ),
        (
            "[tools]\nnode16 = \"16\"\n",
            &[],
            "the setting tools.node16 (BOSUN_TOOLS_NODE16) is `16`, which is not a version of \
             Node such as 24.22.1",
        ),
        (
            "[server]\nlisten = \"127.0.0.1:0\"\n",
            &[("BOSUN_TOOLS_NODE_UPSTREAM", Some("ftp://nodejs.org/dist"))],
            "the setting tools.node_upstream (BOSUN_TOOLS_NODE_UPSTREAM) is \
             `ftp://nodejs.org/dist`, which is not an http or https address with a host, or a \
             file address of a directory",
        ),
        (
            "[tools]\nnode_musl_upstream = \"https://builds.example.org/node?mirror=1\"\n",
            &[],
            "the setting tools.node_musl_upstream (BOSUN_TOOLS_NODE_MUSL_UPSTREAM) is \
             `https://builds.example.org/node?mirror=1`, which is not an address without a user, \
             a password, a query or a fragment",
        ),
        (
            "[tools]\nnode_upstream = \"https://:secret@mirror.example.org/dist\"\n",
            &[],
            "the setting tools.node_upstream (BOSUN_TOOLS_NODE_UPSTREAM) is \
             `https://:secret@mirror.example.org/dist`, which is not an address without a user, a \
             password, a query or a fragment",
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

async fn runtime(address: SocketAddr, path: &str, credential: Option<&str>) -> (u16, Vec<u8>) {
    let mut request = client().get(format!("http://{address}{path}"));
    if let Some(credential) = credential {
        request = request.bearer_auth(credential);
    }
    let response = request
        .send()
        .await
        .unwrap_or_else(|error| panic!("GET {path}: {error}"));
    let status = response.status().as_u16();
    let body = response.bytes().await.expect("a body");
    (status, body.to_vec())
}

fn refusal(body: &[u8]) -> (String, String) {
    let error: Value = serde_json::from_slice(body).unwrap_or_else(|error| {
        panic!(
            "a body that is not an error envelope ({error}): {}",
            String::from_utf8_lossy(body)
        )
    });
    let field = |name: &str| error["error"][name].as_str().unwrap_or_default().to_owned();
    (field("code"), field("message"))
}

#[tokio::test]
async fn the_runtime_map_takes_versions_from_the_file_and_the_variables_and_serves_those() {
    let database = TestDatabase::migrated().await;
    let owner = owner(&database.url(), "roman").await;
    let launch = Launch::serve(&database.url());
    for version in ["16.20.2", "20.19.5", "24.22.1"] {
        let archive = format!("node-v{version}-linux-x64.tar.xz");
        launch.file(&format!("upstream/v{version}/{archive}"), NODE_ARCHIVE);
        launch.file(
            &format!("upstream/v{version}/SHASUMS256.txt"),
            &format!("{NODE_ARCHIVE_SHA256}  {archive}\n"),
        );
    }
    let upstream =
        Url::from_directory_path(launch.data_dir().join("upstream")).expect("a file address");
    let mut server = launch
        .config(&format!(
            "{CONFIG}\n[tools]\nnode16 = \"16.20.2\"\nnode20 = \"20.0.0\"\nnode_upstream = \
             \"{upstream}\"\n"
        ))
        .env("BOSUN_TOOLS_NODE20", "20.19.5")
        .spawn();
    let address = server.listening().await;
    eventually(address, "/readyz", 200).await;

    for path in [
        "/_runtime/tools/node/16.20.2/linux-x64.tar.xz".to_owned(),
        format!(
            "/_runtime/actions/actions/checkout/{}.tar.gz",
            "a".repeat(40)
        ),
    ] {
        let (status, body) = runtime(address, &path, None).await;
        assert_eq!(status, 401, "{path}: {}", String::from_utf8_lossy(&body));
        assert_eq!(
            refusal(&body),
            (
                "unauthenticated".to_owned(),
                "the request carries no agent credential".to_owned()
            ),
            "{path}"
        );
    }

    let response = client()
        .post(format!("http://{address}/api/v1/registration-tokens"))
        .bearer_auth(&owner)
        .json(&json!({ "pool": "default", "reusable": false, "expires_at": null }))
        .send()
        .await
        .expect("a registration token");
    assert_eq!(response.status().as_u16(), 201);
    let token: Value = response.json().await.expect("a JSON body");
    let agent = FakeAgent::register(
        address,
        token["token"].as_str().expect("a token"),
        "runner1-1",
        &[],
    )
    .await;
    for version in ["16.20.2", "20.19.5", "24.22.1"] {
        let path = format!("/_runtime/tools/node/{version}/linux-x64.tar.xz");
        let (status, body) = runtime(address, &path, Some(agent.credential())).await;
        assert_eq!(status, 200, "{path}: {}", String::from_utf8_lossy(&body));
        assert_eq!(body, NODE_ARCHIVE.as_bytes(), "{path}");
    }
    for version in ["20.0.0", "16.20.3"] {
        let path = format!("/_runtime/tools/node/{version}/linux-x64.tar.xz");
        let (status, body) = runtime(address, &path, Some(agent.credential())).await;
        assert_eq!(status, 404, "{path}: {}", String::from_utf8_lossy(&body));
        assert_eq!(
            refusal(&body),
            (
                "not_found".to_owned(),
                format!(
                    "the server serves no Node {version}, since its runtime map names no action \
                     runtime that runs on it"
                )
            )
        );
    }
}
