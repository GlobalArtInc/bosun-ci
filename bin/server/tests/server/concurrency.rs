use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::time::Duration;

use bosun_proto::agent::v1::agent_message::Message as Report;
use bosun_proto::agent::v1::server_message::Message as Incoming;
use bosun_proto::agent::v1::{Accept, Conclusion};
use serde_json::{Value, json};
use tokio::task::JoinHandle;

use crate::api::text;
use crate::client;
use crate::queue::Cluster;

const RUNS: usize = 24;

const AGENTS: usize = 4;

const WORKFLOWS: usize = 6;

const ACTIVE: [&str; 3] = ["queued", "assigned", "in_progress"];

fn workflow(concurrency: &str) -> String {
    format!(
        "name: deploy\non: workflow_dispatch\nconcurrency:\n{concurrency}jobs:\n  build:\n    \
         runs-on: [self-hosted]\n    steps:\n      - run: \"true\"\n  ship:\n    needs: build\n    \
         runs-on: [self-hosted]\n    steps:\n      - run: \"true\"\n"
    )
}

async fn dispatch(address: SocketAddr, owner: String, workflow: String) -> String {
    let response = client()
        .post(format!(
            "http://{address}/api/v1/workflows/{workflow}/dispatch"
        ))
        .bearer_auth(owner)
        .json(&json!({ "ref": null, "inputs": {} }))
        .send()
        .await
        .unwrap_or_else(|error| panic!("a dispatch through {address}: {error}"));
    let status = response.status().as_u16();
    let body: Value = response.json().await.expect("a JSON body");
    assert_eq!(status, 201, "a dispatch through {address}: {body}");
    text(&body["id"]).to_owned()
}

async fn agents(cluster: &Cluster) -> Vec<JoinHandle<()>> {
    let mut tasks = Vec::new();
    for number in 0..AGENTS {
        let address = cluster.addresses[number % cluster.addresses.len()];
        let fake = cluster.agent(&format!("runner{number}"), &[]).await;
        let mut connection = fake.connect(address).await;
        tasks.push(tokio::spawn(async move {
            connection.idle();
            loop {
                let assignment = match connection.message().await {
                    Ok(Some(Incoming::Assignment(assignment))) => assignment,
                    Ok(Some(_)) => continue,
                    Ok(None) | Err(_) => return,
                };
                let execution = assignment.execution_id;
                connection.send(Report::Accept(Accept {
                    execution_id: execution.clone(),
                }));
                let conclusion = match connection.within(Duration::from_millis(20)).await {
                    Some(Ok(Some(Incoming::Control(control)))) => {
                        connection.acknowledge(&control, true);
                        Conclusion::Cancelled
                    }
                    _ => Conclusion::Success,
                };
                connection.finish_job(&execution, conclusion, &[]);
                connection.idle();
            }
        }));
    }
    tasks
}

async fn workflows(cluster: &Cluster, concurrency: &str) -> Vec<String> {
    cluster
        .post(
            "/api/v1/organisations",
            &json!({ "slug": "globalart", "name": "GlobalArt" }),
        )
        .await;
    let project = cluster
        .post(
            "/api/v1/projects",
            &json!({ "organisation": "globalart", "slug": "ops", "name": "Operations" }),
        )
        .await;
    let mut created = Vec::new();
    for number in 0..WORKFLOWS {
        let stored = cluster
            .post(
                &format!("/api/v1/projects/{}/workflows", text(&project["id"])),
                &json!({ "name": format!("deploy-{number}"), "source": workflow(concurrency) }),
            )
            .await;
        created.push(text(&stored["id"]).to_owned());
    }
    created
}

async fn race(cluster: &Cluster, workflows: &[String]) -> Vec<String> {
    let mut dispatches = Vec::new();
    for number in 0..RUNS {
        let address = cluster.addresses[number % cluster.addresses.len()];
        dispatches.push(tokio::spawn(dispatch(
            address,
            cluster.owner.clone(),
            workflows[number % workflows.len()].clone(),
        )));
    }
    let mut runs = Vec::new();
    for dispatched in dispatches {
        runs.push(dispatched.await.expect("a dispatch task"));
    }
    runs
}

async fn never_two_running(cluster: &Cluster) {
    let events = cluster
        .values(
            "SELECT concat_ws(' ', run_id, job_id, data->>'to') AS value FROM bosun.run_event \
             WHERE kind = 'job.status' ORDER BY seq",
        )
        .await;
    assert!(
        events.len() >= 2 * RUNS,
        "the runs moved their jobs {} times",
        events.len()
    );
    let mut jobs: BTreeMap<(String, String), String> = BTreeMap::new();
    let mut overlaps = Vec::new();
    for event in &events {
        let parts: Vec<&str> = event.split(' ').collect();
        let [run, job, to] = parts.as_slice() else {
            panic!("a job.status event without a run, a job or a status: {event}");
        };
        jobs.insert(((*run).to_owned(), (*job).to_owned()), (*to).to_owned());
        let running: BTreeSet<&str> = jobs
            .iter()
            .filter(|(_, status)| ACTIVE.contains(&status.as_str()))
            .map(|((run, _), _)| run.as_str())
            .collect();
        if running.len() > 1 {
            overlaps.push(format!("{event}: {running:?}"));
        }
    }
    assert!(
        overlaps.is_empty(),
        "two members of the group ran at once: {overlaps:#?}"
    );
}

#[tokio::test]
async fn dispatches_into_one_group_through_two_replicas_never_leave_two_members_running() {
    let cluster = Cluster::start(2, "").await;
    let workflows = workflows(&cluster, "  group: deploy\n  queue: max\n").await;
    let tasks = agents(&cluster).await;
    let runs = race(&cluster, &workflows).await;
    assert_eq!(runs.len(), RUNS);
    cluster
        .until(
            "SELECT concat_ws(' ', status, conclusion, count(*)) AS value FROM bosun.run \
             GROUP BY status, conclusion",
            &[&format!("completed success {RUNS}")],
        )
        .await;
    for task in &tasks {
        task.abort();
    }
    never_two_running(&cluster).await;
    assert_eq!(
        cluster
            .values("SELECT count(*)::text AS value FROM bosun.concurrency_member")
            .await,
        ["0"]
    );
}

#[tokio::test]
async fn arrivals_that_cancel_through_two_replicas_never_leave_two_members_running() {
    let cluster = Cluster::start(2, "").await;
    let workflows = workflows(&cluster, "  group: deploy\n  cancel-in-progress: true\n").await;
    let tasks = agents(&cluster).await;
    let runs = race(&cluster, &workflows).await;
    assert_eq!(runs.len(), RUNS);
    cluster
        .until(
            "SELECT count(*)::text AS value FROM bosun.run WHERE status <> 'completed'",
            &["0"],
        )
        .await;
    for task in &tasks {
        task.abort();
    }
    never_two_running(&cluster).await;
    let concluded = cluster
        .values(
            "SELECT concat_ws(' ', conclusion, count(*)) AS value FROM bosun.run \
             GROUP BY conclusion ORDER BY conclusion",
        )
        .await;
    assert!(
        concluded
            .iter()
            .all(|row| row.starts_with("cancelled ") || row.starts_with("success ")),
        "the runs ended {concluded:?}"
    );
    assert!(
        concluded.iter().any(|row| row.starts_with("success ")),
        "the last arrival did not succeed: {concluded:?}"
    );
    assert_eq!(
        cluster
            .values("SELECT count(*)::text AS value FROM bosun.concurrency_member")
            .await,
        ["0"]
    );
}
