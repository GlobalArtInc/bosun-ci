use std::io::{self, ErrorKind, PipeReader};
use std::os::fd::OwnedFd;
use std::time::Duration;

use rustix::io::Errno;
use tokio::net::unix::pipe::Receiver;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout_at};

use crate::mask::Sink;

pub(crate) const LONGEST_LINE: usize = 64 * 1024;

const DRAIN_LIMIT: usize = 1024 * 1024;

pub(crate) struct Output {
    flushes: UnboundedSender<oneshot::Sender<()>>,
    task: JoinHandle<()>,
    ended: bool,
}

impl Output {
    pub(crate) fn start(reader: PipeReader, index: u32, sink: Sink) -> io::Result<Output> {
        let receiver = Receiver::from_owned_fd(OwnedFd::from(reader))?;
        let (flushes, requests) = mpsc::unbounded_channel();
        let task = tokio::spawn(read(receiver, Lines::new(index, sink), requests));
        Ok(Output {
            flushes,
            task,
            ended: false,
        })
    }

    pub(crate) async fn flush(&mut self) {
        let (done, flushed) = oneshot::channel();
        if self.flushes.send(done).is_err() {
            return;
        }
        tokio::select! {
            _ = flushed => {}
            _ = &mut self.task => self.ended = true,
        }
    }

    pub(crate) async fn finish(outputs: Vec<Output>, wait: Duration) {
        let deadline = Instant::now() + wait;
        let tasks: Vec<JoinHandle<()>> = outputs
            .into_iter()
            .filter(|output| !output.ended)
            .map(|Output { flushes, task, .. }| {
                drop(flushes);
                task
            })
            .collect();
        for mut task in tasks {
            if timeout_at(deadline, &mut task).await.is_err() {
                task.abort();
            }
        }
    }
}

async fn read(
    receiver: Receiver,
    mut lines: Lines,
    mut requests: UnboundedReceiver<oneshot::Sender<()>>,
) {
    let mut buffer = vec![0; LONGEST_LINE];
    let mut listening = true;
    loop {
        tokio::select! {
            biased;
            request = requests.recv(), if listening => match request {
                Some(done) => {
                    let ended = drain(&receiver, &mut buffer, &mut lines);
                    lines.finish();
                    let _ = done.send(());
                    if ended {
                        return;
                    }
                }
                None => listening = false,
            },
            ready = receiver.readable() => {
                if ready.is_err() {
                    break;
                }
                match receiver.try_read(&mut buffer) {
                    Ok(0) => break,
                    Ok(read) => lines.push(&buffer[..read]),
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                    Err(_) => break,
                }
            }
        }
    }
    lines.finish();
}

fn drain(receiver: &Receiver, buffer: &mut [u8], lines: &mut Lines) -> bool {
    let mut drained = 0;
    while drained < DRAIN_LIMIT {
        match rustix::io::read(receiver, &mut *buffer) {
            Ok(0) => return true,
            Ok(read) => {
                lines.push(&buffer[..read]);
                drained += read;
            }
            Err(Errno::INTR) => {}
            Err(Errno::AGAIN) => return false,
            Err(_) => return true,
        }
    }
    false
}

struct Lines {
    index: u32,
    sink: Sink,
    pending: Vec<u8>,
    masked: usize,
}

impl Lines {
    fn new(index: u32, sink: Sink) -> Lines {
        Lines {
            index,
            sink,
            pending: Vec::new(),
            masked: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        let mut start = 0;
        while let Some(end) = self.pending[start..].iter().position(|byte| *byte == b'\n') {
            let masked = std::mem::take(&mut self.masked);
            self.sink
                .output(self.index, &self.pending[start..start + end], masked);
            start += end + 1;
        }
        self.pending.drain(..start);
        let held = self.sink.masker().longest().saturating_sub(1);
        while self.pending.len() >= LONGEST_LINE + held {
            let cut = self
                .sink
                .masker()
                .cut(&self.pending, LONGEST_LINE, self.masked);
            self.sink
                .output(self.index, &self.pending[..cut.at], cut.masked);
            self.pending.drain(..cut.at);
            self.masked = cut.carried;
        }
    }

    fn finish(&mut self) {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            let masked = std::mem::take(&mut self.masked);
            self.sink.output(self.index, &line, masked);
        }
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;

    use super::{LONGEST_LINE, Lines};
    use crate::engine::Event;
    use crate::mask::{Masker, Sink};

    fn texts(secrets: &[&str], chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let (events, mut received) = mpsc::unbounded_channel();
        let mut lines = Lines::new(3, Sink::new(events, Masker::new(secrets.iter().copied())));
        for chunk in chunks {
            lines.push(chunk);
        }
        lines.finish();
        drop(lines);
        let mut texts = Vec::new();
        while let Ok(event) = received.try_recv() {
            match event {
                Event::Line { index: 3, text, .. } => texts.push(text),
                other => panic!("an unexpected event {other:?}"),
            }
        }
        texts
    }

    #[test]
    fn lines_are_split_at_newlines_across_reads_and_a_last_partial_line_is_kept() {
        assert_eq!(
            texts(&[], &[b"one\ntw", b"o\n\nthree"]),
            vec![
                b"one".to_vec(),
                b"two".to_vec(),
                Vec::new(),
                b"three".to_vec()
            ]
        );
    }

    #[test]
    fn lines_longer_than_64_kib_are_split() {
        let long = vec![b'x'; LONGEST_LINE * 2 + 5];
        let mut input = long.clone();
        input.push(b'\n');
        let texts = texts(&[], &[&input]);
        assert_eq!(texts.len(), 3);
        assert_eq!(texts[0].len(), LONGEST_LINE);
        assert_eq!(texts[1].len(), LONGEST_LINE);
        assert_eq!(texts[2].len(), 5);
        assert_eq!(texts.concat(), long);
    }

    fn secret_across_the_split() -> (Vec<u8>, Vec<u8>) {
        let mut line = vec![b'x'; LONGEST_LINE - 3];
        line.extend_from_slice(b"a-secret-value");
        line.extend(vec![b'y'; 10]);
        let mut masked = vec![b'x'; LONGEST_LINE - 3];
        masked.extend_from_slice(b"***");
        masked.extend(vec![b'y'; 10]);
        (line, masked)
    }

    #[test]
    fn a_secret_across_the_split_of_a_long_line_is_masked_whole() {
        let (line, masked) = secret_across_the_split();
        let mut ended = line.clone();
        ended.push(b'\n');
        assert_eq!(texts(&["a-secret-value"], &[&ended]).concat(), masked);
        let reads: Vec<&[u8]> = line.chunks(1000).collect();
        let texts = texts(&["a-secret-value"], &reads);
        assert!(texts.len() > 1, "the line was not split");
        assert!(
            texts.iter().all(|text| text.len() <= LONGEST_LINE
                && !text.windows(3).any(|part| part == b"sec" || part == b"lue")),
            "a piece of the line holds a part of the secret"
        );
        assert_eq!(texts.concat(), masked);
    }

    #[test]
    fn a_mask_that_crosses_the_split_from_the_start_of_a_piece_is_carried_into_the_next() {
        let line = b"ab".repeat(LONGEST_LINE);
        let reads: Vec<&[u8]> = line.chunks(4096).collect();
        let texts = texts(&["ab"], &reads);
        assert!(texts.len() > 1, "the line was not split");
        assert!(
            texts
                .iter()
                .all(|text| !text.is_empty() && text.iter().all(|byte| *byte == b'*')),
            "{:?}",
            texts
                .iter()
                .map(|text| String::from_utf8_lossy(text).into_owned())
                .collect::<Vec<_>>()
        );
    }
}
