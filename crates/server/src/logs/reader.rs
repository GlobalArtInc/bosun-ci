use std::io;
use std::sync::Arc;

use bosun_blob::{Key, Store as _};
use bosun_db::logs::{Log, StoredChunk, StoredObject};
use bosun_types::id::{ExecutionId, StepId};
use chrono::{DateTime, Utc};
use sea_orm::DatabaseConnection;
use tokio::io::AsyncReadExt;

use super::notice;
use super::object::{self, FRAME_LINES};
use crate::store::Backend;

const PAGE: u64 = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Line {
    pub(crate) number: u64,
    pub(crate) text: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Read {
    Lines(Vec<Line>),
    Waiting,
    End(u64),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Failure {
    #[error("there is no such step in that execution")]
    Missing,
    #[error(transparent)]
    Database(#[from] bosun_db::Error),
    #[error(transparent)]
    Store(#[from] bosun_blob::Error),
    #[error("the log object `{key}` cannot be read: {problem}")]
    Object { key: String, problem: String },
}

pub(crate) struct Reader {
    db: DatabaseConnection,
    store: Arc<Backend>,
    execution: ExecutionId,
    step: StepId,
    position: u64,
    seq: u64,
    counted: u64,
    dropped: u64,
}

impl Reader {
    pub(crate) fn new(
        db: DatabaseConnection,
        store: Arc<Backend>,
        execution: ExecutionId,
        step: StepId,
        offset: u64,
    ) -> Reader {
        Reader {
            db,
            store,
            execution,
            step,
            position: offset,
            seq: 0,
            counted: 0,
            dropped: 0,
        }
    }

    pub(crate) async fn read(&mut self) -> Result<Read, Failure> {
        let window = bosun_db::logs::window(&self.db, self.execution, self.step, self.seq, PAGE)
            .await?
            .ok_or(Failure::Missing)?;
        match window.log {
            Log::Compacted(None) => Ok(Read::End(0)),
            Log::Compacted(Some(stored)) => self.read_object(&stored).await,
            Log::Live(chunks) => Ok(self.read_chunks(chunks, window.completed_at)),
        }
    }

    async fn read_object(&mut self, stored: &StoredObject) -> Result<Read, Failure> {
        if self.position >= stored.lines {
            return Ok(Read::End(stored.lines));
        }
        let unreadable = |problem: String| Failure::Object {
            key: stored.key.clone(),
            problem,
        };
        let frame = self.position / FRAME_LINES;
        let index = usize::try_from(frame).map_err(|error| unreadable(error.to_string()))?;
        let start = *stored
            .frames
            .get(index)
            .ok_or_else(|| unreadable(format!("it has no frame for line {}", self.position + 1)))?;
        let end = stored.frames.get(index + 1).copied().unwrap_or(stored.size);
        let key = Key::new(stored.key.clone()).map_err(|error| unreadable(error.to_string()))?;
        let mut body = self.store.get(&key, start..end).await?;
        let mut compressed = Vec::new();
        body.read_to_end(&mut compressed)
            .await
            .map_err(|error| unreadable(error.to_string()))?;
        let raw = object::decode(compressed)
            .await
            .map_err(|error: io::Error| unreadable(error.to_string()))?;
        let first = frame * FRAME_LINES;
        let mut lines = Vec::new();
        for (offset, text) in (1..).zip(object::lines(&raw)) {
            let number = first + offset;
            if number > self.position {
                lines.push(Line {
                    number,
                    text: text.to_vec(),
                });
                self.position = number;
            }
        }
        if lines.is_empty() {
            return Err(unreadable(format!(
                "it holds fewer than the {} lines it is recorded with",
                stored.lines
            )));
        }
        Ok(Read::Lines(lines))
    }

    fn read_chunks(
        &mut self,
        chunks: Vec<StoredChunk>,
        completed_at: Option<DateTime<Utc>>,
    ) -> Read {
        let full = chunks.len() as u64 == PAGE;
        let mut lines = Vec::new();
        for chunk in chunks {
            for text in object::lines(&chunk.data) {
                self.counted += 1;
                if self.counted > self.position {
                    lines.push(Line {
                        number: self.counted,
                        text: text.to_vec(),
                    });
                    self.position = self.counted;
                }
            }
            self.dropped += chunk.dropped;
            self.seq = chunk.seq;
        }
        if full || !lines.is_empty() {
            return Read::Lines(lines);
        }
        let Some(completed_at) = completed_at else {
            return Read::Waiting;
        };
        if self.dropped == 0 {
            return Read::End(self.counted);
        }
        let total = self.counted + 1;
        if total > self.position {
            self.position = total;
            return Read::Lines(vec![Line {
                number: total,
                text: notice(completed_at, self.dropped),
            }]);
        }
        Read::End(total)
    }
}
