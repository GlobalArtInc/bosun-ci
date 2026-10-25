use std::fmt::Debug;
use std::str::FromStr;

use bosun_types::actor::Actor;
use bosun_types::api::{Capability, TrustLevel, WorkflowSource};
use bosun_types::command::{Command, CommandRequest};
use bosun_types::diagnostic::Severity;
use bosun_types::error::ErrorCode;
use bosun_types::event::RunEvent;
use bosun_types::id::{JobId, RunId};
use bosun_types::ir::{self, Segment, Shell, Template, Trigger};
use bosun_types::key::{JobKey, StepKey};
use bosun_types::slug;
use bosun_types::status::{
    Conclusion, ConclusionSource, ExecutionStatus, JobStatus, RunConclusion, RunStatus,
    StatusReason, StepStatus, WaitingReason,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::fixture;

fn refuses<T: DeserializeOwned + Debug>(value: Value) {
    let result = serde_json::from_value::<T>(value.clone());
    assert!(
        result.is_err(),
        "{} accepted {value}: {result:?}",
        std::any::type_name::<T>()
    );
}

#[test]
fn closed_enums_refuse_unknown_values() {
    refuses::<RunStatus>(json!("running"));
    refuses::<JobStatus>(json!("running"));
    refuses::<ExecutionStatus>(json!("pending"));
    refuses::<StepStatus>(json!("held"));
    refuses::<Conclusion>(json!("startup_failure"));
    refuses::<RunConclusion>(json!("neutral"));
    refuses::<ConclusionSource>(json!("plugin"));
    refuses::<WaitingReason>(json!("no_agent"));
    refuses::<StatusReason>(json!("timeout"));
    refuses::<ErrorCode>(json!("teapot"));
    refuses::<Severity>(json!("info"));
    refuses::<Shell>(json!("zsh"));
    refuses::<TrustLevel>(json!("semi"));
    refuses::<Capability>(json!("kubernetes"));
    refuses::<WorkflowSource>(json!({ "kind": "repository" }));
    refuses::<Trigger>(json!({ "push": {} }));
    refuses::<Actor>(json!({ "kind": "plugin", "instance": "telegram" }));
    refuses::<Command>(json!({ "type": "run.pause" }));

    let mut event = fixture("events/job-status.json");
    event["kind"] = json!("job.teleported");
    refuses::<RunEvent>(event);

    let mut status = fixture("events/job-status.json");
    status["data"]["to"] = json!("running");
    refuses::<RunEvent>(status);
}

#[test]
fn the_ir_refuses_unknown_fields() {
    let workflow = fixture("ir/two-jobs.json");
    serde_json::from_value::<ir::Workflow>(workflow.clone()).expect("the fixture itself is valid");

    let mut top = workflow.clone();
    top["concurrency"] = json!({ "group": "ci" });
    refuses::<ir::Workflow>(top);

    let mut job = workflow.clone();
    job["jobs"][0]["retry"] = json!({ "attempts": 2 });
    refuses::<ir::Workflow>(job);

    let mut step = workflow.clone();
    step["jobs"][0]["kind"]["steps"][0]["pause"] = json!("before");
    refuses::<ir::Workflow>(step);

    let mut runs_on = workflow.clone();
    runs_on["jobs"][0]["runs_on"]["literal"]["pool"] = json!("default");
    refuses::<ir::Workflow>(runs_on);

    let mut span = workflow.clone();
    span["jobs"][0]["span"]["offset"] = json!(120);
    refuses::<ir::Workflow>(span);

    let mut trigger = workflow.clone();
    trigger["triggers"][0]["workflow_dispatch"]["inputs"] = json!({});
    refuses::<ir::Workflow>(trigger);

    let mut segment = workflow;
    segment["env"]["CARGO_TERM_COLOR"] = json!([{ "expr": { "literal": true } }]);
    refuses::<ir::Workflow>(segment);
}

#[test]
fn identifiers_accept_only_version_7_uuids() {
    let v7 = "0192f3a4-5b6c-7d8e-9f01-000000000007";
    assert_eq!(RunId::from_str(v7).expect("a v7 UUID").to_string(), v7);
    assert!(RunId::from_str("2f1b6e1e-6a5b-4c3d-9e8f-0a1b2c3d4e5f").is_err());
    assert!(RunId::from_str("0192f3a4-5b6c-7d8e-1f01-000000000007").is_err());
    assert!(RunId::from_str("not-a-uuid").is_err());
    refuses::<JobId>(json!("2f1b6e1e-6a5b-4c3d-9e8f-0a1b2c3d4e5f"));
    refuses::<JobId>(json!(7));
}

#[test]
fn keys_follow_the_identifier_pattern() {
    for valid in ["build", "_private", "test-unit_2", "B"] {
        assert_eq!(JobKey::from_str(valid).expect(valid).as_str(), valid);
        assert_eq!(StepKey::from_str(valid).expect(valid).as_str(), valid);
    }
    for invalid in ["", "1build", "-x", "build job", "build.x", "сборка"] {
        assert!(JobKey::from_str(invalid).is_err(), "{invalid:?}");
        assert!(StepKey::from_str(invalid).is_err(), "{invalid:?}");
        refuses::<JobKey>(json!(invalid));
    }
}

#[test]
fn slugs_follow_the_rule_of_logins() {
    let longest = "a".repeat(64);
    for valid in ["globalart", "keel", "0day", "a.b-c_d", longest.as_str()] {
        assert!(slug::is_slug(valid), "{valid:?}");
    }
    let too_long = "a".repeat(65);
    for invalid in [
        "",
        "Keel",
        "-keel",
        ".keel",
        "_keel",
        "../escape",
        "a b",
        "ключ",
        &too_long,
    ] {
        assert!(!slug::is_slug(invalid), "{invalid:?}");
    }
}

#[test]
fn a_shell_is_parsed_from_the_text_it_displays() {
    for (text, shell) in [
        ("bash", Shell::Bash),
        ("sh", Shell::Sh),
        ("pwsh", Shell::Pwsh),
        ("python", Shell::Python),
        (
            "bash -c \"{0}\"",
            Shell::Custom("bash -c \"{0}\"".to_owned()),
        ),
        ("python3 {0}", Shell::Custom("python3 {0}".to_owned())),
    ] {
        assert_eq!(Shell::from_str(text).as_ref(), Ok(&shell), "{text}");
        assert_eq!(shell.to_string(), text);
    }
    for refused in ["perl -w", "zsh", ""] {
        let error = Shell::from_str(refused).expect_err(refused);
        assert_eq!(
            error.to_string(),
            format!(
                "`{refused}`, which is neither `bash`, `sh`, `pwsh` nor `python` and has no `{{0}}` \
                 for the script"
            )
        );
    }
}

#[test]
fn a_template_of_text_is_a_literal() {
    let template = Template::new(vec![
        Segment::Text("cargo ".into()),
        Segment::Text("test".into()),
    ]);
    assert_eq!(template.as_literal().as_deref(), Some("cargo test"));
    assert_eq!(Template::default().as_literal().as_deref(), Some(""));
    assert_eq!(
        serde_json::to_value(Template::text("echo")).expect("serialisable"),
        json!([{ "text": "echo" }])
    );
}

#[test]
fn commands_without_arguments_need_no_args_field() {
    let request: CommandRequest = serde_json::from_value(json!({
        "type": "job.cancel",
        "target": { "attempt": null, "job": null, "step": null },
        "reason": null
    }))
    .expect("a command without args");
    assert_eq!(request.command, Command::JobCancel);
}

#[test]
fn run_conclusions_include_every_job_conclusion() {
    for conclusion in [
        Conclusion::Success,
        Conclusion::Failure,
        Conclusion::Cancelled,
        Conclusion::Skipped,
        Conclusion::TimedOut,
    ] {
        assert_eq!(
            crate::name(RunConclusion::from(conclusion)),
            crate::name(conclusion)
        );
    }
}
