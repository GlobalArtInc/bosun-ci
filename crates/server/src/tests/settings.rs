use axum::http::{Method, StatusCode};
use bosun_db::roles::Subject;
use serde_json::{Value, json};

use super::access::{Answer, Credentials, Site, document, forbidden, owner_credentials};

const DEFAULTS: &str = "contracts/fixtures/api/project-settings.json";

fn defaults() -> Value {
    serde_json::from_str(&document(DEFAULTS)).expect("the fixture is JSON")
}

fn etag(answer: &Answer) -> String {
    answer
        .headers
        .get("etag")
        .unwrap_or_else(|| panic!("no ETag in {answer:?}"))
        .to_str()
        .expect("an ETag of visible characters")
        .to_owned()
}

fn refused(answer: &Answer, status: StatusCode, code: &str) {
    assert_eq!(answer.status, status, "{answer:?}");
    assert_eq!(answer.body["error"]["code"], code, "{answer:?}");
}

fn fields(answer: &Answer) -> Vec<String> {
    refused(answer, StatusCode::UNPROCESSABLE_ENTITY, "invalid_input");
    answer.body["error"]["fields"]
        .as_array()
        .expect("the fields at fault")
        .iter()
        .map(|field| {
            field["field"]
                .as_str()
                .expect("the name of a field")
                .to_owned()
        })
        .collect()
}

async fn change(
    site: &Site,
    path: &str,
    credentials: &Credentials,
    tag: Option<&str>,
    body: Value,
) -> Answer {
    let headers: Vec<(&str, String)> = tag
        .map(|tag| ("if-match", tag.to_owned()))
        .into_iter()
        .collect();
    site.send_with(Method::PATCH, path, credentials, &headers, Some(body))
        .await
}

#[tokio::test]
async fn a_new_projects_settings_are_the_committed_defaults_at_their_first_version() {
    let site = Site::new().await;
    let owner = owner_credentials(&site).await;
    let created = site
        .send(
            Method::POST,
            "/api/v1/projects",
            &owner,
            Some(json!({ "organisation": "globalart", "slug": "settings", "name": "Settings" })),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{created:?}");
    let id = created.body["id"].as_str().expect("an id");

    for path in [
        "/api/v1/projects/globalart/settings/settings".to_owned(),
        format!("/api/v1/projects/{id}/settings"),
    ] {
        let read = site.get(&path, &owner).await;
        assert_eq!(read.status, StatusCode::OK, "{path}: {read:?}");
        assert_eq!(read.body, defaults(), "{path}");
        assert_eq!(etag(&read), "\"1\"", "{path}");
    }
    let project = site
        .get("/api/v1/projects/globalart/settings", &owner)
        .await;
    assert_eq!(project.body, created.body);
    let posted = site
        .send(
            Method::POST,
            &format!("/api/v1/projects/{id}/settings"),
            &owner,
            Some(defaults()),
        )
        .await;
    refused(&posted, StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn a_change_needs_the_etag_of_the_current_version() {
    let site = Site::new().await;
    let owner = owner_credentials(&site).await;
    let path = "/api/v1/projects/globalart/keel/settings";
    let first = site.get(path, &owner).await;
    let first_tag = etag(&first);
    let approval = json!({ "fork_policy": "approval" });

    for tag in [None, Some("*"), Some("W/\"1\""), Some("\"2\""), Some("1")] {
        let answer = change(&site, path, &owner, tag, approval.clone()).await;
        refused(
            &answer,
            StatusCode::PRECONDITION_FAILED,
            "precondition_failed",
        );
    }
    let unchecked = change(&site, path, &owner, None, json!({ "retention": 7 })).await;
    refused(
        &unchecked,
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
    );
    assert_eq!(site.get(path, &owner).await.body, defaults());

    let changed = change(&site, path, &owner, Some(&first_tag), approval.clone()).await;
    assert_eq!(changed.status, StatusCode::OK, "{changed:?}");
    let mut expected = defaults();
    expected["fork_policy"] = json!("approval");
    assert_eq!(changed.body, expected);
    let second_tag = etag(&changed);
    assert_ne!(second_tag, first_tag);
    let read = site.get(path, &owner).await;
    assert_eq!(
        (read.body.clone(), etag(&read)),
        (expected.clone(), second_tag.clone())
    );

    let stale = change(
        &site,
        path,
        &owner,
        Some(&first_tag),
        json!({ "fork_policy": "automatic" }),
    )
    .await;
    refused(
        &stale,
        StatusCode::PRECONDITION_FAILED,
        "precondition_failed",
    );
    assert_eq!(
        stale.body["error"]["message"],
        format!(
            "`If-Match` is `{first_tag}`, and the current version of the settings is \
             `{second_tag}`: read the current version and repeat the change"
        )
    );
    assert_eq!(site.get(path, &owner).await.body, expected);

    let same = change(&site, path, &owner, Some(&second_tag), approval).await;
    assert_eq!(same.status, StatusCode::OK, "{same:?}");
    assert_eq!(
        etag(&same),
        second_tag,
        "a change to the same values made a version"
    );

    let listed = format!("\"9\", {second_tag}");
    let everything = json!({
        "github_workflows": true,
        "fork_policy": "automatic",
        "token_permissions": { "contents": "write", "pull_requests": "read" },
        "report": "statuses"
    });
    let whole = change(&site, path, &owner, Some(&listed), everything.clone()).await;
    assert_eq!(whole.status, StatusCode::OK, "{whole:?}");
    assert_eq!(whole.body, everything);
    assert_ne!(etag(&whole), second_tag);
}

#[tokio::test]
async fn an_unknown_setting_and_a_value_outside_its_set_are_invalid_input_naming_the_field() {
    let site = Site::new().await;
    let owner = owner_credentials(&site).await;
    let path = "/api/v1/projects/globalart/keel/settings";
    let tag = etag(&site.get(path, &owner).await);

    for (body, named) in [
        (json!({ "fork_policy": "sometimes" }), vec!["fork_policy"]),
        (json!({ "retention": "90d" }), vec!["retention"]),
        (
            json!({ "retention": "90d", "fork_policy": "sometimes", "report": "checks" }),
            vec!["fork_policy", "retention"],
        ),
        (
            json!({ "github_workflows": "yes" }),
            vec!["github_workflows"],
        ),
        (json!({ "report": "comments" }), vec!["report"]),
        (
            json!({ "token_permissions": { "administration": "write" } }),
            vec!["token_permissions"],
        ),
        (
            json!({ "token_permissions": { "contents": "none" } }),
            vec!["token_permissions"],
        ),
        (json!({ "fork_policy": null }), vec!["fork_policy"]),
    ] {
        let answer = change(&site, path, &owner, Some(&tag), body.clone()).await;
        assert_eq!(fields(&answer), named, "{body}");
        assert_eq!(
            answer.body["error"]["message"], "the settings are not valid",
            "{body}"
        );
    }
    let unknown = change(&site, path, &owner, Some(&tag), json!({ "retention": 1 })).await;
    assert_eq!(
        unknown.body["error"]["fields"][0]["message"],
        "there is no setting `retention`; the settings are `fork_policy`, `github_workflows`, \
         `report`, `token_permissions`"
    );
    let outside = change(
        &site,
        path,
        &owner,
        Some(&tag),
        json!({ "fork_policy": "sometimes" }),
    )
    .await;
    assert_eq!(
        outside.body["error"]["fields"][0]["message"],
        "`fork_policy` cannot be \"sometimes\": unknown variant `sometimes`, expected one of \
         `disabled`, `approval`, `automatic`"
    );

    for body in [json!({}), json!([]), json!("approval")] {
        let answer = change(&site, path, &owner, Some(&tag), body.clone()).await;
        assert_eq!(fields(&answer), Vec::<String>::new(), "{body}");
    }

    let read = site.get(path, &owner).await;
    assert_eq!((read.body.clone(), etag(&read)), (defaults(), tag));
}

#[tokio::test]
async fn a_viewer_reads_the_settings_a_maintainer_cannot_change_them_and_an_admin_can() {
    let site = Site::new().await;
    let project = site.project().await;
    let path = format!("/api/v1/projects/{}/settings", project.id);
    let mut credentials = Vec::new();
    for (login, role) in [("vera", "viewer"), ("mia", "maintainer"), ("ada", "admin")] {
        let (user, token) = site.user(login).await;
        site.bind(
            role,
            Subject::User(user),
            project.organisation_id,
            Some(project.id),
        )
        .await;
        credentials.push(token);
    }
    let [viewer, maintainer, admin] = credentials.as_slice() else {
        panic!("three callers");
    };

    for reader in [viewer, maintainer, admin] {
        let read = site.get(&path, reader).await;
        assert_eq!(read.status, StatusCode::OK, "{read:?}");
        assert_eq!(read.body, defaults());
    }
    let tag = etag(&site.get(&path, viewer).await);
    let automatic = json!({ "fork_policy": "automatic" });
    forbidden(
        &change(&site, &path, viewer, Some(&tag), automatic.clone()).await,
        "a viewer's change",
    );
    let maintained = change(&site, &path, maintainer, Some(&tag), automatic.clone()).await;
    forbidden(&maintained, "a maintainer's change");
    assert_eq!(
        maintained.body["error"]["message"],
        "`mia` does not hold `project.manage` on project `keel`"
    );
    assert_eq!(site.get(&path, viewer).await.body, defaults());

    let changed = change(&site, &path, admin, Some(&tag), automatic).await;
    assert_eq!(changed.status, StatusCode::OK, "{changed:?}");
    assert_eq!(changed.body["fork_policy"], "automatic");
    assert_eq!(site.get(&path, viewer).await.body, changed.body);

    let (_, stranger) = site.user("sam").await;
    let hidden = site.get(&path, &stranger).await;
    refused_as_missing(&hidden, &project.id.to_string());
    let hidden = change(&site, &path, &stranger, Some(&tag), json!({})).await;
    refused_as_missing(&hidden, &project.id.to_string());
}

fn refused_as_missing(answer: &Answer, project: &str) {
    refused(answer, StatusCode::NOT_FOUND, "not_found");
    assert_eq!(
        answer.body["error"]["message"],
        format!("there is no project `{project}`")
    );
}
