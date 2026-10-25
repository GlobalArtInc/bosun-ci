use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use bosun_types::id::ExecutionId;

const PARTIAL: &str = "partial";

pub(crate) struct Spool {
    root: PathBuf,
    limit: u64,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    bytes: u64,
    executions: BTreeMap<ExecutionId, Entry>,
}

struct Entry {
    next: u64,
    running: bool,
    chunks: BTreeMap<u64, Stored>,
}

#[derive(Clone, Copy)]
struct Stored {
    step: u32,
    bytes: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Chunk {
    pub(crate) seq: u64,
    pub(crate) step: u32,
    pub(crate) lines: Vec<u8>,
}

impl Spool {
    pub(crate) fn open(root: &Path, limit: u64) -> io::Result<Spool> {
        fs::create_dir_all(root)?;
        let mut state = State::default();
        for entry in fs::read_dir(root)? {
            let path = entry?.path();
            let Some(execution) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.parse::<ExecutionId>().ok())
            else {
                continue;
            };
            let mut chunks = BTreeMap::new();
            for file in fs::read_dir(&path)? {
                let file = file?.path();
                let name = file
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default();
                if name.ends_with(PARTIAL) {
                    fs::remove_file(&file)?;
                    continue;
                }
                let Some((seq, step)) = name
                    .split_once('-')
                    .and_then(|(seq, step)| Some((seq.parse().ok()?, step.parse().ok()?)))
                else {
                    continue;
                };
                let bytes = file.metadata()?.len();
                state.bytes += bytes;
                chunks.insert(seq, Stored { step, bytes });
            }
            if chunks.is_empty() {
                let _ = fs::remove_dir_all(&path);
                continue;
            }
            let next = chunks.keys().next_back().map_or(1, |last| last + 1);
            state.executions.insert(
                execution,
                Entry {
                    next,
                    running: false,
                    chunks,
                },
            );
        }
        Ok(Spool {
            root: root.to_owned(),
            limit,
            state: Mutex::new(state),
        })
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn directory(&self, execution: ExecutionId) -> PathBuf {
        self.root.join(execution.to_string())
    }

    fn file(&self, execution: ExecutionId, seq: u64, step: u32) -> PathBuf {
        self.directory(execution).join(format!("{seq}-{step}"))
    }

    pub(crate) fn store(
        &self,
        execution: ExecutionId,
        step: u32,
        lines: &[u8],
    ) -> io::Result<Option<u64>> {
        let bytes = lines.len() as u64;
        let seq = {
            let mut state = self.state();
            if state.bytes.saturating_add(bytes) > self.limit {
                return Ok(None);
            }
            state.bytes += bytes;
            state
                .executions
                .entry(execution)
                .or_insert(Entry {
                    next: 1,
                    running: true,
                    chunks: BTreeMap::new(),
                })
                .next
        };
        let written = self.write(execution, seq, step, lines);
        let mut guard = self.state();
        let state = &mut *guard;
        match state.executions.get_mut(&execution) {
            Some(entry) if written.is_ok() => {
                entry.next = seq + 1;
                entry.chunks.insert(seq, Stored { step, bytes });
            }
            _ => state.bytes -= bytes,
        }
        written.map(|()| Some(seq))
    }

    fn write(&self, execution: ExecutionId, seq: u64, step: u32, lines: &[u8]) -> io::Result<()> {
        fs::create_dir_all(self.directory(execution))?;
        let file = self.file(execution, seq, step);
        let partial = file.with_file_name(format!("{seq}-{step}.{PARTIAL}"));
        let mut written = File::create(&partial)?;
        written.write_all(lines)?;
        written.sync_data()?;
        fs::rename(&partial, &file)
    }

    pub(crate) fn read(&self, execution: ExecutionId, seq: u64) -> io::Result<Option<Chunk>> {
        let stored = self
            .state()
            .executions
            .get(&execution)
            .and_then(|entry| entry.chunks.get(&seq))
            .copied();
        let Some(stored) = stored else {
            return Ok(None);
        };
        let lines = fs::read(self.file(execution, seq, stored.step))?;
        Ok(Some(Chunk {
            seq,
            step: stored.step,
            lines,
        }))
    }

    pub(crate) fn unacknowledged(&self) -> Vec<(ExecutionId, u64)> {
        self.state()
            .executions
            .iter()
            .flat_map(|(execution, entry)| entry.chunks.keys().map(|seq| (*execution, *seq)))
            .collect()
    }

    pub(crate) fn last(&self, execution: ExecutionId) -> u64 {
        self.state()
            .executions
            .get(&execution)
            .map_or(0, |entry| entry.next - 1)
    }

    pub(crate) fn acknowledge(&self, execution: ExecutionId, seq: u64) -> io::Result<()> {
        let (files, done) = {
            let mut guard = self.state();
            let state = &mut *guard;
            let Some(entry) = state.executions.get_mut(&execution) else {
                return Ok(());
            };
            let kept = entry.chunks.split_off(&(seq.saturating_add(1)));
            let acknowledged = std::mem::replace(&mut entry.chunks, kept);
            state.bytes -= acknowledged
                .values()
                .map(|stored| stored.bytes)
                .sum::<u64>();
            let files: Vec<PathBuf> = acknowledged
                .iter()
                .map(|(seq, stored)| self.file(execution, *seq, stored.step))
                .collect();
            (files, forget_when_done(state, execution))
        };
        for file in files {
            match fs::remove_file(file) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        self.remove_when_done(execution, done)
    }

    pub(crate) fn finish(&self, execution: ExecutionId) -> io::Result<()> {
        let done = {
            let mut state = self.state();
            if let Some(entry) = state.executions.get_mut(&execution) {
                entry.running = false;
            }
            forget_when_done(&mut state, execution)
        };
        self.remove_when_done(execution, done)
    }

    fn remove_when_done(&self, execution: ExecutionId, done: bool) -> io::Result<()> {
        if !done {
            return Ok(());
        }
        match fs::remove_dir_all(self.directory(execution)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn forget_when_done(state: &mut State, execution: ExecutionId) -> bool {
    let done = state
        .executions
        .get(&execution)
        .is_some_and(|entry| !entry.running && entry.chunks.is_empty());
    if done {
        state.executions.remove(&execution);
    }
    done
}

#[cfg(test)]
mod tests {
    use std::fs;

    use bosun_types::id::ExecutionId;

    use super::Spool;

    const FIRST: &str = "0199a5f2-2c6f-7a3e-8a3b-5f0d4c1e2b3a";

    const SECOND: &str = "0199a5f2-2c6f-7a3e-8a3b-5f0d4c1e2b3b";

    fn id(text: &str) -> ExecutionId {
        text.parse().expect("an execution id")
    }

    #[test]
    fn chunks_are_numbered_per_execution_and_survive_reopening_until_acknowledged() {
        let directory = tempfile::tempdir().expect("a directory");
        let root = directory.path().join("spool");
        let spool = Spool::open(&root, 1024).expect("a spool");
        assert_eq!(spool.store(id(FIRST), 0, b"a\n").expect("stored"), Some(1));
        assert_eq!(spool.store(id(FIRST), 1, b"b\n").expect("stored"), Some(2));
        assert_eq!(spool.store(id(SECOND), 0, b"c\n").expect("stored"), Some(1));
        assert_eq!(spool.last(id(FIRST)), 2);
        fs::write(root.join(FIRST).join("3-1.partial"), "torn").expect("a torn chunk");
        drop(spool);

        let spool = Spool::open(&root, 1024).expect("a spool");
        assert!(!root.join(FIRST).join("3-1.partial").exists());
        assert_eq!(
            spool.unacknowledged(),
            vec![(id(FIRST), 1), (id(FIRST), 2), (id(SECOND), 1)]
        );
        let chunk = spool.read(id(FIRST), 2).expect("read").expect("a chunk");
        assert_eq!((chunk.step, chunk.lines.as_slice()), (1, b"b\n".as_slice()));
        spool.acknowledge(id(FIRST), 1).expect("acknowledged");
        assert_eq!(spool.read(id(FIRST), 1).expect("read"), None);
        assert_eq!(
            spool.unacknowledged(),
            vec![(id(FIRST), 2), (id(SECOND), 1)]
        );
        spool.acknowledge(id(FIRST), 2).expect("acknowledged");
        assert!(
            !root.join(FIRST).exists(),
            "a delivered execution kept its directory"
        );
        drop(spool);

        let spool = Spool::open(&root, 1024).expect("a spool");
        assert_eq!(spool.unacknowledged(), vec![(id(SECOND), 1)]);
    }

    #[test]
    fn a_running_execution_keeps_its_numbering_after_everything_is_acknowledged() {
        let directory = tempfile::tempdir().expect("a directory");
        let spool = Spool::open(&directory.path().join("spool"), 1024).expect("a spool");
        assert_eq!(spool.store(id(FIRST), 0, b"a\n").expect("stored"), Some(1));
        spool.acknowledge(id(FIRST), 1).expect("acknowledged");
        assert_eq!(spool.store(id(FIRST), 0, b"b\n").expect("stored"), Some(2));
        spool.acknowledge(id(FIRST), 2).expect("acknowledged");
        spool.finish(id(FIRST)).expect("finished");
        assert!(spool.unacknowledged().is_empty());
        assert!(!directory.path().join("spool").join(FIRST).exists());
    }

    #[test]
    fn a_full_spool_refuses_chunks_until_acknowledgements_free_it() {
        let directory = tempfile::tempdir().expect("a directory");
        let spool = Spool::open(&directory.path().join("spool"), 8).expect("a spool");
        assert_eq!(
            spool.store(id(FIRST), 0, b"12345\n").expect("stored"),
            Some(1)
        );
        assert_eq!(spool.store(id(FIRST), 0, b"678\n").expect("refused"), None);
        spool.acknowledge(id(FIRST), 1).expect("acknowledged");
        assert_eq!(
            spool.store(id(FIRST), 0, b"678\n").expect("stored"),
            Some(2)
        );
    }
}
