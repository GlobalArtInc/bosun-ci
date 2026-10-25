use std::ops::RangeBounds;

use bosun_blob::{Directory, Key, Object, S3, Store};
use tokio::io::AsyncRead;

use crate::Error;
use crate::config;

const STARTED: &str = "probes/start";

const READINESS: &str = "probes/readiness";

pub(crate) enum Backend {
    Directory(Directory),
    S3(S3),
}

impl Store for Backend {
    async fn put<R>(&self, key: &Key, body: R) -> Result<(), bosun_blob::Error>
    where
        R: AsyncRead + Send + Unpin,
    {
        match self {
            Backend::Directory(store) => store.put(key, body).await,
            Backend::S3(store) => store.put(key, body).await,
        }
    }

    async fn get<B>(&self, key: &Key, range: B) -> Result<Object, bosun_blob::Error>
    where
        B: RangeBounds<u64> + Send,
    {
        match self {
            Backend::Directory(store) => store.get(key, range).await,
            Backend::S3(store) => store.get(key, range).await,
        }
    }

    async fn delete(&self, key: &Key) -> Result<(), bosun_blob::Error> {
        match self {
            Backend::Directory(store) => store.delete(key).await,
            Backend::S3(store) => store.delete(key).await,
        }
    }

    async fn list(&self, prefix: &str) -> Result<Vec<Key>, bosun_blob::Error> {
        match self {
            Backend::Directory(store) => store.list(prefix).await,
            Backend::S3(store) => store.list(prefix).await,
        }
    }
}

pub(crate) async fn open(config: &config::Store) -> Result<Backend, Error> {
    let (location, opened) = match config {
        config::Store::Directory(path) => (
            format!("in {}", path.display()),
            Directory::open(path).await.map(Backend::Directory),
        ),
        config::Store::S3(settings) => (
            format!(
                "in the bucket `{}` at {}",
                settings.bucket, settings.endpoint
            ),
            S3::new(settings.clone()).map(Backend::S3),
        ),
    };
    let failed = |problem: String| Error::Store {
        location: location.clone(),
        problem,
    };
    let store = opened.map_err(|error| failed(error.to_string()))?;
    let probe = Key::new(STARTED).map_err(|error| failed(error.to_string()))?;
    store
        .put(&probe, &b""[..])
        .await
        .map_err(|error| failed(error.to_string()))?;
    store
        .delete(&probe)
        .await
        .map_err(|error| failed(error.to_string()))?;
    Ok(store)
}

pub(crate) async fn answers(store: &Backend) -> Result<(), String> {
    let probe = Key::new(READINESS).map_err(|error| error.to_string())?;
    match store.get(&probe, ..).await {
        Ok(_) | Err(bosun_blob::Error::NotFound(_)) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}
