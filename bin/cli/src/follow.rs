use std::time::Duration;

use bosun_types::api::{LogEnd, LogLine};
use bosun_types::id::{ExecutionId, StepId};
use tokio::time::{sleep, timeout};

use crate::client::{Client, Failure, Patience, chain};

const QUIET: Duration = Duration::from_secs(45);

const RECONNECT: Duration = Duration::from_millis(250);

#[derive(Debug, PartialEq, Eq)]
struct Message {
    event: String,
    data: String,
}

#[derive(Default)]
struct Parser {
    pending: Vec<u8>,
    event: Option<String>,
    data: Option<String>,
}

impl Parser {
    fn feed(&mut self, bytes: &[u8]) -> Vec<Message> {
        self.pending.extend_from_slice(bytes);
        let mut messages = Vec::new();
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = String::from_utf8_lossy(&line[..end]);
            let line = line.strip_suffix('\r').unwrap_or(&line);
            if line.is_empty() {
                let event = self.event.take();
                if let Some(data) = self.data.take() {
                    messages.push(Message {
                        event: event.unwrap_or_else(|| "message".to_owned()),
                        data,
                    });
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => self.event = Some(value.to_owned()),
                "data" => match &mut self.data {
                    Some(data) => {
                        data.push('\n');
                        data.push_str(value);
                    }
                    None => self.data = Some(value.to_owned()),
                },
                _ => {}
            }
        }
        messages
    }
}

pub(crate) async fn step(
    client: &Client,
    execution: ExecutionId,
    step: StepId,
    mut line: impl FnMut(&LogLine),
) -> Result<u64, Failure> {
    let path = format!("/api/v1/executions/{execution}/steps/{step}/log");
    let mut last = None;
    let mut patience = Patience::new();
    loop {
        match once(client, &path, &mut last, &mut line).await {
            Ok(Some(lines)) => return Ok(lines),
            Ok(None) => {
                patience = Patience::new();
                sleep(RECONNECT).await;
            }
            Err(failure) => patience.bear(failure).await?,
        }
    }
}

async fn once(
    client: &Client,
    path: &str,
    last: &mut Option<u64>,
    line: &mut impl FnMut(&LogLine),
) -> Result<Option<u64>, Failure> {
    let mut response = client.events(path, &[("follow", "true")], *last).await?;
    let mut parser = Parser::default();
    loop {
        let chunk = match timeout(QUIET, response.chunk()).await {
            Err(_) => {
                return Err(Failure::transient(format!(
                    "{path} sent nothing for {}s",
                    QUIET.as_secs()
                )));
            }
            Ok(Err(error)) => {
                return Err(Failure::transient(format!(
                    "{path} broke off: {}",
                    chain(&error)
                )));
            }
            Ok(Ok(None)) => return Ok(None),
            Ok(Ok(Some(chunk))) => chunk,
        };
        for message in parser.feed(&chunk) {
            if message.event == "end" {
                let end: LogEnd = serde_json::from_str(&message.data).map_err(|error| {
                    Failure::new(format!("{path} sent an end that is not one: {error}"))
                })?;
                return Ok(Some(end.lines));
            }
            let next: LogLine = serde_json::from_str(&message.data).map_err(|error| {
                Failure::new(format!("{path} sent a line that is not one: {error}"))
            })?;
            if last.is_none_or(|last| next.number > last) {
                *last = Some(next.number);
                line(&next);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Message, Parser};

    fn message(event: &str, data: &str) -> Message {
        Message {
            event: event.to_owned(),
            data: data.to_owned(),
        }
    }

    #[test]
    fn events_split_across_chunks_and_lines_arrive_whole() {
        let mut parser = Parser::default();
        assert_eq!(parser.feed(b"id: 1\ndata: {\"number\":1,"), Vec::new());
        assert_eq!(
            parser.feed(b"\"line\":\"a\"}\n\n: keep-alive\n\nid: 2\r\ndata: x\r\ndata:y\r\n\r\n"),
            vec![
                message("message", "{\"number\":1,\"line\":\"a\"}"),
                message("message", "x\ny"),
            ]
        );
        assert_eq!(
            parser.feed(b"event: end\ndata: {\"lines\":2}\n\nevent: empty\n\n"),
            vec![message("end", "{\"lines\":2}")]
        );
        assert_eq!(parser.feed(b"data: z\n\n"), vec![message("message", "z")]);
    }
}
