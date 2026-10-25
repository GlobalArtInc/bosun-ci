use std::fs;

use bosun_types::ir::Template;
use bosun_types::status::Conclusion;
use serde_json::{Value, json};

use crate::{job, run, step, template};

#[tokio::test(flavor = "multi_thread")]
async fn every_step_sees_its_github_context_the_runner_and_the_payload_at_the_event_path() {
    let directory = tempfile::tempdir().expect("a directory");
    let payload = json!({ "inputs": null, "ref": null, "workflow": "backup" });
    let mut job = job(
        directory.path(),
        vec![
            step(
                "printf '%s\\n' \"$GITHUB_EVENT_NAME\" \"$GITHUB_JOB\" \"$GITHUB_ACTION\" \
                 \"$GITHUB_WORKSPACE\" \"$RUNNER_NAME\" \"$RUNNER_OS\" \"$RUNNER_ARCH\" \
                 \"$RUNNER_TEMP\" \"$RUNNER_TOOL_CACHE\" \"$RUNNER_WORKSPACE\" \
                 \"$RUNNER_ENVIRONMENT\" \"$BOSUN_JOB_ID\" \"$GITHUB_ACTIONS\" \"$CI\"\n\
                 cp \"$GITHUB_EVENT_PATH\" \"$RUNNER_TEMP/../event.json\"",
            ),
            step(
                "printf '%s\\n' \"$GITHUB_ENV\" \"$GITHUB_OUTPUT\" \"$GITHUB_PATH\" \
                 \"$GITHUB_STATE\" \"$GITHUB_STEP_SUMMARY\" \"$GITHUB_EVENT_PATH\"",
            ),
            step("echo \"$GITHUB_OUTPUT\""),
        ],
    );
    job.contexts.github = json!({
        "event_name": "workflow_dispatch",
        "job": "backup",
        "workspace": "",
        "event": payload,
    })
    .as_object()
    .cloned()
    .expect("an object");
    let finished = run(job).await;
    assert_eq!(
        finished.conclusion,
        Conclusion::Success,
        "{:#?}",
        finished.events
    );
    let root = fs::canonicalize(directory.path()).expect("a canonical directory");
    let temp = root.join("work/_temp").display().to_string();
    assert_eq!(
        finished.lines(0),
        [
            "workflow_dispatch".to_owned(),
            "backup".to_owned(),
            "__run".to_owned(),
            root.join("work/globalart.keel/keel").display().to_string(),
            "runner1-1".to_owned(),
            "Linux".to_owned(),
            "X64".to_owned(),
            temp.clone(),
            root.join("tools").display().to_string(),
            root.join("work/globalart.keel").display().to_string(),
            "self-hosted".to_owned(),
            "0192f3a4-5b6c-7d8e-9f01-000000000009".to_owned(),
            "true".to_owned(),
            "true".to_owned(),
        ]
    );
    let copied: Value = serde_json::from_slice(
        &fs::read(root.join("work/event.json")).expect("the copied payload"),
    )
    .expect("JSON");
    assert_eq!(copied, payload);

    let files = finished.lines(1);
    let commands = format!("{temp}/_runner_file_commands/");
    let id = files[0]
        .strip_prefix(&format!("{commands}set_env_"))
        .unwrap_or_else(|| panic!("GITHUB_ENV is {}", files[0]));
    assert_eq!(
        files,
        [
            format!("{commands}set_env_{id}"),
            format!("{commands}set_output_{id}"),
            format!("{commands}add_path_{id}"),
            format!("{commands}save_state_{id}"),
            format!("{commands}step_summary_{id}"),
            format!("{temp}/_github_workflow/event.json"),
        ]
    );
    assert!(
        id.parse::<uuid::Uuid>()
            .is_ok_and(|id| id.get_version_num() == 7),
        "`{id}` is not a version 7 UUID"
    );
    assert_ne!(
        finished.lines(2),
        [format!("{commands}set_output_{id}")],
        "two steps share their file commands"
    );
    assert!(
        !root.join("work/_temp/_github_workflow").exists(),
        "the payload outlived its job"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_expressions_of_a_step_see_the_contexts_of_the_server_and_of_the_runner() {
    let directory = tempfile::tempdir().expect("a directory");
    let object = |value: Value| value.as_object().cloned().expect("an object");
    let mut first = step("");
    first.key = Some("first".parse().expect("a step id"));
    let mut second = step("");
    second.action = bosun_types::ir::Action::Run {
        script: template(
            "printf '%s\\n' '${{ github.event_name }} ${{ github.action }} ${{ needs.setup.result }} \
             ${{ needs.setup.outputs.label }} ${{ matrix.os }} ${{ strategy.job-index }} \
             ${{ inputs.level }} ${{ vars.REGION }} ${{ secrets.TOKEN }} ${{ bosun.run_id }}' \
             '${{ github.workspace }} ${{ runner.temp }}' \
             '${{ env.LEVEL }} ${{ job.status }} ${{ steps.first.outcome }}' \
             '${{ toJSON(runner) }}'",
        ),
    };
    let mut job = job(directory.path(), vec![first, second]);
    job.contexts.github = object(json!({ "event_name": "push", "job": "build", "workspace": "" }));
    job.contexts.needs =
        object(json!({ "setup": { "result": "success", "outputs": { "label": "gpu" } } }));
    job.contexts.matrix = json!({ "os": "linux" });
    job.contexts.strategy =
        json!({ "fail-fast": true, "job-index": 1, "job-total": 2, "max-parallel": 2 });
    job.contexts.inputs = object(json!({ "level": 3 }));
    job.contexts.vars = object(json!({ "REGION": "eu" }));
    job.contexts.secrets = object(json!({ "TOKEN": "hidden" }));
    job.contexts.bosun = object(json!({ "run_id": "r1" }));
    job.evaluated
        .env
        .insert("LEVEL".to_owned(), "high".to_owned());
    let finished = run(job).await;
    assert_eq!(
        finished.conclusion,
        Conclusion::Success,
        "{:#?}",
        finished.events
    );
    let root = fs::canonicalize(directory.path()).expect("a canonical directory");
    let workspace = root.join("work/globalart.keel/keel").display().to_string();
    let lines = finished.lines(1);
    assert_eq!(
        lines[..3],
        [
            "push __run success gpu linux 1 3 eu hidden r1".to_owned(),
            format!("{workspace} {}", root.join("work/_temp").display()),
            "high success success".to_owned(),
        ]
    );
    let runner: Value = serde_json::from_str(&lines[3..].join("\n")).expect("the runner context");
    assert_eq!(
        runner,
        json!({
            "os": "Linux",
            "arch": "X64",
            "name": "runner1-1",
            "environment": "self-hosted",
            "tool_cache": root.join("tools").display().to_string(),
            "temp": root.join("work/_temp").display().to_string(),
            "workspace": root.join("work/globalart.keel").display().to_string(),
        })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_job_env_overrides_the_proxies_of_the_agent_and_the_step_env_overrides_both() {
    let directory = tempfile::tempdir().expect("a directory");
    let mut overriding = step("echo \"$HTTP_PROXY $http_proxy $NO_PROXY\"");
    overriding
        .env
        .insert("NO_PROXY".to_owned(), Template::text("step"));
    let mut job = job(directory.path(), vec![overriding]);
    job.proxies = [
        ("HTTP_PROXY", "http://proxy:3128"),
        ("http_proxy", "http://proxy:3128"),
        ("NO_PROXY", "localhost"),
    ]
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .into();
    job.evaluated.env = [("http_proxy", "http://job:8080"), ("NO_PROXY", "job")]
        .map(|(name, value)| (name.to_owned(), value.to_owned()))
        .into();
    let finished = run(job).await;
    assert_eq!(
        finished.lines(0),
        ["http://proxy:3128 http://job:8080 step"]
    );
}
