use std::fs;

use bosun_types::status::Conclusion;
use serde_json::{Value, json};

use crate::{job, run, step};

#[tokio::test(flavor = "multi_thread")]
async fn every_step_sees_its_github_context_the_runner_and_the_payload_at_the_event_path() {
    let directory = tempfile::tempdir().expect("a directory");
    let payload = json!({ "inputs": null, "ref": null, "workflow": "backup" });
    let mut job = job(
        directory.path(),
        vec![
            step(
                0,
                "printf '%s\\n' \"$GITHUB_EVENT_NAME\" \"$GITHUB_JOB\" \"$GITHUB_ACTION\" \
                 \"$GITHUB_WORKSPACE\" \"$RUNNER_NAME\" \"$RUNNER_OS\" \"$RUNNER_ARCH\" \
                 \"$RUNNER_TEMP\" \"$RUNNER_TOOL_CACHE\" \"$RUNNER_WORKSPACE\" \
                 \"$RUNNER_ENVIRONMENT\" \"$BOSUN_JOB_ID\" \"$GITHUB_ACTIONS\" \"$CI\"\n\
                 cp \"$GITHUB_EVENT_PATH\" \"$RUNNER_TEMP/../event.json\"",
            ),
            step(
                1,
                "printf '%s\\n' \"$GITHUB_ENV\" \"$GITHUB_OUTPUT\" \"$GITHUB_PATH\" \
                 \"$GITHUB_STATE\" \"$GITHUB_STEP_SUMMARY\" \"$GITHUB_EVENT_PATH\"",
            ),
            step(2, "echo \"$GITHUB_OUTPUT\""),
        ],
    );
    job.github = json!({
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
