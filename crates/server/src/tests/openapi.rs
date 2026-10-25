use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::api;

const ERROR: &str = "#/components/responses/Error";

const ENVELOPE: &str = "#/components/schemas/ErrorEnvelope";

const PAGE: &str = "#/components/schemas/Page";

const CURSOR: &str = "#/components/parameters/Cursor";

enum Part {
    Request,
    Response(&'static str),
    Within(&'static str),
}

const FIXTURES: &[(&str, &str, &str, Part)] = &[
    (
        "add-member-request.json",
        "post",
        "/api/v1/teams/{team}/members",
        Part::Request,
    ),
    ("agent.json", "get", "/api/v1/agents", Part::Within("Agent")),
    (
        "command-job-cancel.json",
        "post",
        "/api/v1/runs/{run}/commands",
        Part::Request,
    ),
    (
        "command-run-cancel.json",
        "post",
        "/api/v1/runs/{run}/commands",
        Part::Request,
    ),
    (
        "create-organisation-request.json",
        "post",
        "/api/v1/organisations",
        Part::Request,
    ),
    (
        "create-personal-token-request.json",
        "post",
        "/api/v1/tokens",
        Part::Request,
    ),
    (
        "create-pool-request.json",
        "post",
        "/api/v1/pools",
        Part::Request,
    ),
    (
        "create-project-request.json",
        "post",
        "/api/v1/projects",
        Part::Request,
    ),
    (
        "create-role-binding-request.json",
        "post",
        "/api/v1/role-bindings",
        Part::Request,
    ),
    (
        "create-role-request.json",
        "post",
        "/api/v1/roles",
        Part::Request,
    ),
    (
        "create-team-request.json",
        "post",
        "/api/v1/teams",
        Part::Request,
    ),
    (
        "create-user-request.json",
        "post",
        "/api/v1/users",
        Part::Request,
    ),
    (
        "create-workflow-request.json",
        "post",
        "/api/v1/projects/{project}/workflows",
        Part::Request,
    ),
    (
        "dispatch.json",
        "post",
        "/api/v1/workflows/{workflow}/dispatch",
        Part::Request,
    ),
    (
        "error-invalid-input.json",
        "put",
        "/api/v1/workflows/{workflow}/source",
        Part::Response("default"),
    ),
    (
        "error-invalid-state.json",
        "post",
        "/api/v1/runs/{run}/commands",
        Part::Response("default"),
    ),
    (
        "execution.json",
        "get",
        "/api/v1/executions/{execution}",
        Part::Response("200"),
    ),
    (
        "job.json",
        "get",
        "/api/v1/jobs/{job}",
        Part::Response("200"),
    ),
    (
        "label-alias.json",
        "post",
        "/api/v1/label-aliases",
        Part::Response("201"),
    ),
    ("lint-request.json", "post", "/api/v1/lint", Part::Request),
    (
        "lint-response.json",
        "post",
        "/api/v1/lint",
        Part::Response("200"),
    ),
    (
        "log-end.json",
        "get",
        "/api/v1/executions/{execution}/steps/{step}/log",
        Part::Within("LogEnd"),
    ),
    (
        "log-line.json",
        "get",
        "/api/v1/executions/{execution}/steps/{step}/log",
        Part::Within("LogLine"),
    ),
    ("me.json", "get", "/api/v1/me", Part::Response("200")),
    (
        "password-change-request.json",
        "post",
        "/api/v1/auth/password",
        Part::Request,
    ),
    (
        "organisation.json",
        "get",
        "/api/v1/organisations/{organisation}",
        Part::Response("200"),
    ),
    (
        "page-jobs.json",
        "get",
        "/api/v1/runs/{run}/attempts/{attempt}/jobs",
        Part::Response("200"),
    ),
    (
        "personal-token-created.json",
        "post",
        "/api/v1/tokens",
        Part::Response("201"),
    ),
    (
        "personal-token.json",
        "get",
        "/api/v1/tokens",
        Part::Within("PersonalToken"),
    ),
    ("pool.json", "post", "/api/v1/pools", Part::Response("201")),
    (
        "project.json",
        "get",
        "/api/v1/projects/{project}",
        Part::Response("200"),
    ),
    (
        "project-settings.json",
        "get",
        "/api/v1/projects/{project}/settings",
        Part::Response("200"),
    ),
    (
        "queued-job.json",
        "get",
        "/api/v1/queue",
        Part::Within("QueuedJob"),
    ),
    (
        "register-agent-request.json",
        "post",
        "/api/v1/agents/register",
        Part::Request,
    ),
    (
        "register-agent-response.json",
        "post",
        "/api/v1/agents/register",
        Part::Response("201"),
    ),
    (
        "registration-token-created.json",
        "post",
        "/api/v1/registration-tokens",
        Part::Response("201"),
    ),
    (
        "registration-token-request.json",
        "post",
        "/api/v1/registration-tokens",
        Part::Request,
    ),
    (
        "repository-dispatch-request.json",
        "post",
        "/api/v1/projects/{project}/events",
        Part::Request,
    ),
    (
        "revision.json",
        "put",
        "/api/v1/workflows/{workflow}/source",
        Part::Response("201"),
    ),
    (
        "role-binding.json",
        "post",
        "/api/v1/role-bindings",
        Part::Response("201"),
    ),
    ("role.json", "post", "/api/v1/roles", Part::Response("201")),
    (
        "run.json",
        "get",
        "/api/v1/runs/{run}",
        Part::Response("200"),
    ),
    (
        "run-actions.json",
        "get",
        "/api/v1/runs/{run}",
        Part::Response("200"),
    ),
    (
        "run-startup-failure.json",
        "get",
        "/api/v1/runs/{run}",
        Part::Response("200"),
    ),
    (
        "secret.json",
        "post",
        "/api/v1/secrets",
        Part::Response("201"),
    ),
    (
        "set-label-alias-request.json",
        "post",
        "/api/v1/label-aliases",
        Part::Request,
    ),
    (
        "set-secret-request.json",
        "post",
        "/api/v1/secrets",
        Part::Request,
    ),
    (
        "set-variable-request.json",
        "post",
        "/api/v1/variables",
        Part::Request,
    ),
    (
        "sign-in-methods.json",
        "get",
        "/api/v1/auth/methods",
        Part::Response("200"),
    ),
    (
        "sign-in-request.json",
        "post",
        "/api/v1/auth/login",
        Part::Request,
    ),
    (
        "step.json",
        "get",
        "/api/v1/executions/{execution}",
        Part::Within("Step"),
    ),
    ("team.json", "post", "/api/v1/teams", Part::Response("201")),
    (
        "update-agent-request.json",
        "patch",
        "/api/v1/agents/{agent}",
        Part::Request,
    ),
    (
        "update-organisation-request.json",
        "patch",
        "/api/v1/organisations/{organisation}",
        Part::Request,
    ),
    (
        "update-project-request.json",
        "patch",
        "/api/v1/projects/{project}",
        Part::Request,
    ),
    (
        "update-source-request.json",
        "put",
        "/api/v1/workflows/{workflow}/source",
        Part::Request,
    ),
    (
        "update-workflow-request.json",
        "patch",
        "/api/v1/workflows/{workflow}",
        Part::Request,
    ),
    ("user.json", "post", "/api/v1/users", Part::Response("201")),
    (
        "variable.json",
        "post",
        "/api/v1/variables",
        Part::Response("201"),
    ),
    (
        "workflow.json",
        "get",
        "/api/v1/workflows/{workflow}",
        Part::Response("200"),
    ),
];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/fixtures/api")
}

fn resolve<'a>(document: &'a Value, value: &'a Value) -> &'a Value {
    match value.get("$ref").and_then(Value::as_str) {
        Some(reference) => {
            let pointer = reference.strip_prefix('#').unwrap_or(reference);
            document
                .pointer(pointer)
                .unwrap_or_else(|| panic!("`{reference}` names nothing in the document"))
        }
        None => value,
    }
}

fn references(document: &Value, value: &Value, found: &mut BTreeSet<String>) {
    match value {
        Value::Object(object) => {
            for (key, inner) in object {
                match (key.as_str(), inner.as_str()) {
                    ("$ref", Some(reference)) => {
                        if found.insert(reference.to_owned()) {
                            references(document, resolve(document, value), found);
                        }
                    }
                    _ => references(document, inner, found),
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                references(document, item, found);
            }
        }
        _ => {}
    }
}

fn operations(document: &Value) -> Vec<(String, String, &Value)> {
    let paths = document["paths"]
        .as_object()
        .expect("the document has paths");
    paths
        .iter()
        .flat_map(|(path, item)| {
            item.as_object()
                .expect("a path item is an object")
                .iter()
                .map(move |(method, operation)| (method.to_uppercase(), path.clone(), operation))
        })
        .collect()
}

fn holders(value: &Value, pointer: &str, property: &str, found: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if object
                .get("properties")
                .and_then(Value::as_object)
                .is_some_and(|properties| match property {
                    "next" => properties.contains_key("next") && properties.contains_key("items"),
                    _ => properties.contains_key(property),
                })
            {
                found.push(pointer.to_owned());
            }
            for (key, inner) in object {
                let escaped = key.replace('~', "~0").replace('/', "~1");
                holders(inner, &format!("{pointer}/{escaped}"), property, found);
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                holders(item, &format!("{pointer}/{index}"), property, found);
            }
        }
        _ => {}
    }
}

#[test]
fn every_api_fixture_validates_against_the_schema_it_belongs_to() {
    let document = api().document();
    let on_disk: BTreeSet<String> = fs::read_dir(fixtures())
        .expect("contracts/fixtures/api is readable")
        .map(|entry| {
            entry
                .expect("a directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let mapped: BTreeSet<String> = FIXTURES
        .iter()
        .map(|(file, ..)| (*file).to_owned())
        .collect();
    let orphans: Vec<&String> = on_disk.difference(&mapped).collect();
    let missing: Vec<&String> = mapped.difference(&on_disk).collect();
    assert!(
        orphans.is_empty(),
        "fixtures in contracts/fixtures/api/ that belong to no request or response: {orphans:?}"
    );
    assert!(
        missing.is_empty(),
        "fixtures the test maps but cannot find: {missing:?}"
    );

    for (file, method, path, part) in FIXTURES {
        let operation = document["paths"]
            .get(path)
            .and_then(|item| item.get(method))
            .unwrap_or_else(|| panic!("{file}: the document has no {method} {path}"));
        let schema = match part {
            Part::Request => operation
                .pointer("/requestBody/content/application~1json/schema")
                .cloned(),
            Part::Response(status) => operation
                .pointer(&format!("/responses/{status}"))
                .map(|response| resolve(&document, response))
                .and_then(|response| response.pointer("/content/application~1json/schema"))
                .cloned(),
            Part::Within(component) => {
                let reference = format!("#/components/schemas/{component}");
                let mut reached = BTreeSet::new();
                references(&document, operation, &mut reached);
                reached
                    .contains(&reference)
                    .then(|| json!({ "$ref": reference }))
            }
        }
        .unwrap_or_else(|| panic!("{file}: {method} {path} has no such part"));
        let validator = jsonschema::options()
            .should_validate_formats(true)
            .build(&json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "components": { "schemas": document["components"]["schemas"] },
                "allOf": [schema]
            }))
            .unwrap_or_else(|error| panic!("{file}: the schema is not usable: {error}"));
        let text = fs::read_to_string(fixtures().join(file))
            .unwrap_or_else(|error| panic!("{file}: {error}"));
        let fixture: Value =
            serde_json::from_str(&text).unwrap_or_else(|error| panic!("{file}: {error}"));
        let errors: Vec<String> = validator
            .iter_errors(&fixture)
            .map(|error| format!("{error} at {}", error.instance_path()))
            .collect();
        assert!(
            errors.is_empty(),
            "{file} does not validate against its schema in {method} {path}: {errors:#?}"
        );
        let vacuous = if *method == "patch" {
            json!({ "a field no schema has": true })
        } else {
            json!({})
        };
        assert!(
            !validator.is_valid(&vacuous),
            "{file}: the schema in {method} {path} takes {vacuous}"
        );
    }
}

#[test]
fn the_error_envelope_and_the_cursor_page_are_one_component_each() {
    let document = api().document();
    assert_eq!(
        document["components"]["responses"]["Error"]["content"]["application/json"]["schema"],
        json!({ "$ref": ENVELOPE })
    );
    for (method, path, operation) in operations(&document) {
        assert_eq!(
            operation["responses"]["default"],
            json!({ "$ref": ERROR }),
            "{method} {path} does not answer its errors with the error envelope"
        );
        let lists = operation["parameters"]
            .as_array()
            .is_some_and(|parameters| parameters.contains(&json!({ "$ref": CURSOR })));
        let pages = operation.pointer("/responses/200/content/application~1json/schema/allOf/0")
            == Some(&json!({ "$ref": PAGE }));
        assert_eq!(
            lists, pages,
            "{method} {path} takes a cursor without answering with a page, or the reverse"
        );
    }

    let mut envelopes = Vec::new();
    holders(&document, "", "error", &mut envelopes);
    assert_eq!(envelopes, ["/components/schemas/ErrorEnvelope"]);
    let mut pages = Vec::new();
    holders(&document, "", "next", &mut pages);
    assert_eq!(pages, ["/components/schemas/Page"]);
}

#[test]
fn every_operation_declares_the_parameters_of_its_path() {
    let document = api().document();
    for (method, path, operation) in operations(&document) {
        let templated: BTreeSet<&str> = path
            .split('/')
            .filter_map(|segment| segment.strip_prefix('{')?.strip_suffix('}'))
            .collect();
        let declared: BTreeSet<&str> = operation["parameters"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|parameter| parameter["in"] == "path")
            .filter_map(|parameter| parameter["name"].as_str())
            .collect();
        assert_eq!(templated, declared, "{method} {path}");
        assert!(
            operation["responses"]
                .as_object()
                .is_some_and(|responses| responses
                    .keys()
                    .any(|status| status.starts_with('2') || status == "303")),
            "{method} {path} has no successful response"
        );
    }
}
