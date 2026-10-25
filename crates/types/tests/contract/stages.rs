use std::collections::BTreeMap;

use bosun_types::ir::{
    Action, ActionReference, Evaluable, NodeRuns, NodeRuntime, Phase, Span, Stage, Step, Template,
    stages,
};

fn step(action: Action) -> Step {
    Step {
        key: None,
        name: None,
        r#if: None,
        action,
        shell: None,
        working_directory: None,
        with: BTreeMap::new(),
        env: BTreeMap::new(),
        continue_on_error: Evaluable::Literal(false),
        timeout_minutes: None,
        span: Span {
            line: 1,
            column: 1,
            end_line: 1,
            end_column: 1,
        },
    }
}

fn uses(repository: &str) -> Step {
    step(Action::Uses(ActionReference {
        owner: "bosun-fixtures".to_owned(),
        repository: repository.to_owned(),
        path: None,
        r#ref: "v1".to_owned(),
    }))
}

fn runs(pre: bool, post: bool) -> NodeRuns {
    NodeRuns {
        using: NodeRuntime::Node24,
        main: "main.js".to_owned(),
        pre: pre.then(|| "pre.js".to_owned()),
        pre_if: None,
        post: post.then(|| "post.js".to_owned()),
        post_if: None,
    }
}

fn stage(phase: Phase, step: u32) -> Stage {
    Stage { phase, step }
}

#[test]
fn pre_steps_come_first_in_step_order_and_post_steps_last_in_reverse() {
    let both = runs(true, true);
    let post = runs(false, true);
    let plain = runs(false, false);
    let steps = vec![
        uses("both"),
        step(Action::Run {
            script: Template::text("make"),
        }),
        uses("post"),
        uses("plain"),
        uses("both"),
        uses("unknown"),
    ];
    let found = stages(&steps, |reference| match reference.repository.as_str() {
        "both" => Some(&both),
        "post" => Some(&post),
        "plain" => Some(&plain),
        _ => None,
    });
    assert_eq!(
        found,
        vec![
            stage(Phase::Pre, 0),
            stage(Phase::Pre, 4),
            stage(Phase::Main, 0),
            stage(Phase::Main, 1),
            stage(Phase::Main, 2),
            stage(Phase::Main, 3),
            stage(Phase::Main, 4),
            stage(Phase::Main, 5),
            stage(Phase::Post, 4),
            stage(Phase::Post, 2),
            stage(Phase::Post, 0),
        ]
    );
    assert_eq!(
        serde_json::to_value(found[0]).expect("JSON"),
        serde_json::json!({ "phase": "pre", "step": 0 })
    );
    assert_eq!(stages(&[], |_| None), Vec::new());
}
