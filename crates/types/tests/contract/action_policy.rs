use bosun_types::action_policy::{ActionPattern, ActionPolicy, LONGEST_PATTERN, MOST_PATTERNS};
use bosun_types::ir::ActionReference;
use serde::Deserialize;
use serde_json::json;

use crate::fixture;

#[derive(Deserialize)]
struct Cases {
    cases: Vec<Case>,
    patterns: Patterns,
    limits: Limits,
}

#[derive(Deserialize)]
struct Limits {
    most_patterns: usize,
    longest_pattern: usize,
}

#[derive(Deserialize)]
struct Case {
    description: String,
    policy: ActionPolicy,
    account: Option<String>,
    uses: String,
    allowed: bool,
}

#[derive(Deserialize)]
struct Patterns {
    valid: Vec<Valid>,
    invalid: Vec<String>,
}

#[derive(Deserialize)]
struct Valid {
    text: String,
    normalised: String,
}

fn reference(uses: &str) -> ActionReference {
    let (name, git_ref) = uses.split_once('@').expect("a ref");
    let mut parts = name.split('/');
    let owner = parts.next().expect("an owner");
    let repository = parts.next().expect("a repository");
    let path: Vec<&str> = parts.collect();
    ActionReference {
        owner: owner.to_owned(),
        repository: repository.to_owned(),
        path: (!path.is_empty()).then(|| path.join("/")),
        r#ref: git_ref.to_owned(),
    }
}

fn cases() -> Cases {
    serde_json::from_value(fixture("action-policy/cases.json")).expect("the cases")
}

#[test]
fn the_table_of_patterns_against_references_matches_the_function_exactly() {
    let cases = cases();
    assert!(!cases.cases.is_empty());
    for case in cases.cases {
        assert_eq!(
            case.policy
                .permits(case.account.as_deref(), &reference(&case.uses)),
            case.allowed,
            "{}",
            case.description
        );
    }
}

#[test]
fn a_pattern_is_one_of_the_five_forms_and_is_kept_in_lower_case_except_its_path() {
    let patterns = cases().patterns;
    for valid in patterns.valid {
        let pattern: ActionPattern = valid
            .text
            .parse()
            .unwrap_or_else(|error| panic!("{}: {error}", valid.text));
        assert_eq!(pattern.to_string(), valid.normalised, "{}", valid.text);
    }
    for invalid in patterns.invalid {
        assert!(
            invalid.parse::<ActionPattern>().is_err(),
            "`{invalid}` was accepted"
        );
    }
}

#[test]
fn a_policy_reads_and_writes_its_mode_and_refuses_what_does_not_belong_to_it() {
    let listed = json!({ "mode": "listed", "patterns": ["actions/*", "acme/tool@v1"] });
    let policy: ActionPolicy = serde_json::from_value(listed.clone()).expect("a policy");
    assert_eq!(serde_json::to_value(&policy).expect("JSON"), listed);
    assert_eq!(ActionPolicy::default(), ActionPolicy::All {});
    for refused in [
        json!({ "mode": "everything" }),
        json!({ "mode": "listed" }),
        json!({ "mode": "listed", "patterns": ["actions"] }),
        json!({ "mode": "listed", "patterns": "actions/*" }),
        json!({ "mode": "all", "patterns": [] }),
        json!({ "mode": "organisation", "account": "acme" }),
        json!({}),
    ] {
        assert!(
            serde_json::from_value::<ActionPolicy>(refused.clone()).is_err(),
            "{refused} was accepted"
        );
    }
}

#[test]
fn a_policy_holds_at_most_the_patterns_and_a_pattern_at_most_the_bytes_the_fixture_gives() {
    let limits = cases().limits;
    assert_eq!(
        (MOST_PATTERNS, LONGEST_PATTERN),
        (limits.most_patterns, limits.longest_pattern)
    );
    let policy = |count: usize| {
        let patterns: Vec<String> = (0..count)
            .map(|number| format!("acme/tool{number}@*"))
            .collect();
        serde_json::from_value::<ActionPolicy>(json!({ "mode": "listed", "patterns": patterns }))
    };
    assert!(policy(limits.most_patterns).is_ok());
    let over = policy(limits.most_patterns + 1).expect_err("a policy over the limit");
    assert!(over.to_string().contains("at most"), "{over}");

    let pattern = |bytes: usize| format!("acme/{}@*", "a".repeat(bytes - "acme/@*".len()));
    assert!(
        pattern(limits.longest_pattern)
            .parse::<ActionPattern>()
            .is_ok()
    );
    assert!(
        pattern(limits.longest_pattern + 1)
            .parse::<ActionPattern>()
            .is_err()
    );
}
