use bosun_types::diagnostic::{Diagnostic, PathSegment};
use bosun_types::ir::{ActionMetadata, NodeRuntime, Runs};
use serde_json::{Value, json};

use crate::documents::{quoted, table, workflows_document};
use crate::fixtures::{cases, fixtures, json, pretty};
use crate::{path, read};

const ACTION: &str = "action.yml";

fn lowered(source: &str) -> ActionMetadata {
    match bosun_workflow::action(ACTION, source) {
        Ok(parsed) => {
            assert!(
                parsed.warnings.is_empty(),
                "{source}\nwarned {:#?}",
                parsed.warnings
            );
            parsed.metadata
        }
        Err(diagnostics) => panic!("{source}\nproduced {diagnostics:#?}"),
    }
}

fn refused(source: &str) -> Vec<Diagnostic> {
    match bosun_workflow::action(ACTION, source) {
        Ok(parsed) => panic!("{source}\nlowered to {:#?}", parsed.metadata),
        Err(diagnostics) => diagnostics,
    }
}

fn case(name: &str) -> String {
    read(&fixtures().join("actions").join(format!("{name}.yml")))
}

#[test]
fn every_action_fixture_lowers_to_its_metadata_or_its_diagnostics() {
    let directory = fixtures().join("actions");
    let sources = cases(&directory, "yml");
    assert!(sources.len() > 10, "too few action fixtures: {sources:?}");
    assert_eq!(cases(&directory, "json"), sources);
    for name in sources {
        let produced = match bosun_workflow::action(ACTION, &case(&name)) {
            Ok(parsed) => json!({ "metadata": parsed.metadata, "diagnostics": parsed.warnings }),
            Err(diagnostics) => json!({ "metadata": null, "diagnostics": diagnostics }),
        };
        let committed = json(&directory.join(format!("{name}.json")));
        assert_eq!(
            produced,
            committed,
            "{name}: the result differs from actions/{name}.json; it is now\n{}",
            pretty(&produced)
        );
    }
}

#[test]
fn every_node_runtime_lowers_from_a_public_action() {
    let runtimes = [
        ("actions-rs-toolchain-v1", NodeRuntime::Node12),
        ("actions-setup-node-v3", NodeRuntime::Node16),
        ("actions-checkout-v4", NodeRuntime::Node20),
        ("swatinem-rust-cache-v2", NodeRuntime::Node24),
    ];
    for (name, runtime) in runtimes {
        let Runs::Node(runs) = lowered(&case(name)).runs;
        assert_eq!(runs.using, runtime, "{name}");
    }
}

#[test]
fn a_pre_if_a_runs_without_main_and_a_composite_action_each_produce_their_diagnostic() {
    let cases = [
        (
            "pre-if-steps-context",
            "unavailable_context",
            vec!["runs", "pre-if"],
        ),
        ("runs-without-main", "missing_key", vec!["runs"]),
        ("using-composite", "not_implemented", vec!["runs", "using"]),
    ];
    for (name, code, at) in cases {
        let diagnostics = refused(&case(name));
        assert_eq!(diagnostics.len(), 1, "{name}: {diagnostics:#?}");
        assert_eq!(diagnostics[0].code, code, "{name}");
        assert_eq!(diagnostics[0].path, path(&at), "{name}");
    }
    assert!(
        refused(&case("using-composite"))[0]
            .message
            .ends_with("it arrives in M2")
    );
}

#[test]
fn unknown_keys_are_ignored_where_the_runner_ignores_them() {
    let runs = "runs:\n  using: node20\n  main: index.js\n";
    for (source, at) in [
        (format!("colour: red\n{runs}"), vec!["colour"]),
        (
            format!("inputs:\n  token:\n    type: string\n{runs}"),
            vec!["inputs", "token", "type"],
        ),
        (
            format!("branding:\n  colour: red\n{runs}"),
            vec!["branding", "colour"],
        ),
    ] {
        let parsed = bosun_workflow::action(ACTION, &source)
            .unwrap_or_else(|diagnostics| panic!("{source}\nproduced {diagnostics:#?}"));
        assert_eq!(parsed.warnings.len(), 1, "{source}");
        assert_eq!(parsed.warnings[0].code, "ignored_key", "{source}");
        assert_eq!(parsed.warnings[0].path, path(&at), "{source}");
    }
    let suggested = bosun_workflow::action(ACTION, &format!("descripton: x\n{runs}"))
        .expect("an ignored key is no error");
    assert_eq!(
        suggested.warnings[0].message,
        "unknown key `descripton` in the action's metadata is ignored; did you mean `description`?"
    );
    for (source, at) in [
        (
            format!("outputs:\n  digest:\n    describe: x\n{runs}"),
            vec!["outputs", "digest", "describe"],
        ),
        (format!("{runs}  args: x\n"), vec!["runs", "args"]),
    ] {
        let diagnostics = refused(&source);
        assert_eq!(diagnostics.len(), 1, "{source}: {diagnostics:#?}");
        assert_eq!(diagnostics[0].code, "unknown_key", "{source}");
        assert_eq!(diagnostics[0].path, path(&at), "{source}");
    }
}

#[test]
fn defaults_are_templates_and_other_texts_are_never_evaluated() {
    let metadata = lowered(
        "name: Deploy ${{ inputs.target }}\ndescription: Uses ${{ secrets.TOKEN }}\n\
         inputs:\n  depth:\n    default: 1\n  lfs:\n    default: false\n  filter:\n    default: null\n\
         runs:\n  using: node20\n  main: index.js\n",
    );
    assert_eq!(
        metadata.name.as_deref(),
        Some("Deploy ${{ inputs.target }}")
    );
    assert_eq!(
        metadata.description.as_deref(),
        Some("Uses ${{ secrets.TOKEN }}")
    );
    let defaults: Vec<Option<String>> = metadata
        .inputs
        .iter()
        .map(|input| {
            input
                .default
                .as_ref()
                .and_then(|default| default.as_literal())
        })
        .collect();
    assert_eq!(
        defaults,
        [Some("1".to_string()), Some("false".to_string()), None]
    );
}

#[test]
fn runs_takes_non_empty_literal_values() {
    let head = "runs:\n  using: node20\n";
    for (source, code, at) in [
        (
            format!("{head}  main: ''\n"),
            "invalid_value",
            vec!["runs", "main"],
        ),
        (
            format!("{head}  main: index.js\n  post-if: ' '\n"),
            "invalid_value",
            vec!["runs", "post-if"],
        ),
        (
            format!("{head}  main: index.js\n  pre: [a, b]\n"),
            "invalid_type",
            vec!["runs", "pre"],
        ),
        (
            "runs:\n  main: index.js\n".to_string(),
            "missing_key",
            vec!["runs"],
        ),
        ("name: x\n".to_string(), "missing_key", vec![]),
    ] {
        let diagnostics = refused(&source);
        assert_eq!(diagnostics.len(), 1, "{source}: {diagnostics:#?}");
        assert_eq!(diagnostics[0].code, code, "{source}");
        assert_eq!(diagnostics[0].path, path(&at), "{source}");
    }
}

fn action_with(place: &str, key: &str) -> String {
    let runs = "runs:\n  using: node20\n  main: index.js\n";
    match place {
        "root" => format!("{key}: x\n{runs}"),
        "branding" => format!("branding:\n  {key}: x\n{runs}"),
        "inputs.<input_id>" => format!("inputs:\n  token:\n    {key}: x\n{runs}"),
        "outputs.<output_id>" => format!("outputs:\n  digest:\n    {key}: x\n{runs}"),
        "runs" => format!("{runs}  {key}: x\n"),
        other => panic!("no action template for keys of `{other}`"),
    }
}

fn about(diagnostic: &Diagnostic, key: &str) -> bool {
    ["not_implemented", "unknown_key", "ignored_key"].contains(&diagnostic.code.as_str())
        && diagnostic.path.last() == Some(&PathSegment::Key(key.to_string()))
        && diagnostic.message.contains(&format!("`{key}` in "))
}

#[test]
fn keys_of_action_metadata_arrive_with_their_milestone() {
    let rows = table(&workflows_document(), "| In the metadata | Keys | From |");
    let mut checked = 0;
    for row in rows {
        let place = row[0].trim_matches('`');
        let milestone = crate::documents::milestone(&row[2]);
        for key in quoted(&row[1]) {
            checked += 1;
            let source = action_with(place, &key);
            let diagnostics = match bosun_workflow::action(ACTION, &source) {
                Ok(parsed) => parsed.warnings,
                Err(diagnostics) => diagnostics,
            };
            let implemented = milestone <= bosun_workflow::MILESTONE
                && !bosun_workflow::PENDING.contains(&(place, key.as_str()));
            if implemented {
                assert!(
                    !diagnostics.iter().any(|diagnostic| about(diagnostic, &key)),
                    "`{key}` of {place} arrives in {} and should be known: {diagnostics:#?}",
                    milestone.name()
                );
            } else {
                let expected = format!("it arrives in {}", milestone.name());
                assert!(
                    diagnostics.iter().any(|diagnostic| about(diagnostic, &key)
                        && diagnostic.code == "not_implemented"
                        && diagnostic.message.ends_with(&expected)),
                    "`{key}` of {place} should be refused as arriving in {}: {diagnostics:#?}",
                    milestone.name()
                );
            }
        }
    }
    assert!(
        checked > 15,
        "the table of action keys lists only {checked} keys"
    );
}

#[test]
fn the_metadata_round_trips_through_json() {
    let metadata = lowered(&case("hooks"));
    let text = serde_json::to_string(&metadata).expect("serialisable");
    let back: ActionMetadata = serde_json::from_str(&text).expect("deserialisable");
    assert_eq!(back, metadata);
    let mut value: Value = serde_json::from_str(&text).expect("JSON");
    value["runs"]["node"]["steps"] = json!([]);
    assert!(serde_json::from_value::<ActionMetadata>(value).is_err());
}
