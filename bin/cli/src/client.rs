use std::error::Error as _;
use std::time::Duration;

use bosun_types::api::Page;
use bosun_types::diagnostic::{Diagnostic, Severity};
use bosun_types::error::ErrorEnvelope;
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::{Method, RequestBuilder, Response, StatusCode};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::time::{Instant, sleep};

use crate::config::Profile;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

const PATIENCE: Duration = Duration::from_secs(60);

const FIRST_RETRY: Duration = Duration::from_millis(250);

const LONGEST_RETRY: Duration = Duration::from_secs(2);

const EVENT_STREAM: &str = "text/event-stream";

#[derive(Debug)]
pub(crate) struct Failure {
    pub(crate) message: String,
    pub(crate) details: Vec<String>,
    transient: bool,
}

impl Failure {
    pub(crate) fn new(message: impl Into<String>) -> Failure {
        Failure {
            message: message.into(),
            details: Vec::new(),
            transient: false,
        }
    }

    pub(crate) fn transient(message: impl Into<String>) -> Failure {
        Failure {
            transient: true,
            ..Failure::new(message)
        }
    }
}

#[derive(Clone)]
pub(crate) struct Client {
    profile: Profile,
    http: reqwest::Client,
}

impl Client {
    pub(crate) fn new(profile: Profile) -> Result<Client, Failure> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|error| Failure::new(format!("there is no HTTP client: {}", chain(&error))))?;
        Ok(Client { profile, http })
    }

    pub(crate) fn server(&self) -> &str {
        &self.profile.server
    }

    fn request(&self, method: Method, path: &str) -> RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.profile.server))
            .bearer_auth(&self.profile.token)
    }

    pub(crate) async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, Failure> {
        let request = self
            .request(Method::GET, path)
            .query(query)
            .timeout(REQUEST_TIMEOUT);
        decode(path, send(path, request).await?).await
    }

    pub(crate) async fn all<T: DeserializeOwned>(&self, path: &str) -> Result<Vec<T>, Failure> {
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut query = vec![("limit", "500")];
            if let Some(cursor) = &cursor {
                query.push(("cursor", cursor));
            }
            let page: Page<T> = self.get(path, &query).await?;
            items.extend(page.items);
            match page.next {
                Some(next) => cursor = Some(next),
                None => return Ok(items),
            }
        }
    }

    pub(crate) async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, Failure> {
        let request = self
            .request(Method::POST, path)
            .json(body)
            .timeout(REQUEST_TIMEOUT);
        decode(path, send(path, request).await?).await
    }

    pub(crate) async fn put<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, Failure> {
        let request = self
            .request(Method::PUT, path)
            .json(body)
            .timeout(REQUEST_TIMEOUT);
        decode(path, send(path, request).await?).await
    }

    pub(crate) async fn text(&self, path: &str) -> Result<String, Failure> {
        let request = self.request(Method::GET, path).timeout(REQUEST_TIMEOUT);
        let response = send(path, request).await?;
        response
            .text()
            .await
            .map_err(|error| Failure::transient(format!("{path} broke off: {}", chain(&error))))
    }

    pub(crate) async fn events(
        &self,
        path: &str,
        query: &[(&str, &str)],
        last_event_id: Option<u64>,
    ) -> Result<Response, Failure> {
        let mut request = self
            .request(Method::GET, path)
            .query(query)
            .header(ACCEPT, EVENT_STREAM);
        if let Some(last) = last_event_id {
            request = request.header("Last-Event-ID", last.to_string());
        }
        let response = send(path, request).await?;
        let streamed = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with(EVENT_STREAM));
        if !streamed {
            return Err(Failure::new(format!(
                "{path} answered without an event stream"
            )));
        }
        Ok(response)
    }
}

pub(crate) struct Patience {
    failing_since: Option<Instant>,
    delay: Duration,
}

impl Patience {
    pub(crate) fn new() -> Patience {
        Patience {
            failing_since: None,
            delay: FIRST_RETRY,
        }
    }

    pub(crate) async fn bear(&mut self, failure: Failure) -> Result<(), Failure> {
        if !failure.transient {
            return Err(failure);
        }
        let since = *self.failing_since.get_or_insert_with(|| {
            eprintln!(
                "warning: {}; trying again for up to {}s",
                failure.message,
                PATIENCE.as_secs()
            );
            Instant::now()
        });
        if since.elapsed() >= PATIENCE {
            return Err(failure);
        }
        sleep(self.delay).await;
        self.delay = (self.delay * 2).min(LONGEST_RETRY);
        Ok(())
    }
}

pub(crate) async fn patiently<T, F: Future<Output = Result<T, Failure>>>(
    mut attempt: impl FnMut() -> F,
) -> Result<T, Failure> {
    let mut patience = Patience::new();
    loop {
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(failure) => patience.bear(failure).await?,
        }
    }
}

async fn send(path: &str, request: RequestBuilder) -> Result<Response, Failure> {
    let response = request.send().await.map_err(|error| {
        Failure::transient(format!("{path} could not be reached: {}", chain(&error)))
    })?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let transient = matches!(
        status,
        StatusCode::BAD_GATEWAY | StatusCode::SERVICE_UNAVAILABLE | StatusCode::GATEWAY_TIMEOUT
    );
    let text = response.text().await.unwrap_or_default();
    let mut failure = match serde_json::from_str::<ErrorEnvelope>(&text) {
        Ok(envelope) => {
            let mut failure = Failure::new(envelope.error.message);
            failure.details.extend(
                envelope
                    .error
                    .fields
                    .iter()
                    .map(|field| format!("{}: {}", field.field, field.message)),
            );
            failure
                .details
                .extend(envelope.error.diagnostics.iter().map(diagnostic));
            failure
        }
        Err(_) => Failure::new(format!("{path} answered {status}: {}", text.trim())),
    };
    failure.transient = transient;
    Err(failure)
}

async fn decode<T: DeserializeOwned>(path: &str, response: Response) -> Result<T, Failure> {
    let bytes = response
        .bytes()
        .await
        .map_err(|error| Failure::transient(format!("{path} broke off: {}", chain(&error))))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| Failure::new(format!("{path} answered with an unexpected body: {error}")))
}

pub(crate) fn diagnostic(diagnostic: &Diagnostic) -> String {
    let severity = match diagnostic.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };
    format!(
        "{}:{}:{}: {severity}: {} [{}]",
        diagnostic.file,
        diagnostic.span.line,
        diagnostic.span.column,
        diagnostic.message,
        diagnostic.code
    )
}

pub(crate) fn chain(error: &reqwest::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use bosun_types::diagnostic::{Diagnostic, Severity};
    use bosun_types::ir::Span;

    use super::{Failure, diagnostic, patiently};

    #[test]
    fn a_diagnostic_reads_as_its_file_line_and_column() {
        let text = diagnostic(&Diagnostic {
            severity: Severity::Error,
            code: "unknown_needs".to_owned(),
            message: "job `test` needs `biuld`, which is not a job".to_owned(),
            file: ".bosun/workflows/ci.yml".to_owned(),
            span: Span {
                line: 7,
                column: 12,
                end_line: 7,
                end_column: 17,
            },
            path: Vec::new(),
        });
        assert_eq!(
            text,
            ".bosun/workflows/ci.yml:7:12: error: job `test` needs `biuld`, which is not a job \
             [unknown_needs]"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn transient_failures_are_tried_again_and_others_are_not() {
        let attempts = Cell::new(0);
        let recovered = patiently(|| {
            attempts.set(attempts.get() + 1);
            let count = attempts.get();
            async move {
                if count < 3 {
                    Err(Failure::transient("the server restarts"))
                } else {
                    Ok(count)
                }
            }
        })
        .await;
        assert_eq!(recovered.ok(), Some(3));

        attempts.set(0);
        let refused = patiently(|| {
            attempts.set(attempts.get() + 1);
            async { Err::<(), _>(Failure::new("there is no run")) }
        })
        .await;
        assert_eq!(
            refused.err().map(|failure| failure.message).as_deref(),
            Some("there is no run")
        );
        assert_eq!(attempts.get(), 1);

        let abandoned =
            patiently(|| async { Err::<(), _>(Failure::transient("the server is gone")) }).await;
        assert_eq!(
            abandoned.err().map(|failure| failure.message).as_deref(),
            Some("the server is gone")
        );
    }
}
