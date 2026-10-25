use std::fmt::Debug;
use std::str::FromStr;

use bosun_types::actor::Actor;
use bosun_types::api::{
    Capability, ForgeAccess, ForgePermission, ForkPolicy, ProjectSettings, Reporting, TrustLevel,
    UpdateOrganisationRequest, UpdateProjectRequest, WorkflowSource,
};
use bosun_types::command::{Command, CommandRequest};
use bosun_types::diagnostic::Severity;
use bosun_types::error::ErrorCode;
use bosun_types::event::RunEvent;
use bosun_types::id::{JobId, RunId};
use bosun_types::ir::{self, Expr, Number, Segment, Shell, Span, Template, Trigger};
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
    refuses::<ForkPolicy>(json!("sometimes"));
    refuses::<Reporting>(json!("comments"));
    refuses::<ForgePermission>(json!("pull-requests"));
    refuses::<ForgeAccess>(json!("none"));
    let mut settings = fixture("api/project-settings.json");
    settings["token_permissions"]["administration"] = json!("write");
    refuses::<ProjectSettings>(settings);
    refuses::<WorkflowSource>(json!({ "kind": "repository" }));
    refuses::<Trigger>(json!({ "workflow_call": {} }));
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
    top["priority"] = json!("high");
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

    let mut defaults = workflow.clone();
    defaults["jobs"][0]["defaults"]["run"] = json!({});
    refuses::<ir::Workflow>(defaults);

    let mut secrets = workflow.clone();
    secrets["jobs"][0]["secrets"] = json!({ "named": ["A"], "inherit": true });
    refuses::<ir::Workflow>(secrets);

    let mut uses = workflow.clone();
    uses["jobs"][0]["kind"]["steps"][0]["action"] = json!({ "uses": {
        "owner": "actions", "repository": "checkout", "path": null, "ref": "v4"
    } });
    serde_json::from_value::<ir::Workflow>(uses.clone()).expect("a step that uses an action");
    uses["jobs"][0]["kind"]["steps"][0]["action"]["uses"]["sha"] = json!("0");
    refuses::<ir::Workflow>(uses);

    let mut scope = workflow.clone();
    scope["permissions"] = json!({ "scopes": { "contents": "read", "id-token": "write" } });
    serde_json::from_value::<ir::Workflow>(scope.clone()).expect("permissions by scope");
    scope["permissions"]["scopes"]["metadata"] = json!("read");
    refuses::<ir::Workflow>(scope);

    let mut span = workflow.clone();
    span["jobs"][0]["span"]["offset"] = json!(120);
    refuses::<ir::Workflow>(span);

    let mut trigger = workflow.clone();
    trigger["triggers"][0]["workflow_dispatch"]["outputs"] = json!({});
    refuses::<ir::Workflow>(trigger);

    let mut segment = workflow.clone();
    segment["env"]["CARGO_TERM_COLOR"] = json!([{ "expr": { "literal": true } }]);
    refuses::<ir::Workflow>(segment);

    let span = json!({ "line": 3, "column": 8, "end_line": 3, "end_column": 14 });
    let context = json!({ "context": { "name": "github", "span": span } });
    let mut expression = workflow.clone();
    expression["env"]["CARGO_TERM_COLOR"] = json!([{ "expression": context }]);
    serde_json::from_value::<ir::Workflow>(expression).expect("an expression segment");

    let mut node = workflow.clone();
    node["env"]["CARGO_TERM_COLOR"] =
        json!([{ "expression": { "variable": { "name": "github", "span": span } } }]);
    refuses::<ir::Workflow>(node);

    let mut field = workflow.clone();
    field["env"]["CARGO_TERM_COLOR"] = json!([{ "expression": {
        "context": { "name": "github", "span": span, "case": "upper" }
    } }]);
    refuses::<ir::Workflow>(field);

    let mut number = workflow;
    number["env"]["CARGO_TERM_COLOR"] =
        json!([{ "expression": { "number": { "value": "nan", "span": span } } }]);
    refuses::<ir::Workflow>(number);
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
    let span = Span {
        line: 1,
        column: 8,
        end_line: 1,
        end_column: 11,
    };
    let computed = Template::new(vec![
        Segment::Text("echo ".into()),
        Segment::Expression(Expr::Number {
            value: Number(f64::NAN),
            span,
        }),
    ]);
    assert_eq!(computed.as_literal(), None);
    assert_eq!(
        serde_json::to_value(&computed).expect("serialisable"),
        json!([
            { "text": "echo " },
            { "expression": { "number": { "value": "NaN", "span": span } } }
        ])
    );
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

#[test]
fn an_update_tells_a_field_left_out_from_a_field_set_to_null() {
    let read = |value: Value| -> UpdateProjectRequest {
        serde_json::from_value(value).unwrap_or_else(|error| panic!("an update: {error}"))
    };
    assert_eq!(read(json!({})).repository, None);
    assert_eq!(read(json!({ "repository": null })).repository, Some(None));
    assert_eq!(
        read(json!({ "repository": "GlobalArtInc/ops" })).repository,
        Some(Some("GlobalArtInc/ops".to_owned()))
    );
    assert_eq!(
        serde_json::to_value(read(json!({}))).expect("JSON"),
        json!({})
    );
    assert_eq!(
        serde_json::to_value(read(json!({ "repository": null }))).expect("JSON"),
        json!({ "repository": null })
    );
    refuses::<UpdateProjectRequest>(json!({ "name": "Operations" }));
    refuses::<UpdateProjectRequest>(json!({ "repository": 7 }));

    let installation: UpdateOrganisationRequest =
        serde_json::from_value(json!({ "installation": null })).expect("an update");
    assert_eq!(installation.installation, Some(None));
    refuses::<UpdateOrganisationRequest>(json!({ "installation": "GlobalArtInc" }));
    refuses::<UpdateOrganisationRequest>(json!({ "slug": "globalart" }));
}
