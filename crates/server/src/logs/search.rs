use std::io;
use std::sync::Arc;

use bosun_blob::{Key, Store as _};
use bosun_db::logs::StoredObject;
use tokio::sync::mpsc;
use tokio_util::io::SyncIoBridge;

use super::object::{self, FRAME_LINES};
use super::reader::{Failure, Line, Reader, Scanned};
use crate::store::Backend;

pub(crate) type Found = mpsc::Sender<Result<Line, Failure>>;

#[derive(Debug, Clone)]
pub(crate) struct Needle(String);

impl Needle {
    pub(crate) fn new(text: &str) -> Needle {
        Needle(text.to_lowercase())
    }

    pub(crate) fn finds(&self, line: &[u8]) -> bool {
        let text = line
            .iter()
            .position(|byte| *byte == b' ')
            .map_or(&b""[..], |space| &line[space + 1..]);
        String::from_utf8_lossy(text)
            .to_lowercase()
            .contains(&self.0)
    }
}

pub(crate) async fn run(
    mut reader: Reader,
    store: Arc<Backend>,
    needle: Needle,
    limit: u64,
    first: Scanned,
    found: Found,
) {
    let mut remaining = limit;
    let mut next = Ok(first);
    loop {
        match next {
            Ok(Scanned::Lines(lines)) => {
                for line in lines {
                    if !needle.finds(&line.text) {
                        continue;
                    }
                    if found.send(Ok(line)).await.is_err() {
                        return;
                    }
                    remaining -= 1;
                    if remaining == 0 {
                        return;
                    }
                }
            }
            Ok(Scanned::Object(stored)) => {
                let position = reader.position();
                if let Err(failure) =
                    stream(&store, &stored, position, needle, remaining, found.clone()).await
                {
                    let _ = found.send(Err(failure)).await;
                }
                return;
            }
            Ok(Scanned::End) => return,
            Err(failure) => {
                let _ = found.send(Err(failure)).await;
                return;
            }
        }
        next = reader.scan().await;
    }
}

async fn stream(
    store: &Backend,
    stored: &StoredObject,
    position: u64,
    needle: Needle,
    limit: u64,
    found: Found,
) -> Result<(), Failure> {
    if position >= stored.lines {
        return Ok(());
    }
    let unreadable = |problem: String| Failure::Object {
        key: stored.key.clone(),
        problem,
    };
    let frame = position / FRAME_LINES;
    let start = usize::try_from(frame)
        .ok()
        .and_then(|index| stored.frames.get(index))
        .copied()
        .ok_or_else(|| unreadable(format!("it has no frame for line {}", position + 1)))?;
    let key = Key::new(stored.key.clone()).map_err(|error| unreadable(error.to_string()))?;
    let body = store.get(&key, start..stored.size).await?;
    let bridge = SyncIoBridge::new(body);
    let streamed = tokio::task::spawn_blocking(move || {
        let mut number = frame * FRAME_LINES;
        let mut remaining = limit;
        object::each_line(bridge, |text| {
            number += 1;
            if number <= position || !needle.finds(text) {
                return true;
            }
            let line = Line {
                number,
                text: text.to_vec(),
            };
            if found.blocking_send(Ok(line)).is_err() {
                return false;
            }
            remaining -= 1;
            remaining > 0
        })
    })
    .await
    .map_err(io::Error::other)
    .and_then(|streamed| streamed);
    streamed.map_err(|error| unreadable(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::Needle;

    #[test]
    fn a_needle_finds_its_text_in_any_case_after_the_time_of_a_line() {
        let needle = Needle::new("Error");
        assert!(needle.finds(b"2026-10-25T10:00:00.000000000Z an ERROR here"));
        assert!(needle.finds(b"2026-10-25T10:00:00.000000000Z \x1b[31merror\x1b[0m: x"));
        assert!(!needle.finds(b"2026-10-25T10:00:00.000000000Z all is well"));
        assert!(!Needle::new("2026").finds(b"2026-10-25T10:00:00.000000000Z no year"));
        assert!(Needle::new("\u{c9}t\u{c9}").finds("t \u{e9}t\u{e9}".as_bytes()));
        assert!(!needle.finds(b"error-without-a-time"));
    }
}
