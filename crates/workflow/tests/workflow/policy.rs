use bosun_types::action_policy::ActionPolicy;
use bosun_types::ir::{Action, ActionReference, JobKind};
use serde_json::json;

use crate::fixtures::{fixtures, json};
use crate::parsed;

fn lowered(uses: &str) -> ActionReference {
    let source = json!({
        "on": "workflow_dispatch",
        "jobs": {
            "build": {
                "runs-on": "ubuntu-latest",
                "steps": [{ "uses": uses }]
            }
        }
    })
    .to_string();
    let workflow = parsed(&source).workflow;
    let JobKind::Steps(steps) = &workflow.jobs[0].kind else {
        panic!("a job of steps");
    };
    match &steps[0].action {
        Action::Uses(reference) => reference.clone(),
        other => panic!("`{uses}` lowered to {other:?}"),
    }
}

#[test]
fn every_reference_of_the_policy_table_reaches_the_function_as_the_workflow_parser_lowers_it() {
    let table = json(&fixtures().join("action-policy/cases.json"));
    let cases = table["cases"].as_array().expect("cases");
    assert!(!cases.is_empty());
    for case in cases {
        let policy: ActionPolicy =
            serde_json::from_value(case["policy"].clone()).expect("a policy");
        let uses = case["uses"].as_str().expect("a reference");
        let account = case["account"].as_str();
        let allowed = case["allowed"].as_bool().expect("a verdict");
        assert_eq!(
            policy.permits(account, &lowered(uses)),
            allowed,
            "{}",
            case["description"]
        );
    }
}
