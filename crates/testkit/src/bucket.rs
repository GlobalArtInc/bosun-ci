use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use bosun_blob::{S3, S3Settings, Store as _};
use url::Url;
use uuid::Uuid;

const VARIABLE: &str = "BOSUN_TEST_S3_URL";

const PREFIX: &str = "bosun-test-";

const REGION: &str = "us-east-1";

pub struct TestBucket {
    settings: S3Settings,
    store: S3,
}

struct Server {
    endpoint: String,
    access_key_id: String,
    secret_access_key: String,
    owner: String,
    counter: AtomicU64,
}

impl TestBucket {
    pub async fn create() -> TestBucket {
        let server = server();
        let number = server.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let settings = S3Settings {
            endpoint: server.endpoint.clone(),
            bucket: format!("{PREFIX}{}-{number}", server.owner),
            region: REGION.to_owned(),
            path_style: true,
            access_key_id: server.access_key_id.clone(),
            secret_access_key: server.secret_access_key.clone(),
        };
        let store = S3::new(settings.clone())
            .unwrap_or_else(|error| panic!("bosun-testkit cannot use {VARIABLE}: {error}"));
        store.create_bucket().await.unwrap_or_else(|error| {
            panic!(
                "bosun-testkit could not create the bucket {}: {error}",
                settings.bucket
            )
        });
        TestBucket { settings, store }
    }

    pub fn settings(&self) -> &S3Settings {
        &self.settings
    }

    pub fn store(&self) -> &S3 {
        &self.store
    }
}

impl Drop for TestBucket {
    fn drop(&mut self) {
        let settings = self.settings.clone();
        let bucket = settings.bucket.clone();
        let removed = thread::Builder::new()
            .name("bosun-testkit".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| error.to_string())?;
                runtime.block_on(remove(settings))
            })
            .map(|thread| thread.join());
        match removed {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(problem))) => eprintln!(
                "bosun-testkit could not remove the bucket {bucket}, which stays on the S3 server \
                 until its container is replaced: {problem}"
            ),
            _ => eprintln!("bosun-testkit could not remove the bucket {bucket}"),
        }
    }
}

async fn remove(settings: S3Settings) -> Result<(), String> {
    let store = S3::new(settings).map_err(|error| error.to_string())?;
    for key in store.list("").await.map_err(|error| error.to_string())? {
        store
            .delete(&key)
            .await
            .map_err(|error| error.to_string())?;
    }
    store
        .delete_bucket()
        .await
        .map_err(|error| error.to_string())
}

fn server() -> &'static Server {
    static SERVER: OnceLock<Server> = OnceLock::new();
    SERVER.get_or_init(Server::new)
}

impl Server {
    fn new() -> Server {
        let text = std::env::var(VARIABLE).unwrap_or_else(|_| {
            panic!(
                "{VARIABLE} is not set: tests create their buckets on that S3 server, for example \
                 http://bosun:bosun-secret@127.0.0.1:9000 (CLAUDE.md says how to start one)"
            )
        });
        let mut url =
            Url::parse(&text).unwrap_or_else(|error| panic!("{VARIABLE} is not a URL: {error}"));
        let access_key_id = url.username().to_owned();
        let secret_access_key = url.password().unwrap_or_default().to_owned();
        if access_key_id.is_empty() || secret_access_key.is_empty() {
            panic!("{VARIABLE} names the access key and the secret as its user and password");
        }
        let cleared = url.set_username("").is_ok() && url.set_password(None).is_ok();
        assert!(cleared, "{VARIABLE} is not the address of a host");
        Server {
            endpoint: url.to_string(),
            access_key_id,
            secret_access_key,
            owner: Uuid::now_v7().simple().to_string(),
            counter: AtomicU64::new(0),
        }
    }
}
