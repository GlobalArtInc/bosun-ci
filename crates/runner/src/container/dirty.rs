use std::collections::BTreeSet;
use std::fs;
use std::io::{self, ErrorKind};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use super::Engine;
use super::lifecycle::sweep;

pub(super) const MARKER: &str = "dirty";

static MARKED: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

fn marked() -> MutexGuard<'static, BTreeSet<PathBuf>> {
    MARKED.lock().unwrap_or_else(PoisonError::into_inner)
}

pub(super) fn mark(state: &Path) -> io::Result<()> {
    marked().insert(state.to_path_buf());
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(state)?;
    fs::write(state.join(MARKER), "a job container could not be removed\n")
}

pub fn is_marked(state: &Path) -> bool {
    if marked().contains(state) {
        return true;
    }
    match fs::symlink_metadata(state.join(MARKER)) {
        Ok(_) => true,
        Err(error) => error.kind() != ErrorKind::NotFound,
    }
}

pub(super) fn clear(state: &Path) -> io::Result<()> {
    match fs::remove_file(state.join(MARKER)) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    marked().remove(state);
    Ok(())
}

pub async fn recover(engine: &Arc<dyn Engine>, state: &Path) -> Result<(), String> {
    if !is_marked(state) {
        return Ok(());
    }
    let faults = sweep(engine, state).await;
    if faults.is_empty() {
        return Ok(());
    }
    Err(format!(
        "a container of an earlier job could not be removed ({}), so the agent takes no job \
         until it can",
        faults.join("; ")
    ))
}
