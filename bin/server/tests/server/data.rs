use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use bosun_testkit::TestDatabase;
use serde_json::Value;

use crate::{Launch, eventually, me, message, owner, strings};

const GENERATED: &str = "generated the KEK, under which the server keeps every secret; back the \
                         file up outside this host, because every secret is lost without it";

const FIRST_OWNER: &str = "created the owner admin on the first start and wrote its token and \
                           password to the data directory";

fn mode(path: &Path) -> u32 {
    fs::metadata(path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        .permissions()
        .mode()
        & 0o777
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

async fn start(launch: Launch) -> Vec<Value> {
    let mut server = launch.spawn();
    let address = server.listening().await;
    eventually(address, "/readyz", 200).await;
    server.terminate();
    let (status, lines) = server.exit().await;
    assert!(status.success(), "{lines:#?}");
    lines
}

#[tokio::test]
async fn a_first_start_with_only_a_database_generates_the_kek_and_writes_the_owners_token() {
    let database = TestDatabase::empty().await;
    let data = tempfile::tempdir().expect("a data directory");
    let launch = || {
        Launch::serve(&database.url())
            .env_remove("BOSUN_KEYS_KEK")
            .env_remove("BOSUN_STORE_PATH")
            .env("BOSUN_SERVER_DATA_DIR", &data.path().to_string_lossy())
    };

    let mut server = launch().spawn();
    let address = server.listening().await;
    eventually(address, "/readyz", 200).await;
    let kek = data.path().join("keys/kek");
    let digits = read(&kek);
    assert_eq!(digits.len(), 65, "{digits:?}");
    assert!(digits.ends_with('\n'), "{digits:?}");
    assert!(
        digits
            .trim_end()
            .bytes()
            .all(|digit| digit.is_ascii_hexdigit()),
        "{digits:?}"
    );
    assert_eq!(mode(&kek), 0o600);
    assert_eq!(mode(&data.path().join("keys")), 0o700);
    let path = data.path().join("admin.token");
    let token = read(&path);
    assert_eq!(mode(&path), 0o600);
    let token = token
        .strip_suffix('\n')
        .expect("a token on a line of its own");
    let (status, body) = me(address, Some(&format!("Bearer {token}"))).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["user"]["login"], "admin");
    assert_eq!(body["user"]["owner"], true);
    assert!(data.path().join("store/objects").is_dir());
    server.terminate();
    let (status, lines) = server.exit().await;
    assert!(status.success(), "{lines:#?}");
    let generated = lines
        .iter()
        .find(|line| message(line) == GENERATED)
        .unwrap_or_else(|| panic!("no warning that the KEK was generated: {lines:#?}"));
    assert_eq!(generated["level"], "WARN");
    assert_eq!(generated["path"], kek.display().to_string());
    let created = lines
        .iter()
        .find(|line| message(line) == FIRST_OWNER)
        .unwrap_or_else(|| panic!("no line that the owner was created: {lines:#?}"));
    assert_eq!(created["login"], "admin");
    assert_eq!(created["token"], path.display().to_string());
    assert_eq!(
        created["password"],
        data.path().join("admin.password").display().to_string()
    );
    assert!(
        !lines.iter().any(|line| line.to_string().contains(token)),
        "the token reached the log: {lines:#?}"
    );

    let modified = fs::metadata(&path).and_then(|metadata| metadata.modified());
    let lines = start(launch()).await;
    assert_eq!(read(&kek), digits, "the KEK changed");
    assert_eq!(read(&path).trim_end(), token, "the token changed");
    assert_eq!(
        fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok(),
        modified.ok(),
        "the token file was written again"
    );
    assert!(
        !lines
            .iter()
            .any(|line| message(line) == GENERATED || message(line) == FIRST_OWNER),
        "{lines:#?}"
    );
    assert_eq!(
        strings(
            database.connection(),
            "SELECT login AS value FROM bosun.\"user\""
        )
        .await,
        ["admin"]
    );
}

#[tokio::test]
async fn a_start_with_a_configured_kek_generates_none() {
    let database = TestDatabase::migrated().await;
    let configured = Launch::serve(&database.url());
    let data = configured.data_dir();
    start(configured).await;
    assert!(!data.join("keys").exists(), "a KEK was generated");

    let from_file = Launch::serve(&database.url()).env_remove("BOSUN_KEYS_KEK");
    let file = from_file.file("elsewhere/kek", &format!("{}\n", crate::KEK));
    let data = from_file.data_dir();
    start(from_file.env("BOSUN_KEYS_KEK_FILE", &file.to_string_lossy())).await;
    assert!(!data.join("keys").exists(), "a KEK was generated");
}

#[tokio::test]
async fn no_token_file_is_written_when_the_database_has_users_or_bootstrap_admin_is_set() {
    let database = TestDatabase::migrated().await;
    let bootstrap = Launch::serve(&database.url()).env("BOSUN_BOOTSTRAP_ADMIN", "roman");
    let data = bootstrap.data_dir();
    start(bootstrap).await;
    assert!(!data.join("admin.token").exists());

    let other = TestDatabase::migrated().await;
    owner(&other.url(), "roman").await;
    let users = Launch::serve(&other.url());
    let data = users.data_dir();
    start(users).await;
    assert!(!data.join("admin.token").exists());
    assert_eq!(
        strings(
            other.connection(),
            "SELECT login AS value FROM bosun.\"user\""
        )
        .await,
        ["roman"]
    );
}

#[tokio::test]
async fn without_a_configuration_file_the_server_reads_the_one_in_its_data_directory() {
    let database = TestDatabase::migrated().await;
    let launch = Launch::without_config_file(&database.url(), &["serve"]);
    launch.file(
        "etc/bosun.toml",
        "[server]\nlisten = \"127.0.0.1:0\"\n\n[database]\nmax_connections = 2\n",
    );
    let mut server = launch.spawn();
    let address = server.listening().await;
    assert!(address.ip().is_loopback(), "{address}");
    assert_ne!(address.port(), 0);
    eventually(address, "/readyz", 200).await;
}

#[tokio::test]
async fn a_data_directory_where_the_kek_cannot_be_written_is_refused_with_the_path() {
    let launch = Launch::serve("postgres://bosun@127.0.0.1:1/bosun").env_remove("BOSUN_KEYS_KEK");
    let keys = launch.data_dir().join("keys");
    fs::create_dir(&keys).expect("the directory of the KEK");
    fs::set_permissions(&keys, fs::Permissions::from_mode(0o500)).expect("a read-only directory");
    let (status, lines) = launch.spawn().exit().await;
    assert!(!status.success(), "{lines:#?}");
    let expected = format!(
        "could not generate the KEK in {}",
        keys.join("kek").display()
    );
    assert!(
        crate::errors(&lines)
            .iter()
            .any(|error| error.starts_with(&expected)),
        "expected an error starting with `{expected}`: {lines:#?}"
    );
}
