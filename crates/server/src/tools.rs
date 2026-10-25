use std::io;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bosun_blob::{Key, Store};
use bosun_db::tools::{NewStored, Stored};
use bosun_types::ir::NodeRuntime;
use sea_orm::{DatabaseConnection, TransactionTrait};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_stream::StreamExt;
use tokio_util::io::StreamReader;
use url::Url;

use crate::Error;
use crate::config;
use crate::digest::{Digesting, unhex};
use crate::store::Backend;

const NODE: &str = "node";

pub(crate) const PLATFORMS: [&str; 4] = [
    "linux-x64",
    "linux-arm64",
    "linux-x64-musl",
    "linux-arm64-musl",
];

const MUSL: &str = "-musl";

const CHECKSUMS: &str = "SHASUMS256.txt";

const LONGEST_CHECKSUMS: u64 = 1 << 20;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

const READ_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) const LOCK_RETRY: Duration = Duration::from_millis(100);

pub(crate) const LOCK_WAIT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Runtimes {
    pub(crate) node12: String,
    pub(crate) node16: String,
    pub(crate) node20: String,
    pub(crate) node24: String,
}

impl Runtimes {
    pub(crate) fn version(&self, using: NodeRuntime) -> &str {
        match using {
            NodeRuntime::Node12 => &self.node12,
            NodeRuntime::Node16 => &self.node16,
            NodeRuntime::Node20 => &self.node20,
            NodeRuntime::Node24 => &self.node24,
        }
    }

    pub(crate) fn serves(&self, version: &str) -> bool {
        [&self.node12, &self.node16, &self.node20, &self.node24]
            .into_iter()
            .any(|served| served == version)
    }

    pub(crate) fn is_version(text: &str) -> bool {
        let parts: Vec<&str> = text.split('.').collect();
        parts.len() == 3
            && parts.iter().all(|part| {
                !part.is_empty()
                    && part.len() <= 5
                    && part.bytes().all(|byte| byte.is_ascii_digit())
            })
    }
}

pub(crate) fn major(version: &str) -> &str {
    version.split('.').next().unwrap_or(version)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Failure {
    #[error(transparent)]
    Database(#[from] bosun_db::Error),
    #[error("the object store failed: {0}")]
    Store(#[from] bosun_blob::Error),
    #[error(transparent)]
    Key(#[from] bosun_blob::KeyError),
    #[error("a task that reads an archive ended: {0}")]
    Task(String),
    #[error("another replica held the lock of {key} for {waited:?}")]
    Locked { key: String, waited: Duration },
}

impl From<sea_orm::DbErr> for Failure {
    fn from(error: sea_orm::DbErr) -> Failure {
        Failure::Database(error.into())
    }
}

pub(crate) struct Tools {
    db: DatabaseConnection,
    store: Arc<Backend>,
    http: reqwest::Client,
    settings: config::Tools,
}

impl Tools {
    pub(crate) fn new(
        db: DatabaseConnection,
        store: Arc<Backend>,
        settings: config::Tools,
    ) -> Result<Tools, Error> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("bosun/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .map_err(|error| Error::Tools(error.to_string()))?;
        Ok(Tools {
            db,
            store,
            http,
            settings,
        })
    }

    pub(crate) fn runtimes(&self) -> &Runtimes {
        &self.settings.runtimes
    }

    pub(crate) async fn node(
        &self,
        version: &str,
        platform: &str,
    ) -> Result<Result<Stored, String>, Failure> {
        if let Some(stored) = bosun_db::tools::find(&self.db, NODE, version, platform).await? {
            return Ok(Ok(stored));
        }
        let name = format!("tools/{NODE}/{version}/{platform}.tar.xz");
        let key = Key::new(&name)?;
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            let transaction = self.db.begin().await?;
            if bosun_db::objects::lock(&transaction, &name).await? {
                if let Some(stored) =
                    bosun_db::tools::find(&transaction, NODE, version, platform).await?
                {
                    return Ok(Ok(stored));
                }
                let (size, sha256) = match self.fetch(version, platform, &key).await? {
                    Ok(fetched) => fetched,
                    Err(refused) => return Ok(Err(refused)),
                };
                bosun_db::tools::record(
                    &transaction,
                    NewStored {
                        tool: NODE,
                        version,
                        platform,
                        key: &name,
                        size,
                        sha256: &sha256,
                    },
                )
                .await?;
                transaction.commit().await?;
                return Ok(Ok(Stored { key: name, size }));
            }
            drop(transaction);
            if Instant::now() >= deadline {
                return Err(Failure::Locked {
                    key: name,
                    waited: LOCK_WAIT,
                });
            }
            tokio::time::sleep(LOCK_RETRY).await;
            if let Some(stored) = bosun_db::tools::find(&self.db, NODE, version, platform).await? {
                return Ok(Ok(stored));
            }
        }
    }

    async fn fetch(
        &self,
        version: &str,
        platform: &str,
        key: &Key,
    ) -> Result<Result<(u64, Vec<u8>), String>, Failure> {
        let upstream = if platform.ends_with(MUSL) {
            &self.settings.node_musl_upstream
        } else {
            &self.settings.node_upstream
        };
        let directory = format!("v{version}");
        let archive = format!("node-v{version}-{platform}.tar.xz");
        let checksums = match self.text(&below(upstream, &directory, CHECKSUMS)).await {
            Ok(checksums) => checksums,
            Err(refused) => return Ok(Err(refused)),
        };
        let Some(expected) = listed(&checksums, &archive) else {
            return Ok(Err(format!(
                "the {CHECKSUMS} of {directory} at the upstream lists no {archive}"
            )));
        };
        let body = match self.open(&below(upstream, &directory, &archive)).await {
            Ok(body) => body,
            Err(refused) => return Ok(Err(refused)),
        };
        let mut digesting = Digesting::expecting(body, expected);
        let stored = self.store.put(key, &mut digesting).await;
        match (stored, digesting.failure()) {
            (Ok(()), _) => Ok(Ok((
                digesting.size(),
                digesting.digest().unwrap_or_default().to_vec(),
            ))),
            (Err(_), Some(failure)) => Ok(Err(format!(
                "{archive} from the upstream was not stored: {failure}"
            ))),
            (Err(error), None) => Err(error.into()),
        }
    }

    async fn text(&self, url: &Url) -> Result<String, String> {
        let mut text = String::new();
        self.open(url)
            .await?
            .take(LONGEST_CHECKSUMS)
            .read_to_string(&mut text)
            .await
            .map_err(|error| format!("{url} cannot be read: {error}"))?;
        Ok(text)
    }

    async fn open(&self, url: &Url) -> Result<Box<dyn AsyncRead + Send + Unpin>, String> {
        if url.scheme() == "file" {
            let path = url
                .to_file_path()
                .map_err(|()| format!("{url} names no file"))?;
            let file = tokio::fs::File::open(&path)
                .await
                .map_err(|error| format!("{} cannot be read: {error}", path.display()))?;
            return Ok(Box::new(file));
        }
        let response = self
            .http
            .get(url.clone())
            .send()
            .await
            .map_err(|error| format!("{url} cannot be reached: {}", error.without_url()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("{url} answered {status}"));
        }
        let body = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|error| io::Error::other(error.without_url())));
        Ok(Box::new(StreamReader::new(body)))
    }
}

fn below(upstream: &Url, directory: &str, file: &str) -> Url {
    let mut url = upstream.clone();
    if let Ok(mut segments) = url.path_segments_mut() {
        segments.pop_if_empty().push(directory).push(file);
    }
    url
}

fn listed(checksums: &str, archive: &str) -> Option<Vec<u8>> {
    checksums.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let digest = fields.next()?;
        let name = fields.next()?.trim_start_matches('*');
        if name != archive || fields.next().is_some() || digest.len() != 64 {
            return None;
        }
        unhex(digest)
    })
}

#[cfg(test)]
mod tests {
    use url::Url;

    use super::{Runtimes, below, listed, major};

    #[test]
    fn a_version_of_node_is_three_numbers() {
        for version in ["24.22.1", "20.0.0", "100.10.1000"] {
            assert!(Runtimes::is_version(version), "{version}");
        }
        for refused in [
            "24",
            "24.1",
            "v24.22.1",
            "24.22.1-rc",
            "24..1",
            "24.22.1.0",
            "",
        ] {
            assert!(!Runtimes::is_version(refused), "{refused}");
        }
        assert_eq!(major("24.22.1"), "24");
    }

    #[test]
    fn an_archive_is_listed_with_its_digest_among_the_checksums() {
        let digest = "6c172a9ab6718f7b801fb5a7bd486af2ee5e32758086a04b84fcce2aa77095e6";
        let checksums = format!(
            "{}  node-v24.22.1-linux-arm64.tar.xz\n{digest}  node-v24.22.1-linux-x64.tar.xz\n\
             {digest} *node-v24.22.1-linux-x64-musl.tar.xz\n",
            "0".repeat(64)
        );
        assert_eq!(
            listed(&checksums, "node-v24.22.1-linux-x64.tar.xz").map(|bytes| bytes.len()),
            Some(32)
        );
        assert!(listed(&checksums, "node-v24.22.1-linux-x64-musl.tar.xz").is_some());
        assert_eq!(listed(&checksums, "node-v24.22.1-linux-s390x.tar.xz"), None);
        assert_eq!(
            listed(
                "abc  node-v1.0.0-linux-x64.tar.xz\n",
                "node-v1.0.0-linux-x64.tar.xz"
            ),
            None
        );
    }

    #[test]
    fn a_file_of_the_upstream_is_below_its_address() {
        for (upstream, expected) in [
            (
                "https://nodejs.org/dist",
                "https://nodejs.org/dist/v24.22.1/SHASUMS256.txt",
            ),
            (
                "https://nodejs.org/dist/",
                "https://nodejs.org/dist/v24.22.1/SHASUMS256.txt",
            ),
            (
                "file:///srv/node",
                "file:///srv/node/v24.22.1/SHASUMS256.txt",
            ),
        ] {
            let upstream = Url::parse(upstream).expect("an address");
            assert_eq!(
                below(&upstream, "v24.22.1", "SHASUMS256.txt").as_str(),
                expected
            );
        }
    }
}
