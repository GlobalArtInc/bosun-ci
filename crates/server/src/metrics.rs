use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bosun_db::metrics::{Durations, Recent, Snapshot};
use bosun_db::outbox::Kind;
use bosun_types::error::ErrorCode;
use chrono::{DateTime, TimeDelta, Utc};
use sea_orm::DatabaseConnection;
use tracing::warn;

use crate::AppState;
use crate::envelope::{self, ApiError};
use crate::outbox;

pub(crate) const PATH: &str = "/metrics";

const EXPOSITION: &str = "text/plain; version=0.0.4; charset=utf-8";

const PLAIN: &str = "text/plain; charset=utf-8";

const RECENT: TimeDelta = TimeDelta::hours(1);

const PERCENTILES: [(f64, &str); 4] = [(0.5, "50"), (0.9, "90"), (0.99, "99"), (1.0, "100")];

const PROCESSING: [f64; 10] = [0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0];

pub(crate) struct Meters {
    verification_failures: AtomicU64,
    processing: Histogram,
    outbox_failures: [AtomicU64; Kind::ALL.len()],
    log_lines: AtomicU64,
    log_bytes: AtomicU64,
}

struct Histogram {
    bounds: &'static [f64],
    observed: Mutex<Observed>,
}

struct Observed {
    counts: Vec<u64>,
    sum: f64,
    count: u64,
}

impl Default for Meters {
    fn default() -> Meters {
        Meters {
            verification_failures: AtomicU64::new(0),
            processing: Histogram::new(&PROCESSING),
            outbox_failures: Default::default(),
            log_lines: AtomicU64::new(0),
            log_bytes: AtomicU64::new(0),
        }
    }
}

impl Meters {
    pub(crate) fn refused_delivery(&self) {
        self.verification_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn processed_delivery(&self, took: Duration) {
        self.processing.observe(took.as_secs_f64());
    }

    pub(crate) fn failed_report(&self, kind: Kind) {
        if let Some(index) = Kind::ALL.iter().position(|known| *known == kind) {
            self.outbox_failures[index].fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn ingested(&self, lines: &[u8]) {
        let count = lines.iter().filter(|byte| **byte == b'\n').count();
        self.log_lines.fetch_add(count as u64, Ordering::Relaxed);
        self.log_bytes
            .fetch_add(lines.len() as u64, Ordering::Relaxed);
    }
}

impl Histogram {
    fn new(bounds: &'static [f64]) -> Histogram {
        Histogram {
            bounds,
            observed: Mutex::new(Observed {
                counts: vec![0; bounds.len()],
                sum: 0.0,
                count: 0,
            }),
        }
    }

    fn observe(&self, value: f64) {
        let mut observed = self.observed.lock().unwrap_or_else(PoisonError::into_inner);
        for (bound, count) in self.bounds.iter().zip(observed.counts.iter_mut()) {
            if value <= *bound {
                *count += 1;
            }
        }
        observed.sum += value;
        observed.count += 1;
    }
}

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(PATH, get(scrape))
        .fallback(envelope::no_route)
        .method_not_allowed_fallback(envelope::no_route)
}

pub(crate) async fn elsewhere(method: Method, uri: Uri) -> ApiError {
    ApiError::new(
        ErrorCode::NotFound,
        format!(
            "there is no {method} {} at this address: the server answers {PATH} only at the \
             address of server.metrics_listen, which is kept off the public address",
            uri.path()
        ),
    )
}

async fn scrape(State(state): State<AppState>) -> Response {
    match render(&state.db, &state.meters, state.clock.now()).await {
        Ok(text) => ([(header::CONTENT_TYPE, EXPOSITION)], text).into_response(),
        Err(error) => {
            warn!(%error, "the metrics could not be read from the database");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [(header::CONTENT_TYPE, PLAIN)],
                format!("the metrics could not be read from the database: {error}\n"),
            )
                .into_response()
        }
    }
}

pub(crate) async fn render(
    db: &DatabaseConnection,
    meters: &Meters,
    now: DateTime<Utc>,
) -> Result<String, bosun_db::Error> {
    let fractions = PERCENTILES.map(|(fraction, _)| fraction);
    let snapshot = bosun_db::metrics::read(
        db,
        now,
        Recent {
            since: now - RECENT,
            fractions: &fractions,
        },
    )
    .await?;
    let mut text = Exposition::default();
    database(&mut text, &snapshot);
    replica(&mut text, meters);
    Ok(text.0)
}

fn database(text: &mut Exposition, snapshot: &Snapshot) {
    text.family(
        "bosun_queue_jobs",
        "gauge",
        "Queued jobs, under each pool that may take them and their set of labels.",
    );
    for queued in &snapshot.queue {
        let labels = queued.labels.join(",");
        text.sample(
            "bosun_queue_jobs",
            &[("pool", &queued.pool), ("labels", &labels)],
            queued.jobs as f64,
        );
    }
    text.family(
        "bosun_queue_longest_wait_seconds",
        "gauge",
        "The time the longest-queued of those jobs has waited since it was queued.",
    );
    for queued in &snapshot.queue {
        let labels = queued.labels.join(",");
        text.sample(
            "bosun_queue_longest_wait_seconds",
            &[("pool", &queued.pool), ("labels", &labels)],
            queued.longest_wait,
        );
    }
    text.family(
        "bosun_jobs",
        "gauge",
        "Jobs by status; a matrix counts as its members.",
    );
    for (status, jobs) in &snapshot.jobs {
        text.sample("bosun_jobs", &[("status", status)], *jobs as f64);
    }
    durations(
        text,
        (
            "bosun_runs_completed_last_hour",
            "Runs that started and completed in the last hour.",
        ),
        (
            "bosun_run_duration_last_hour_seconds",
            "Percentiles of the durations of those runs from their start to their completion; \
             100 is the longest.",
        ),
        &snapshot.runs,
    );
    durations(
        text,
        (
            "bosun_jobs_completed_last_hour",
            "Jobs that ran on an agent and completed in the last hour.",
        ),
        (
            "bosun_job_duration_last_hour_seconds",
            "Percentiles of the durations of those jobs from their start to their completion; \
             100 is the longest.",
        ),
        &snapshot.jobs_that_ran,
    );
    text.family(
        "bosun_agents",
        "gauge",
        "Agents that are not deleted, by pool, state and whether they are drained.",
    );
    for agents in &snapshot.agents {
        let drain = agents.drain.to_string();
        text.sample(
            "bosun_agents",
            &[
                ("pool", &agents.pool),
                ("state", &agents.state),
                ("drain", &drain),
            ],
            agents.agents as f64,
        );
    }
    let mut kinds: Vec<(&str, u64, u64)> = Kind::ALL
        .into_iter()
        .map(|kind| {
            let name = outbox::name(kind);
            snapshot
                .outbox
                .iter()
                .find(|counted| counted.kind == name)
                .map_or((name, 0, 0), |counted| {
                    (name, counted.backlog, counted.dead)
                })
        })
        .collect();
    for counted in &snapshot.outbox {
        if !kinds.iter().any(|(name, _, _)| *name == counted.kind) {
            kinds.push((&counted.kind, counted.backlog, counted.dead));
        }
    }
    text.family(
        "bosun_outbox_backlog",
        "gauge",
        "Outbox rows that wait for their delivery to the forge.",
    );
    for (kind, backlog, _) in &kinds {
        text.sample("bosun_outbox_backlog", &[("kind", kind)], *backlog as f64);
    }
    text.family(
        "bosun_outbox_dead",
        "gauge",
        "Outbox rows that are dead and no longer tried.",
    );
    for (kind, _, dead) in &kinds {
        text.sample("bosun_outbox_dead", &[("kind", kind)], *dead as f64);
    }
}

fn durations(
    text: &mut Exposition,
    (count, count_help): (&str, &str),
    (name, help): (&str, &str),
    durations: &Durations,
) {
    text.family(count, "gauge", count_help);
    text.sample(count, &[], durations.completed as f64);
    text.family(name, "gauge", help);
    for ((_, percentile), seconds) in PERCENTILES.iter().zip(&durations.percentiles) {
        text.sample(name, &[("percentile", percentile)], *seconds);
    }
}

fn replica(text: &mut Exposition, meters: &Meters) {
    text.family(
        "bosun_outbox_failures_total",
        "counter",
        "Deliveries of outbox rows by this replica that failed, the last attempt of a row that \
         dies included.",
    );
    for (kind, failures) in Kind::ALL.into_iter().zip(&meters.outbox_failures) {
        text.sample(
            "bosun_outbox_failures_total",
            &[("kind", outbox::name(kind))],
            failures.load(Ordering::Relaxed) as f64,
        );
    }
    text.family(
        "bosun_log_lines_total",
        "counter",
        "Log lines this replica stored from its agents.",
    );
    text.sample(
        "bosun_log_lines_total",
        &[],
        meters.log_lines.load(Ordering::Relaxed) as f64,
    );
    text.family(
        "bosun_log_bytes_total",
        "counter",
        "Bytes of the log lines this replica stored from its agents.",
    );
    text.sample(
        "bosun_log_bytes_total",
        &[],
        meters.log_bytes.load(Ordering::Relaxed) as f64,
    );
    let name = "bosun_webhook_processing_seconds";
    text.family(
        name,
        "histogram",
        "The time the webhook worker of this replica took to process a delivery, whatever its \
         outcome.",
    );
    let histogram = &meters.processing;
    let observed = histogram
        .observed
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let bucket = format!("{name}_bucket");
    for (bound, count) in histogram.bounds.iter().zip(&observed.counts) {
        text.sample(&bucket, &[("le", &number(*bound))], *count as f64);
    }
    text.sample(
        &bucket,
        &[("le", &number(f64::INFINITY))],
        observed.count as f64,
    );
    text.sample(&format!("{name}_sum"), &[], observed.sum);
    text.sample(&format!("{name}_count"), &[], observed.count as f64);
    drop(observed);
    text.family(
        "bosun_webhook_verification_failures_total",
        "counter",
        "Webhook deliveries this replica refused for a missing or wrong X-Hub-Signature-256.",
    );
    text.sample(
        "bosun_webhook_verification_failures_total",
        &[],
        meters.verification_failures.load(Ordering::Relaxed) as f64,
    );
}

#[derive(Default)]
struct Exposition(String);

impl Exposition {
    fn family(&mut self, name: &str, kind: &str, help: &str) {
        let _ = writeln!(self.0, "# HELP {name} {}", escape(help, false));
        let _ = writeln!(self.0, "# TYPE {name} {kind}");
    }

    fn sample(&mut self, name: &str, labels: &[(&str, &str)], value: f64) {
        self.0.push_str(name);
        if !labels.is_empty() {
            self.0.push('{');
            for (index, (label, value)) in labels.iter().enumerate() {
                if index > 0 {
                    self.0.push(',');
                }
                let _ = write!(self.0, "{label}=\"{}\"", escape(value, true));
            }
            self.0.push('}');
        }
        let _ = writeln!(self.0, " {}", number(value));
    }
}

fn escape(text: &str, quoted: bool) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '"' if quoted => escaped.push_str("\\\""),
            other => escaped.push(other),
        }
    }
    escaped
}

fn number(value: f64) -> String {
    match value {
        f64::INFINITY => "+Inf".to_owned(),
        f64::NEG_INFINITY => "-Inf".to_owned(),
        value => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Exposition, number};

    #[test]
    fn label_values_and_help_are_escaped_as_the_text_format_asks() {
        let mut text = Exposition::default();
        text.family(
            "bosun_example",
            "gauge",
            "A \\ help\nover two lines with \"quotes\".",
        );
        text.sample(
            "bosun_example",
            &[("labels", "a\\b\"c\nd"), ("pool", "default")],
            2.5,
        );
        assert_eq!(
            text.0,
            "# HELP bosun_example A \\\\ help\\nover two lines with \"quotes\".\n\
             # TYPE bosun_example gauge\n\
             bosun_example{labels=\"a\\\\b\\\"c\\nd\",pool=\"default\"} 2.5\n"
        );
    }

    #[test]
    fn numbers_are_written_as_the_text_format_reads_them() {
        assert_eq!(number(3.0), "3");
        assert_eq!(number(0.025), "0.025");
        assert_eq!(number(f64::INFINITY), "+Inf");
        assert_eq!(number(f64::NEG_INFINITY), "-Inf");
        assert_eq!(number(f64::NAN), "NaN");
    }
}
