use bosun_testkit::TestDatabase;
use reqwest::Method;
use serde_json::{Value, json};

use crate::api::{Api, send, text};
use crate::client;

fn fields(body: &Value) -> Vec<String> {
    body["error"]["fields"]
        .as_array()
        .unwrap_or_else(|| panic!("no fields in {body}"))
        .iter()
        .map(|field| text(&field["field"]).to_owned())
        .collect()
}

#[tokio::test]
async fn organisations_and_projects_are_created_and_found_by_id_and_by_slug() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;

    let (status, organisation) = api
        .post(
            "/api/v1/organisations",
            &json!({ "slug": "globalart", "name": "GlobalArt" }),
        )
        .await;
    assert_eq!(status, 201, "{organisation}");
    assert_eq!(organisation["slug"], "globalart");
    assert_eq!(organisation["name"], "GlobalArt");
    let organisation_id = text(&organisation["id"]).to_owned();
    for path in [
        format!("/api/v1/organisations/{organisation_id}"),
        "/api/v1/organisations/globalart".to_owned(),
    ] {
        assert_eq!(api.get(&path).await, (200, organisation.clone()), "{path}");
    }

    let (status, by_slug) = api
        .post(
            "/api/v1/projects",
            &json!({ "organisation": "globalart", "slug": "ops", "name": "Operations" }),
        )
        .await;
    assert_eq!(status, 201, "{by_slug}");
    assert_eq!(by_slug["organisation_id"], organisation["id"]);
    assert_eq!(by_slug["slug"], "ops");
    let (status, by_id) = api
        .post(
            "/api/v1/projects",
            &json!({ "organisation": organisation_id, "slug": "workflows", "name": "Workflows" }),
        )
        .await;
    assert_eq!(status, 201, "{by_id}");
    assert_eq!(by_id["organisation_id"], organisation["id"]);

    for (path, project) in [
        (
            format!("/api/v1/projects/{}", text(&by_slug["id"])),
            &by_slug,
        ),
        ("/api/v1/projects/globalart/ops".to_owned(), &by_slug),
        (format!("/api/v1/projects/{}", text(&by_id["id"])), &by_id),
        ("/api/v1/projects/globalart/workflows".to_owned(), &by_id),
    ] {
        assert_eq!(api.get(&path).await, (200, project.clone()), "{path}");
    }

    for (path, message) in [
        (
            "/api/v1/organisations/nobody",
            "there is no organisation `nobody`",
        ),
        (
            "/api/v1/organisations/0192f3a4-5b6c-7d8e-9f01-000000000001",
            "there is no organisation `0192f3a4-5b6c-7d8e-9f01-000000000001`",
        ),
        (
            "/api/v1/projects/globalart/keel",
            "there is no project `globalart/keel`",
        ),
        (
            "/api/v1/projects/nobody/ops",
            "there is no project `nobody/ops`",
        ),
        ("/api/v1/projects/ops", "there is no project `ops`"),
        (
            "/api/v1/projects/0192f3a4-5b6c-7d8e-9f01-000000000002",
            "there is no project `0192f3a4-5b6c-7d8e-9f01-000000000002`",
        ),
    ] {
        let (status, body) = api.get(path).await;
        assert_eq!(status, 404, "{path}: {body}");
        assert_eq!(body["error"]["code"], "not_found", "{path}");
        assert_eq!(body["error"]["message"], message, "{path}");
    }
}

#[tokio::test]
async fn an_organisation_or_a_project_that_does_not_fit_is_refused_with_its_fields() {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    api.project().await;

    for (body, expected) in [
        (
            json!({ "slug": "Global Art", "name": "GlobalArt" }),
            vec!["slug"],
        ),
        (
            json!({ "slug": "-dash", "name": " " }),
            vec!["slug", "name"],
        ),
        (
            json!({ "slug": "0192f3a4-5b6c-7d8e-9f01-000000000001", "name": "Looks like an id" }),
            vec!["slug"],
        ),
        (
            json!({ "slug": "a".repeat(65), "name": "Long" }),
            vec!["slug"],
        ),
        (
            json!({ "slug": "fine", "name": "x".repeat(256) }),
            vec!["name"],
        ),
    ] {
        let (status, refused) = api.post("/api/v1/organisations", &body).await;
        assert_eq!(status, 422, "{body}: {refused}");
        assert_eq!(refused["error"]["code"], "invalid_input");
        assert_eq!(refused["error"]["message"], "the organisation is not valid");
        assert_eq!(fields(&refused), expected, "{body}");
    }

    let (status, refused) = api
        .post(
            "/api/v1/projects",
            &json!({ "organisation": "nobody", "slug": "ops", "name": "Operations" }),
        )
        .await;
    assert_eq!(status, 422, "{refused}");
    assert_eq!(refused["error"]["message"], "the project is not valid");
    assert_eq!(
        refused["error"]["fields"],
        json!([{ "field": "organisation", "message": "there is no organisation `nobody`" }])
    );
    let (status, refused) = api
        .post(
            "/api/v1/projects",
            &json!({ "organisation": "globalart", "slug": "Ops", "name": "" }),
        )
        .await;
    assert_eq!(status, 422, "{refused}");
    assert_eq!(fields(&refused), vec!["slug", "name"]);

    let (status, taken) = api
        .post(
            "/api/v1/organisations",
            &json!({ "slug": "globalart", "name": "Again" }),
        )
        .await;
    assert_eq!(status, 409, "{taken}");
    assert_eq!(taken["error"]["code"], "conflict");
    assert_eq!(
        taken["error"]["message"],
        "an organisation with the slug `globalart` already exists"
    );
    let (status, taken) = api
        .post(
            "/api/v1/projects",
            &json!({ "organisation": "globalart", "slug": "ops", "name": "Again" }),
        )
        .await;
    assert_eq!(status, 409, "{taken}");
    assert_eq!(
        taken["error"]["message"],
        "the organisation already has a project with the slug `ops`"
    );

    for body in [
        "{\"slug\": \"x\"}",
        "not json",
        "{\"slug\": 1, \"name\": \"x\"}",
    ] {
        let (status, refused) = send(
            api.request(Method::POST, "/api/v1/organisations")
                .header("content-type", "application/json")
                .body(body),
        )
        .await;
        assert_eq!(status, 422, "{body}: {refused}");
        assert_eq!(refused["error"]["code"], "invalid_input");
        let message = text(&refused["error"]["message"]);
        assert!(
            message.starts_with("the request body does not fit: "),
            "{message}"
        );
    }
}

#[tokio::test]
async fn the_routes_of_projects_and_workflows_need_a_token_and_answer_unserved_paths_with_not_found()
 {
    let database = TestDatabase::migrated().await;
    let api = Api::start(&database).await;
    let project = api.project().await;
    let project_id = text(&project["id"]);

    for (method, path) in [
        (Method::POST, "/api/v1/organisations".to_owned()),
        (Method::GET, "/api/v1/organisations/globalart".to_owned()),
        (Method::PATCH, "/api/v1/organisations/globalart".to_owned()),
        (Method::POST, "/api/v1/projects".to_owned()),
        (Method::GET, "/api/v1/projects/globalart/ops".to_owned()),
        (Method::PATCH, "/api/v1/projects/globalart/ops".to_owned()),
        (
            Method::GET,
            format!("/api/v1/projects/{project_id}/workflows"),
        ),
        (
            Method::POST,
            format!("/api/v1/projects/{project_id}/workflows"),
        ),
        (
            Method::GET,
            "/api/v1/workflows/0192f3a4-5b6c-7d8e-9f01-000000000005".to_owned(),
        ),
        (
            Method::PUT,
            "/api/v1/workflows/0192f3a4-5b6c-7d8e-9f01-000000000005/source".to_owned(),
        ),
        (
            Method::GET,
            "/api/v1/workflows/0192f3a4-5b6c-7d8e-9f01-000000000005/revisions".to_owned(),
        ),
        (
            Method::POST,
            "/api/v1/workflows/0192f3a4-5b6c-7d8e-9f01-000000000005/dispatch".to_owned(),
        ),
    ] {
        let (status, body) = send(
            client()
                .request(method.clone(), format!("http://{}{path}", api.address))
                .json(&json!({})),
        )
        .await;
        assert_eq!(status, 401, "{method} {path}: {body}");
        assert_eq!(body["error"]["code"], "unauthenticated");
    }

    for (method, path) in [
        (Method::POST, format!("/api/v1/projects/{project_id}")),
        (Method::POST, "/api/v1/projects/globalart/ops".to_owned()),
        (Method::DELETE, format!("/api/v1/projects/{project_id}")),
        (
            Method::GET,
            format!("/api/v1/projects/{project_id}/workflows/ci"),
        ),
        (
            Method::GET,
            "/api/v1/projects/globalart/ops/runs".to_owned(),
        ),
        (Method::GET, "/api/v1/projects/globalart/ops/".to_owned()),
        (
            Method::PATCH,
            format!("/api/v1/projects/{project_id}/workflows"),
        ),
        (Method::PUT, "/api/v1/organisations/globalart".to_owned()),
    ] {
        let (status, body) = send(api.request(method.clone(), &path).json(&json!({}))).await;
        assert_eq!(status, 404, "{method} {path}: {body}");
        assert_eq!(body["error"]["code"], "not_found");
        assert_eq!(
            body["error"]["message"],
            format!("there is no {method} {path}"),
            "{method} {path}"
        );
    }
}
