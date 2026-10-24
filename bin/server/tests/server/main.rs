mod config;
mod health;
mod http;
mod proxy;
mod shutdown;
mod start;

use std::env;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

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
        Launch::new("serve", database)
    }

    pub fn migrate(database: &str) -> Launch {
        Launch::new("migrate", database)
    }

    fn new(action: &str, database: &str) -> Launch {
        let directory = tempfile::tempdir().expect("a directory for the configuration");
        let launch = Launch {
            command: Command::new(env!("CARGO_BIN_EXE_bosun-server")),
            directory,
        };
        let mut launch = launch.config(CONFIG);
        for (name, _) in env::vars_os() {
            if name.to_string_lossy().starts_with("BOSUN_") {
                launch.command.env_remove(name);
            }
        }
        let config = launch.config_path();
        launch
            .command
            .arg("--config")
            .arg(config)
            .arg(action)
            .env("BOSUN_DATABASE_URL", database)
            .env("BOSUN_KEYS_KEK", KEK)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        launch
    }

    pub fn config_path(&self) -> PathBuf {
        self.directory.path().join("server.toml")
    }

    pub fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.directory.path().join(name);
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
