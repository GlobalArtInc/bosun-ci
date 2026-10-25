use std::collections::BTreeSet;
use std::io;

use bosun_types::ir;
use bosun_types::status::Conclusion;
use chrono::{DateTime, TimeDelta, Utc};
use prometheus_parse::{LineInfo, Sample, Scrape, Value};
use sea_orm::DatabaseConnection;

use super::access::{document, quoted, table};
use super::reports::{Scene, one_job};
use super::{World, finished};
use crate::metrics::{Meters, render};

const SERIES: &str = "| Series |";

const LABELS: &str = "linux,self-hosted";

struct Documented {
    name: String,
    kind: String,
    labels: BTreeSet<String>,
}

fn documented() -> Vec<Documented> {
    table(&document("docs/design/12-operations.md"), SERIES)
        .iter()
        .map(|row| Documented {
            name: quoted(&row[0])
                .pop()
                .unwrap_or_else(|| panic!("`{}` names no series", row[0])),
            kind: row[1].clone(),
            labels: quoted(&row[2]).into_iter().collect(),
        })
        .collect()
}

async fn exposition(db: &DatabaseConnection, meters: &Meters, now: DateTime<Utc>) -> String {
    render(db, meters, now)
        .await
        .unwrap_or_else(|error| panic!("the metrics could not be read: {error}"))
}

fn parse(text: &str) -> Scrape {
    for line in text.lines() {
        if line.starts_with("# HELP ") || line.starts_with("# TYPE ") {
            continue;
        }
        assert!(
            matches!(LineInfo::parse(line), LineInfo::Sample { .. }),
            "`{line}` is not a sample"
        );
    }
    Scrape::parse(text.lines().map(|line| Ok::<_, io::Error>(line.to_owned())))
        .expect("an exposition the parser reads")
}

fn value(text: &str, series: &str) -> f64 {
    let prefix = format!("{series} ");
    let line = text
        .lines()
        .find(|line| line.starts_with(&prefix))
        .unwrap_or_else(|| panic!("no `{series}` in\n{text}"));
    line[prefix.len()..]
        .parse()
        .unwrap_or_else(|error| panic!("`{line}`: {error}"))
}

fn names(sample: &Sample) -> BTreeSet<String> {
    sample.labels.keys().cloned().collect()
}

fn queued(series: &str) -> String {
    format!("{series}{{pool=\"default\",labels=\"{LABELS}\"}}")
}

fn agents(state: &str, drain: bool) -> String {
    format!("bosun_agents{{pool=\"default\",state=\"{state}\",drain=\"{drain}\"}}")
}

#[tokio::test]
async fn every_series_of_the_table_appears_in_an_exposition_that_a_prometheus_parser_reads() {
    let world = World::new().await;
    world.set_online(true).await;
    let run = world.dispatch(&one_job()).await;
    world
        .finish(run, "build", finished(Conclusion::Success))
        .await;
    world.dispatch(&one_job()).await;
    let text = exposition(world.db(), &Meters::default(), Utc::now()).await;
    let scrape = parse(&text);
    let series = documented();
    assert!(!series.is_empty(), "the table of series is empty");
    for documented in &series {
        let name = &documented.name;
        assert!(
            text.contains(&format!("# TYPE {name} {}\n", documented.kind)),
            "`{name}` is not declared a {} in\n{text}",
            documented.kind
        );
        assert!(scrape.docs.contains_key(name), "`{name}` has no HELP");
        let samples: Vec<&Sample> = scrape
            .samples
            .iter()
            .filter(|sample| sample.metric == *name)
            .collect();
        assert!(!samples.is_empty(), "`{name}` has no sample in\n{text}");
        for sample in &samples {
            let typed = match (&sample.value, documented.kind.as_str()) {
                (Value::Gauge(_), "gauge") | (Value::Counter(_), "counter") => true,
                (Value::Histogram(buckets), "histogram") => buckets
                    .last()
                    .is_some_and(|last| last.less_than == f64::INFINITY),
                _ => false,
            };
            assert!(typed, "`{name}` is not a {}: {sample:?}", documented.kind);
            assert_eq!(
                names(sample),
                documented.labels,
                "the labels of `{name}` differ from the table"
            );
        }
        if documented.kind == "histogram" {
            for part in ["sum", "count"] {
                let part = format!("{name}_{part}");
                assert!(
                    scrape.samples.iter().any(|sample| sample.metric == part),
                    "`{name}` has no `{part}`"
                );
            }
        }
    }
    let declared: BTreeSet<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("# TYPE "))
        .filter_map(|line| line.split(' ').next())
        .collect();
    let listed: BTreeSet<&str> = series.iter().map(|series| series.name.as_str()).collect();
    assert_eq!(
        declared, listed,
        "the exposition and the table of docs/design/12-operations.md name other series"
    );
}

#[tokio::test]
async fn a_queued_job_an_online_agent_and_a_row_the_forge_refuses_for_a_day_show_in_the_metrics() {
    let scene = Scene::new().await;
    scene.world.set_online(true).await;
    scene.fake.fail(true);
    scene.dispatch(&one_job()).await;
    for hour in 0..24 {
        assert_eq!(scene.drain().await, 1, "hour {hour}");
        scene.tick(TimeDelta::hours(1));
    }
    assert_eq!(scene.drain().await, 1);

    let text = exposition(scene.world.db(), &scene.outbox.meters, scene.clock.now()).await;
    parse(&text);
    assert_eq!(value(&text, &queued("bosun_queue_jobs")), 1.0, "{text}");
    let waited = value(&text, &queued("bosun_queue_longest_wait_seconds"));
    let expected = (TimeDelta::hours(24) + TimeDelta::minutes(10)).num_seconds() as f64;
    assert!(
        waited <= expected + 5.0 && waited > expected - 600.0,
        "the job waited {waited}s by the injected clock"
    );
    assert_eq!(value(&text, "bosun_jobs{status=\"queued\"}"), 1.0);
    for state in ["offline", "idle", "busy"] {
        for drain in [false, true] {
            let expected = if (state, drain) == ("idle", false) {
                1.0
            } else {
                0.0
            };
            assert_eq!(value(&text, &agents(state, drain)), expected, "{text}");
        }
    }
    assert_eq!(value(&text, "bosun_outbox_dead{kind=\"check_run\"}"), 1.0);
    assert_eq!(
        value(&text, "bosun_outbox_backlog{kind=\"check_run\"}"),
        0.0
    );
    assert_eq!(
        value(&text, "bosun_outbox_failures_total{kind=\"check_run\"}"),
        25.0
    );
    for kind in ["commit_status", "fork_status"] {
        assert_eq!(
            value(&text, &format!("bosun_outbox_dead{{kind=\"{kind}\"}}")),
            0.0
        );
    }
}

fn parsed(source: &str) -> ir::Workflow {
    bosun_workflow::parse("ci", source)
        .unwrap_or_else(|diagnostics| panic!("{source}\n{diagnostics:#?}"))
        .workflow
}

#[tokio::test]
async fn a_matrix_counts_as_its_members_and_a_job_no_pool_takes_is_queued_under_no_pool() {
    let world = World::new().await;
    world
        .dispatch(&parsed(
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: [self-hosted, Linux]\n    \
             strategy:\n      matrix:\n        node: [20, 24]\n    steps:\n      - run: echo \
             ${{ matrix.node }}\n  deploy:\n    runs-on:\n      group: elsewhere\n      labels: \
             [self-hosted]\n    steps:\n      - run: echo deploy\n",
        ))
        .await;
    let text = exposition(world.db(), &Meters::default(), Utc::now()).await;
    assert_eq!(value(&text, &queued("bosun_queue_jobs")), 2.0, "{text}");
    assert_eq!(
        value(&text, "bosun_queue_jobs{pool=\"\",labels=\"self-hosted\"}"),
        1.0,
        "{text}"
    );
    assert_eq!(value(&text, "bosun_jobs{status=\"queued\"}"), 3.0);
    assert_eq!(value(&text, "bosun_jobs{status=\"pending\"}"), 0.0);
}

#[tokio::test]
async fn a_job_that_ran_counts_in_the_durations_for_an_hour_and_its_agent_is_busy_meanwhile() {
    let world = World::new().await;
    world.set_online(true).await;
    let run = world.dispatch(&one_job()).await;
    let (job, _) = world.project(run).await.job("build");
    let execution = world.start(run, job).await;
    let meters = Meters::default();
    let text = exposition(world.db(), &meters, Utc::now()).await;
    assert_eq!(value(&text, &agents("busy", false)), 1.0, "{text}");
    assert_eq!(value(&text, &agents("idle", false)), 0.0);
    assert_eq!(value(&text, "bosun_jobs{status=\"in_progress\"}"), 1.0);
    world.end(execution, finished(Conclusion::Success)).await;

    let text = exposition(world.db(), &meters, Utc::now()).await;
    for (count, durations) in [
        (
            "bosun_runs_completed_last_hour",
            "bosun_run_duration_last_hour_seconds",
        ),
        (
            "bosun_jobs_completed_last_hour",
            "bosun_job_duration_last_hour_seconds",
        ),
    ] {
        assert_eq!(value(&text, count), 1.0, "{text}");
        let mut previous = 0.0;
        for percentile in ["50", "90", "99", "100"] {
            let seconds = value(
                &text,
                &format!("{durations}{{percentile=\"{percentile}\"}}"),
            );
            assert!(
                (previous..10.0).contains(&seconds),
                "the {percentile}th percentile is {seconds}s in\n{text}"
            );
            previous = seconds;
        }
    }
    assert_eq!(value(&text, &agents("idle", false)), 1.0);
    assert_eq!(value(&text, "bosun_jobs{status=\"completed\"}"), 1.0);

    let later = exposition(world.db(), &meters, Utc::now() + TimeDelta::hours(2)).await;
    for (count, durations) in [
        (
            "bosun_runs_completed_last_hour",
            "bosun_run_duration_last_hour_seconds",
        ),
        (
            "bosun_jobs_completed_last_hour",
            "bosun_job_duration_last_hour_seconds",
        ),
    ] {
        assert_eq!(value(&later, count), 0.0, "{later}");
        assert!(
            !later
                .lines()
                .any(|line| line.starts_with(&format!("{durations}{{"))),
            "{later}"
        );
    }
}
