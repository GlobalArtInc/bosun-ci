mod agents;
mod api;
mod auth;
mod check;
mod commands;
mod concurrency;
mod config;
mod data;
mod events;
mod forge_events;
mod health;
mod hooks;
mod http;
mod install;
mod logs;
mod oidc;
mod openapi;
mod owner;
mod pools;
mod projects;
mod proxy;
mod pull_requests;
mod queue;
mod reports;
mod results;
mod runs;
mod secrets;
mod shutdown;
mod sign_in;
mod start;
mod workflows;

use std::env;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use bosun_blob::S3Settings;
use bosun_testkit::forge::{APP_ID, APP_KEY, FakeForge};
use sea_orm::{ConnectionTrait, Statement};
use serde_json::Value;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::{Instant, timeout, timeout_at};

pub const KEK: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

pub const WAIT: Duration = Duration::from_secs(60);

pub const CONFIG: &str = "[server]\nlisten = \"127.0.0.1:0\"\n\n[database]\nmax_connections = 2\n";

pub struct Launch {
    command: Command,
    directory: TempDir,
}

impl Launch {
    pub fn serve(database: &str) -> Launch {
        Launch::new(&["serve"], database)
    }

    pub fn check(database: &str) -> Launch {
        Launch::new(&["check"], database)
    }

    pub fn migrate(database: &str) -> Launch {
        Launch::new(&["migrate"], database)
    }

    pub fn rewrap(database: &str) -> Launch {
        Launch::new(&["secrets", "rewrap"], database)
    }

    pub fn create_owner(database: &str, login: &str) -> Launch {
        Launch::new(&["admin", "create", login], database)
    }

    pub fn without_config_file(database: &str, arguments: &[&str]) -> Launch {
        Launch::with(arguments, database, false)
    }

    fn new(arguments: &[&str], database: &str) -> Launch {
        Launch::with(arguments, database, true)
    }

    fn with(arguments: &[&str], database: &str, config: bool) -> Launch {
        let directory = tempfile::tempdir().expect("a directory for the configuration");
        let mut launch = Launch {
            command: Command::new(env!("CARGO_BIN_EXE_bosun-server")),
            directory,
        };
        for (name, _) in env::vars_os() {
            let text = name.to_string_lossy();
            if text.starts_with("BOSUN_") || text.starts_with("AWS_") {
                launch.command.env_remove(name);
            }
        }
        if config {
            launch = launch.config(CONFIG);
            let path = launch.config_path();
            launch.command.arg("--config").arg(path);
        }
        let data = launch.data_dir();
        let store = data.join("store");
        launch
            .command
            .args(arguments)
            .env("BOSUN_DATABASE_URL", database)
            .env("BOSUN_KEYS_KEK", KEK)
            .env("BOSUN_SERVER_DATA_DIR", data)
            .env("BOSUN_STORE_PATH", store)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        launch
    }

    pub fn config_path(&self) -> PathBuf {
        self.directory.path().join("server.toml")
    }

    pub fn data_dir(&self) -> PathBuf {
        self.directory.path().to_owned()
    }

    pub fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.directory.path().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .unwrap_or_else(|error| panic!("{}: {error}", parent.display()));
        }
        fs::write(&path, text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        path
    }

    pub fn config(self, text: &str) -> Launch {
        self.file("server.toml", text);
        self
    }

    pub fn env(mut self, name: &str, value: &str) -> Launch {
        self.command.env(name, value);
        self
    }

    pub fn env_remove(mut self, name: &str) -> Launch {
        self.command.env_remove(name);
        self
    }

    pub fn github(self, forge: &FakeForge) -> Launch {
        let key = self.file("github-app.pem", APP_KEY);
        let secret = self.file(
            "github-webhook-secret",
            &format!("{}\n", forge.webhook_secret()),
        );
        let mut server = forge.api_url();
        server.set_path("/");
        self.env("BOSUN_GITHUB_APP_ID", &APP_ID.to_string())
            .env("BOSUN_GITHUB_PRIVATE_KEY_FILE", &key.to_string_lossy())
            .env(
                "BOSUN_GITHUB_WEBHOOK_SECRET_FILE",
                &secret.to_string_lossy(),
            )
            .env("BOSUN_GITHUB_SERVER_URL", server.as_str())
    }

    pub fn s3(self, settings: &S3Settings) -> Launch {
        self.env_remove("BOSUN_STORE_PATH")
            .env("BOSUN_STORE_KIND", "s3")
            .env("BOSUN_STORE_ENDPOINT", &settings.endpoint)
            .env("BOSUN_STORE_BUCKET", &settings.bucket)
            .env("BOSUN_STORE_REGION", &settings.region)
            .env("BOSUN_STORE_PATH_STYLE", &settings.path_style.to_string())
            .env("BOSUN_STORE_ACCESS_KEY_ID", &settings.access_key_id)
            .env("BOSUN_STORE_SECRET_ACCESS_KEY", &settings.secret_access_key)
    }

    pub async fn finish(mut self) -> Finished {
        let output = timeout(WAIT, self.command.output())
            .await
            .unwrap_or_else(|_| panic!("bosun-server did not exit within {WAIT:?}"))
            .expect("bosun-server runs");
        Finished {
            status: output.status,
            stdout: String::from_utf8(output.stdout).expect("UTF-8 on standard output"),
            logs: String::from_utf8_lossy(&output.stderr)
                .lines()
                .map(|line| serde_json::from_str(line).unwrap_or(Value::String(line.to_owned())))
                .collect(),
        }
    }

    pub fn spawn(mut self) -> Server {
        let mut child = self.command.spawn().expect("bosun-server starts");
        let (sender, lines) = mpsc::unbounded_channel();
        read(child.stdout.take().expect("a piped stdout"), sender.clone());
        read(child.stderr.take().expect("a piped stderr"), sender);
        Server {
            child,
            lines,
            seen: Vec::new(),
            _directory: self.directory,
        }
    }
}

#[derive(Debug)]
pub struct Finished {
    pub status: ExitStatus,
    pub stdout: String,
    pub logs: Vec<Value>,
}

pub async fn owner(database: &str, login: &str) -> String {
    let finished = Launch::create_owner(database, login).finish().await;
    assert!(finished.status.success(), "{finished:#?}");
    finished.stdout.trim_end().to_owned()
}

fn read(stream: impl AsyncRead + Unpin + Send + 'static, sender: UnboundedSender<Value>) {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let value = serde_json::from_str(&line).unwrap_or(Value::String(line));
            if sender.send(value).is_err() {
                break;
            }
        }
    });
}

pub struct Server {
    child: Child,
    lines: UnboundedReceiver<Value>,
    seen: Vec<Value>,
    _directory: TempDir,
}

impl Server {
    pub async fn expect(&mut self, what: &str, matches: impl Fn(&Value) -> bool) -> Value {
        if let Some(line) = self.seen.iter().find(|line| matches(line)) {
            return line.clone();
        }
        let deadline = Instant::now() + WAIT;
        loop {
            match timeout_at(deadline, self.lines.recv()).await {
                Ok(Some(line)) => {
                    self.seen.push(line.clone());
                    if matches(&line) {
                        return line;
                    }
                }
                Ok(None) => panic!(
                    "bosun-server ended its output before it logged {what}: {:#?}",
                    self.seen
                ),
                Err(_) => panic!(
                    "bosun-server did not log {what} within {WAIT:?}: {:#?}",
                    self.seen
                ),
            }
        }
    }

    pub async fn listening(&mut self) -> SocketAddr {
        let line = self
            .expect("that it listens", |line| message(line) == "listening")
            .await;
        line["address"]
            .as_str()
            .and_then(|address| address.parse().ok())
            .unwrap_or_else(|| panic!("a listening line without an address: {line}"))
    }

    pub fn terminate(&self) {
        let pid = self.child.id().expect("a running bosun-server");
        let status = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .expect("kill runs");
        assert!(status.success(), "kill -TERM {pid} failed");
    }

    pub async fn exit(mut self) -> (ExitStatus, Vec<Value>) {
        let status = timeout(WAIT, self.child.wait())
            .await
            .unwrap_or_else(|_| panic!("bosun-server did not exit: {:#?}", self.seen))
            .expect("the exit status of bosun-server");
        while let Ok(Some(line)) = timeout(WAIT, self.lines.recv()).await {
            self.seen.push(line);
        }
        (status, self.seen)
    }
}

pub fn message(line: &Value) -> &str {
    line["message"].as_str().unwrap_or_default()
}

pub fn errors(lines: &[Value]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line["level"] == "ERROR")
        .map(|line| message(line).to_owned())
        .collect()
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(WAIT)
        .build()
        .expect("an HTTP/1.1 client")
}

pub fn h2_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .http2_prior_knowledge()
        .timeout(WAIT)
        .build()
        .expect("an HTTP/2 client")
}

pub async fn get(address: SocketAddr, path: &str) -> reqwest::Response {
    client()
        .get(format!("http://{address}{path}"))
        .send()
        .await
        .unwrap_or_else(|error| panic!("GET {path}: {error}"))
}

pub async fn me(address: SocketAddr, authorization: Option<&str>) -> (u16, Value) {
    let mut request = client().get(format!("http://{address}/api/v1/me"));
    if let Some(authorization) = authorization {
        request = request.header("authorization", authorization);
    }
    let response = request.send().await.expect("GET /api/v1/me");
    let status = response.status().as_u16();
    (status, response.json().await.expect("a JSON body"))
}

pub async fn eventually(address: SocketAddr, path: &str, status: u16) -> Value {
    let deadline = Instant::now() + WAIT;
    loop {
        let response = get(address, path).await;
        if response.status().as_u16() == status {
            return response.json().await.expect("a JSON body");
        }
        assert!(
            Instant::now() < deadline,
            "GET {path} did not answer {status} within {WAIT:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub async fn strings<C: ConnectionTrait>(db: &C, sql: &str) -> Vec<String> {
    db.query_all(Statement::from_string(db.get_database_backend(), sql))
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .into_iter()
        .map(|row| {
            row.try_get::<String>("", "value")
                .expect("a text column `value`")
        })
        .collect()
}

pub fn major() -> i32 {
    env!("CARGO_PKG_VERSION_MAJOR")
        .parse()
        .expect("a numeric major version")
}
