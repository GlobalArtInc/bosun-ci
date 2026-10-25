use bosun_blob::{Directory, Key, Store as _};

use crate::Error;
use crate::config::Store;

const STARTED: &str = "probes/start";

const READINESS: &str = "probes/readiness";

pub(crate) async fn open(config: &Store) -> Result<Directory, Error> {
    let failed = |problem: String| Error::Store {
        path: config.path.clone(),
        problem,
    };
    let store = Directory::open(&config.path)
        .await
        .map_err(|error| failed(error.to_string()))?;
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

pub(crate) async fn answers(store: &Directory) -> Result<(), String> {
    let probe = Key::new(READINESS).map_err(|error| error.to_string())?;
    match store.get(&probe, ..).await {
        Ok(_) | Err(bosun_blob::Error::NotFound(_)) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}
