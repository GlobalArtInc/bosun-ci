use std::collections::BTreeSet;

use axum::http::{Method, StatusCode};
use bosun_db::entity::{organisation, project};
use bosun_db::roles::Subject;
use bosun_types::api::Project;
use bosun_types::id::{OrganisationId, UserId};
use chrono::{TimeDelta, Utc};
use sea_orm::prelude::Uuid;
use sea_orm::{ActiveModelTrait, ActiveValue, EntityTrait};
use serde_json::{Value, json};

use super::access::{
    Answer, Credentials, NEW_PASSWORD, PASSWORD, Site, absent, document, forbidden, ids,
    owner_credentials,
};

fn fixture(name: &str) -> Value {
    serde_json::from_str(&document(&format!("contracts/fixtures/api/{name}"))).expect("a fixture")
}

fn ahead(days: i64) -> Value {
    json!(Utc::now() + TimeDelta::days(days))
}

fn status(answer: &Answer, expected: StatusCode, context: &str) {
    assert_eq!(answer.status, expected, "{context}: {answer:?}");
}

fn invalid(answer: &Answer, fields: &[&str], context: &str) {
    status(answer, StatusCode::UNPROCESSABLE_ENTITY, context);
    assert_eq!(answer.body["error"]["code"], "invalid_input", "{context}");
    let named: Vec<&str> = answer.body["error"]["fields"]
        .as_array()
        .expect("the fields at fault")
        .iter()
        .map(|field| field["field"].as_str().expect("a field"))
        .collect();
    assert_eq!(named, fields, "{context}: {answer:?}");
}

fn message<'a>(answer: &'a Answer, field: &str) -> &'a str {
    answer.body["error"]["fields"]
        .as_array()
        .expect("the fields at fault")
        .iter()
        .find(|entry| entry["field"] == field)
        .and_then(|entry| entry["message"].as_str())
        .unwrap_or_else(|| panic!("no field `{field}` in {answer:?}"))
}

fn bearer(created: &Answer) -> Credentials {
    status(created, StatusCode::CREATED, "a token");
    Credentials::Bearer(
        created.body["token"]
            .as_str()
            .expect("the token")
            .to_owned(),
    )
}

fn id(answer: &Answer) -> String {
    answer.body["id"].as_str().expect("an id").to_owned()
}

async fn post(site: &Site, path: &str, credentials: &Credentials, body: Value) -> Answer {
    site.send(Method::POST, path, credentials, Some(body)).await
}

async fn delete(site: &Site, path: &str, credentials: &Credentials) -> Answer {
    site.send(Method::DELETE, path, credentials, None).await
}

async fn token(site: &Site, credentials: &Credentials, request: Value) -> Answer {
    post(site, "/api/v1/tokens", credentials, request).await
}

fn viewing(projects: &[&str], days: i64) -> Value {
    json!({
        "name": "ci",
        "permissions": ["project.view"],
        "organisations": [],
        "projects": projects,
        "expires_at": ahead(days)
    })
}

async fn organisation(site: &Site, slug: &str) -> OrganisationId {
    let created = organisation::ActiveModel {
        id: ActiveValue::Set(Uuid::now_v7()),
        slug: ActiveValue::Set(slug.to_owned()),
        name: ActiveValue::Set(slug.to_owned()),
        ..Default::default()
    }
    .insert(site.db())
    .await
    .expect("an organisation");
    OrganisationId::try_from(created.id).expect("an id")
}

async fn member(site: &Site, login: &str, role: &str, project: &Project) -> (UserId, Credentials) {
    let (user, credentials) = site.user(login).await;
    site.bind(role, Subject::User(user), project.organisation_id, None)
        .await;
    (user, credentials)
}

#[tokio::test]
async fn a_token_restricted_to_viewing_one_project_reads_its_runs_and_nothing_else() {
    let site = Site::new().await;
    let seen = site.project().await;
    let hidden = site.second_project("hidden").await;
    let deploy = site.workflow(seen.id, "deploy").await;
    let hidden_deploy = site.workflow(hidden.id, "deploy").await;
    let (dana, _) = member(&site, "dana", "developer", &seen).await;
    let session = site.signed(dana, "dana").await;
    let owner = owner_credentials(&site).await;
    let seen_run = site.dispatch(&deploy, &owner).await;
    status(&seen_run, StatusCode::CREATED, "a run of the seen project");
    let hidden_run = site.dispatch(&hidden_deploy, &owner).await;
    status(
        &hidden_run,
        StatusCode::CREATED,
        "a run of the hidden project",
    );

    let created = token(&site, &session, viewing(&["globalart/keel"], 30)).await;
    let restricted = bearer(&created);
    let shape = fixture("personal-token-created.json");
    assert_eq!(
        created
            .body
            .as_object()
            .map(|body| body.keys().collect::<Vec<_>>()),
        shape
            .as_object()
            .map(|body| body.keys().collect::<Vec<_>>())
    );
    assert_eq!(created.body["restricted"], true);
    assert_eq!(created.body["permissions"], json!(["project.view"]));
    assert_eq!(created.body["organisations"], json!([]));
    assert_eq!(
        created.body["projects"],
        json!([{ "id": seen.id, "organisation_id": seen.organisation_id, "path": "globalart/keel" }])
    );

    let me = site.get("/api/v1/me", &restricted).await;
    assert_eq!(
        me.body["permissions"],
        json!({
            "installation": [],
            "organisations": {},
            "projects": { seen.id.to_string(): ["project.view"] }
        })
    );
    assert_eq!(
        ids(&site.get("/api/v1/runs", &restricted).await),
        BTreeSet::from([id(&seen_run)])
    );
    assert_eq!(
        ids(&site.get("/api/v1/projects", &restricted).await),
        BTreeSet::from([seen.id.to_string()])
    );
    status(
        &site
            .get(&format!("/api/v1/runs/{}", id(&seen_run)), &restricted)
            .await,
        StatusCode::OK,
        "a run of the seen project",
    );
    absent(
        &site
            .get(&format!("/api/v1/runs/{}", id(&hidden_run)), &restricted)
            .await,
        &format!("there is no run `{}`", id(&hidden_run)),
    );
    absent(
        &site
            .get("/api/v1/projects/globalart/hidden", &restricted)
            .await,
        "there is no project `globalart/hidden`",
    );
    absent(
        &site
            .get("/api/v1/organisations/globalart", &restricted)
            .await,
        "there is no organisation `globalart`",
    );
    let refused = site.dispatch(&deploy, &restricted).await;
    forbidden(&refused, "a dispatch with a token that views");
    assert_eq!(
        refused.body["error"]["message"],
        "`dana` does not hold `run.dispatch` on project `keel`"
    );
    status(
        &site.dispatch(&deploy, &session).await,
        StatusCode::CREATED,
        "a dispatch of its creator",
    );
}

#[tokio::test]
async fn a_token_carries_only_what_its_creator_holds_for_at_most_a_year() {
    let site = Site::new().await;
    let project = site.project().await;
    site.second_project("hidden").await;
    let elsewhere = organisation(&site, "other").await;
    bosun_db::projects::create(site.db(), elsewhere, "ops", "Operations")
        .await
        .expect("a project of another organisation");
    let (vera, _) = member(&site, "vera", "viewer", &project).await;
    let session = site.signed(vera, "vera").await;

    let lacking = token(
        &site,
        &session,
        json!({
            "name": "ci",
            "permissions": ["project.view", "run.dispatch"],
            "organisations": [],
            "projects": [],
            "expires_at": ahead(30)
        }),
    )
    .await;
    invalid(&lacking, &["permissions.1"], "a key its creator lacks");
    assert_eq!(
        message(&lacking, "permissions.1"),
        "`vera` does not hold `run.dispatch` anywhere, and a token carries only what its creator \
         holds"
    );
    let beyond = token(&site, &session, viewing(&[], 366)).await;
    invalid(&beyond, &["expires_at"], "an expiry beyond a year");
    assert_eq!(
        message(&beyond, "expires_at"),
        "lies more than a year ahead, and a personal token lives a year at most"
    );
    invalid(
        &token(&site, &session, viewing(&[], -1)).await,
        &["expires_at"],
        "an expiry in the past",
    );
    invalid(
        &token(
            &site,
            &session,
            json!({
                "name": "",
                "permissions": ["future.thing"],
                "organisations": ["nowhere"],
                "projects": ["other/ops"],
                "expires_at": ahead(30)
            }),
        )
        .await,
        &["name", "permissions.0", "organisations.0", "projects.0"],
        "an empty name, an unknown key and places the creator holds nothing on",
    );
    invalid(
        &token(
            &site,
            &session,
            json!({
                "name": "ci",
                "permissions": [],
                "organisations": [],
                "projects": [],
                "expires_at": ahead(30)
            }),
        )
        .await,
        &["permissions"],
        "a token without a permission",
    );
    let outside = token(
        &site,
        &session,
        json!({
            "name": "ci",
            "permissions": ["run.cancel"],
            "organisations": ["globalart"],
            "projects": [],
            "expires_at": ahead(30)
        }),
    )
    .await;
    invalid(
        &outside,
        &["permissions.0"],
        "a key held nowhere it reaches",
    );
    assert_eq!(
        message(&outside, "permissions.0"),
        "`vera` does not hold `run.cancel` on any organisation or project the token is \
         restricted to, and a token carries only what its creator holds"
    );

    let restricted = bearer(&token(&site, &session, viewing(&["globalart/keel"], 10)).await);
    invalid(
        &token(&site, &restricted, viewing(&[], 5)).await,
        &["organisations"],
        "an unrestricted token made with a restricted one",
    );
    invalid(
        &token(&site, &restricted, viewing(&["globalart/hidden"], 5)).await,
        &["projects.0"],
        "a token for a project its maker does not reach",
    );
    invalid(
        &token(&site, &restricted, viewing(&["globalart/keel"], 20)).await,
        &["expires_at"],
        "a token that outlives its maker",
    );
    status(
        &token(&site, &restricted, viewing(&["globalart/keel"], 5)).await,
        StatusCode::CREATED,
        "a token within its maker",
    );
}

#[tokio::test]
async fn a_token_loses_a_permission_its_creator_loses() {
    let site = Site::new().await;
    let project = site.project().await;
    let deploy = site.workflow(project.id, "deploy").await;
    let owner = owner_credentials(&site).await;
    let (dana, _) = site.user("dana").await;
    let binding = post(
        &site,
        "/api/v1/role-bindings",
        &owner,
        json!({
            "role": "developer",
            "user": "dana",
            "team": null,
            "organisation": "globalart",
            "project": null
        }),
    )
    .await;
    status(&binding, StatusCode::CREATED, "a developer's binding");
    let session = site.signed(dana, "dana").await;
    let dispatching = bearer(
        &token(
            &site,
            &session,
            json!({
                "name": "deploys",
                "permissions": ["project.view", "run.dispatch"],
                "organisations": [],
                "projects": [],
                "expires_at": ahead(30)
            }),
        )
        .await,
    );
    status(
        &site.dispatch(&deploy, &dispatching).await,
        StatusCode::CREATED,
        "a dispatch while its creator develops",
    );

    status(
        &delete(
            &site,
            &format!("/api/v1/role-bindings/{}", id(&binding)),
            &owner,
        )
        .await,
        StatusCode::NO_CONTENT,
        "the removal of the binding",
    );
    site.bind("viewer", Subject::User(dana), project.organisation_id, None)
        .await;
    let refused = site.dispatch(&deploy, &dispatching).await;
    forbidden(&refused, "a dispatch after its creator lost run.dispatch");
    let listed = site.get("/api/v1/tokens", &dispatching).await;
    status(&listed, StatusCode::OK, "the tokens");
    let carried: Vec<&Value> = listed.body["items"]
        .as_array()
        .expect("a page")
        .iter()
        .filter(|item| item["name"] == "deploys")
        .map(|item| &item["permissions"])
        .collect();
    assert_eq!(carried, [&json!(["project.view", "run.dispatch"])]);
}

#[tokio::test]
async fn a_custom_role_holds_known_keys_and_its_binding_grants_exactly_them() {
    let site = Site::new().await;
    let project = site.project().await;
    let deploy = site.workflow(project.id, "deploy").await;
    let (ada, _) = member(&site, "ada", "admin", &project).await;
    let session = site.signed(ada, "ada").await;
    let (_, tom) = site.user("tom").await;

    invalid(
        &post(
            &site,
            "/api/v1/roles",
            &session,
            json!({
                "organisation": "globalart",
                "name": "releaser",
                "permissions": ["run.dispatch", "future.thing"]
            }),
        )
        .await,
        &["permissions.1"],
        "a role with an unknown key",
    );
    for (name, expected) in [
        ("admin", StatusCode::CONFLICT),
        (
            "0192f3a4-5b6c-7d8e-9f01-000000000014",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        status(
            &post(
                &site,
                "/api/v1/roles",
                &session,
                json!({ "organisation": "globalart", "name": name, "permissions": [] }),
            )
            .await,
            expected,
            name,
        );
    }
    let role = post(
        &site,
        "/api/v1/roles",
        &session,
        fixture("create-role-request.json"),
    )
    .await;
    status(&role, StatusCode::CREATED, "the role of the fixture");
    let shape = fixture("role.json");
    for field in ["name", "permissions"] {
        assert_eq!(role.body[field], shape[field], "{field}");
    }
    assert_eq!(role.body["organisation_id"], json!(project.organisation_id));
    status(
        &post(
            &site,
            "/api/v1/roles",
            &session,
            fixture("create-role-request.json"),
        )
        .await,
        StatusCode::CONFLICT,
        "a second role of the name",
    );
    let roles = site
        .get("/api/v1/roles?organisation=globalart", &session)
        .await;
    let names: Vec<&str> = roles.body["items"]
        .as_array()
        .expect("a page")
        .iter()
        .map(|role| role["name"].as_str().expect("a name"))
        .collect();
    assert_eq!(
        names,
        ["viewer", "developer", "maintainer", "admin", "releaser"]
    );

    status(
        &post(
            &site,
            "/api/v1/role-bindings",
            &session,
            json!({
                "role": "releaser",
                "user": "tom",
                "team": null,
                "organisation": "globalart",
                "project": null
            }),
        )
        .await,
        StatusCode::CREATED,
        "a binding of the custom role",
    );
    assert_eq!(
        site.get("/api/v1/me", &tom).await.body["permissions"],
        json!({
            "installation": [],
            "organisations": { project.organisation_id.to_string(): ["run.dispatch", "run.cancel"] },
            "projects": {}
        })
    );
    status(
        &site.dispatch(&deploy, &tom).await,
        StatusCode::CREATED,
        "a dispatch through the custom role",
    );
    forbidden(
        &site.get("/api/v1/projects/globalart/keel", &tom).await,
        "a view of the project without project.view",
    );

    let changed = site
        .send(
            Method::PATCH,
            &format!("/api/v1/roles/{}", id(&role)),
            &session,
            Some(json!({ "permissions": ["project.view"] })),
        )
        .await;
    status(&changed, StatusCode::OK, "a change of the role");
    assert_eq!(changed.body["name"], "releaser");
    forbidden(
        &site.dispatch(&deploy, &tom).await,
        "a dispatch after the role lost run.dispatch",
    );
    status(
        &site.get("/api/v1/projects/globalart/keel", &tom).await,
        StatusCode::OK,
        "a view after the role gained project.view",
    );
    let viewer = roles.body["items"][0]["id"]
        .as_str()
        .expect("the id of viewer")
        .to_owned();
    for (method, body) in [
        (Method::PATCH, Some(json!({ "name": "watcher" }))),
        (Method::DELETE, None),
    ] {
        let refused = site
            .send(
                method.clone(),
                &format!("/api/v1/roles/{viewer}"),
                &session,
                body,
            )
            .await;
        status(&refused, StatusCode::CONFLICT, &format!("{method} viewer"));
        assert_eq!(refused.body["error"]["code"], "invalid_state");
    }
    status(
        &delete(&site, &format!("/api/v1/roles/{}", id(&role)), &session).await,
        StatusCode::NO_CONTENT,
        "the deletion of the role",
    );
    assert_eq!(
        site.get("/api/v1/me", &tom).await.body["permissions"],
        json!({ "installation": [], "organisations": {}, "projects": {} })
    );
}

#[tokio::test]
async fn an_organisations_admin_manages_its_teams_and_bindings_and_only_owners_create_users() {
    let site = Site::new().await;
    let project = site.project().await;
    let deploy = site.workflow(project.id, "deploy").await;
    let acme = organisation(&site, "acme").await;
    organisation(&site, "elsewhere").await;
    let (ada, _) = member(&site, "ada", "admin", &project).await;
    site.bind("viewer", Subject::User(ada), acme, None).await;
    let session = site.signed(ada, "ada").await;
    let (tom, tom_credentials) = site.user("tom").await;

    let team = post(
        &site,
        "/api/v1/teams",
        &session,
        fixture("create-team-request.json"),
    )
    .await;
    status(&team, StatusCode::CREATED, "the team of the fixture");
    let shape = fixture("team.json");
    for field in ["slug", "name"] {
        assert_eq!(team.body[field], shape[field], "{field}");
    }
    let members = format!("/api/v1/teams/{}/members", id(&team));
    invalid(
        &post(
            &site,
            &members,
            &session,
            fixture("add-member-request.json"),
        )
        .await,
        &["user"],
        "a member who does not exist",
    );
    for refused in [
        post(
            &site,
            "/api/v1/users",
            &session,
            fixture("create-user-request.json"),
        )
        .await,
        site.get("/api/v1/users", &session).await,
    ] {
        forbidden(&refused, "users managed by an organisation's admin");
        assert_eq!(
            refused.body["error"]["message"],
            "`ada` does not hold `organisation.manage` on this installation, which only its \
             owners do"
        );
    }
    let owner = owner_credentials(&site).await;
    let created = post(
        &site,
        "/api/v1/users",
        &owner,
        fixture("create-user-request.json"),
    )
    .await;
    status(&created, StatusCode::CREATED, "a user created by an owner");
    let shape = fixture("user.json");
    for field in ["login", "name", "email", "owner"] {
        assert_eq!(created.body[field], shape[field], "{field}");
    }
    let dana = id(&created);
    for expected in [StatusCode::CREATED, StatusCode::OK] {
        let added = post(
            &site,
            &members,
            &session,
            fixture("add-member-request.json"),
        )
        .await;
        status(&added, expected, "dana joins the team");
        assert_eq!(added.body["login"], "dana");
    }
    status(
        &post(&site, &members, &session, json!({ "user": tom })).await,
        StatusCode::CREATED,
        "tom joins the team by id",
    );
    assert_eq!(
        ids(&site.get(&members, &session).await),
        BTreeSet::from([tom.to_string(), dana])
    );
    assert_eq!(
        site.get("/api/v1/teams?organisation=globalart", &session)
            .await
            .body["items"]
            .as_array()
            .map(|teams| teams.iter().map(|team| team["slug"].clone()).collect()),
        Some(vec![json!("platform")])
    );

    let binding = post(
        &site,
        "/api/v1/role-bindings",
        &session,
        fixture("create-role-binding-request.json"),
    )
    .await;
    status(&binding, StatusCode::CREATED, "the binding of the fixture");
    let shape = fixture("role-binding.json");
    assert_eq!(
        binding
            .body
            .as_object()
            .map(|body| body.keys().collect::<Vec<_>>()),
        shape
            .as_object()
            .map(|body| body.keys().collect::<Vec<_>>())
    );
    assert_eq!(binding.body["role"], shape["role"]);
    assert_eq!(binding.body["user"], Value::Null);
    assert_eq!(binding.body["team"]["slug"], shape["team"]["slug"]);
    assert_eq!(binding.body["project"]["path"], shape["project"]["path"]);
    assert!(
        ids(&site
            .get("/api/v1/role-bindings?organisation=globalart", &session)
            .await)
        .contains(&id(&binding))
    );
    status(
        &site.dispatch(&deploy, &tom_credentials).await,
        StatusCode::CREATED,
        "a maintainer's dispatch through the team",
    );
    status(
        &delete(&site, &format!("{members}/{tom}"), &session).await,
        StatusCode::NO_CONTENT,
        "tom leaves the team",
    );
    absent(
        &site.dispatch(&deploy, &tom_credentials).await,
        &format!("there is no workflow `{}`", deploy.id),
    );
    absent(
        &delete(&site, &format!("{members}/{tom}"), &session).await,
        &format!("there is no member `{tom}` in team `platform`"),
    );

    for (path, body) in [
        (
            "/api/v1/teams",
            json!({ "organisation": "acme", "slug": "platform", "name": "Platform" }),
        ),
        (
            "/api/v1/role-bindings",
            json!({
                "role": "viewer",
                "user": "tom",
                "team": null,
                "organisation": "acme",
                "project": null
            }),
        ),
    ] {
        let refused = post(&site, path, &session, body).await;
        forbidden(&refused, &format!("{path} in another organisation"));
        assert_eq!(
            refused.body["error"]["message"],
            "`ada` does not hold `organisation.manage` on organisation `acme`"
        );
    }
    forbidden(
        &site.get("/api/v1/teams?organisation=acme", &session).await,
        "the teams of another organisation",
    );
    let unseen = post(
        &site,
        "/api/v1/teams",
        &session,
        json!({ "organisation": "elsewhere", "slug": "platform", "name": "Platform" }),
    )
    .await;
    invalid(
        &unseen,
        &["organisation"],
        "an organisation ada holds nothing on",
    );
    assert_eq!(
        message(&unseen, "organisation"),
        "there is no organisation `elsewhere`"
    );
}

#[tokio::test]
async fn a_binding_names_a_role_a_subject_and_a_project_of_its_own_organisation() {
    let site = Site::new().await;
    let project = site.project().await;
    let acme = organisation(&site, "acme").await;
    bosun_db::projects::create(site.db(), acme, "ops", "Operations")
        .await
        .expect("a project of another organisation");
    bosun_db::teams::create(site.db(), acme, "outsiders", "Outsiders")
        .await
        .expect("a team of another organisation");
    bosun_db::teams::create(site.db(), project.organisation_id, "platform", "Platform")
        .await
        .expect("a team");
    let foreign = bosun_db::roles::create(
        site.db(),
        acme,
        "releaser",
        &BTreeSet::from([bosun_types::permission::Permission::RunDispatch]),
    )
    .await
    .expect("a role of another organisation");
    site.user("tom").await;
    let owner = owner_credentials(&site).await;
    let bind = |role: Value, user: Value, team: Value, project: Value| {
        json!({
            "role": role,
            "user": user,
            "team": team,
            "organisation": "globalart",
            "project": project
        })
    };
    for (body, field, context) in [
        (
            bind(json!("releaser"), json!("tom"), Value::Null, Value::Null),
            "role",
            "a custom role of another organisation by name",
        ),
        (
            bind(json!(foreign.id), json!("tom"), Value::Null, Value::Null),
            "role",
            "a custom role of another organisation by id",
        ),
        (
            bind(
                json!("viewer"),
                Value::Null,
                json!("outsiders"),
                Value::Null,
            ),
            "team",
            "a team of another organisation",
        ),
        (
            bind(
                json!("viewer"),
                json!("tom"),
                Value::Null,
                json!("acme/ops"),
            ),
            "project",
            "a project of another organisation",
        ),
        (
            bind(
                json!("viewer"),
                json!("tom"),
                json!("platform"),
                Value::Null,
            ),
            "team",
            "a user and a team",
        ),
        (
            bind(json!("viewer"), Value::Null, Value::Null, Value::Null),
            "user",
            "neither a user nor a team",
        ),
        (
            bind(json!("viewer"), json!("nobody"), Value::Null, Value::Null),
            "user",
            "a user who does not exist",
        ),
    ] {
        invalid(
            &post(&site, "/api/v1/role-bindings", &owner, body).await,
            &[field],
            context,
        );
    }
    let viewer = bind(
        json!("viewer"),
        json!("tom"),
        Value::Null,
        json!("globalart/keel"),
    );
    status(
        &post(&site, "/api/v1/role-bindings", &owner, viewer.clone()).await,
        StatusCode::CREATED,
        "a binding on a project",
    );
    status(
        &post(&site, "/api/v1/role-bindings", &owner, viewer).await,
        StatusCode::CONFLICT,
        "the same binding again",
    );
    invalid(
        &post(
            &site,
            "/api/v1/teams",
            &owner,
            json!({
                "organisation": "globalart",
                "slug": "0192f3a4-5b6c-7d8e-9f01-000000000013",
                "name": "Ids"
            }),
        )
        .await,
        &["slug"],
        "a team slug that reads as an id",
    );
}

#[tokio::test]
async fn a_user_created_with_an_initial_password_sets_a_new_one_at_the_first_sign_in() {
    let site = Site::new().await;
    let owner = owner_credentials(&site).await;
    let created = post(
        &site,
        "/api/v1/users",
        &owner,
        json!({
            "login": "nina",
            "name": "Nina",
            "email": "nina@example.org",
            "password": PASSWORD
        }),
    )
    .await;
    status(&created, StatusCode::CREATED, "a user");
    assert_eq!(created.body["owner"], false);

    let (signed, session) = site.sign_in("nina", PASSWORD).await;
    status(&signed, StatusCode::OK, "the first sign-in");
    assert_eq!(signed.body["password_change_required"], true);
    let session = session.expect("a session");
    let refused = site.get("/api/v1/projects", &session).await;
    forbidden(&refused, "a request before the new password");
    assert_eq!(
        refused.body["error"]["message"],
        "`nina` must set a new password before anything else; POST /api/v1/auth/password sets it"
    );
    status(
        &post(
            &site,
            "/api/v1/auth/password",
            &session,
            json!({ "current_password": PASSWORD, "new_password": NEW_PASSWORD }),
        )
        .await,
        StatusCode::OK,
        "the new password",
    );
    status(
        &site.get("/api/v1/projects", &session).await,
        StatusCode::OK,
        "a request after the new password",
    );
    let (again, _) = site.sign_in("nina", NEW_PASSWORD).await;
    assert_eq!(again.body["password_change_required"], false);

    for (body, field) in [
        (
            json!({
                "login": "0192f3a4-5b6c-7d8e-9f01-000000000003",
                "name": "Ids",
                "email": "ids@example.org",
                "password": PASSWORD
            }),
            "login",
        ),
        (
            json!({ "login": "omar", "name": "Omar", "email": "omar", "password": PASSWORD }),
            "email",
        ),
        (
            json!({
                "login": "omar",
                "name": "Omar",
                "email": "omar@example.org",
                "password": "short"
            }),
            "password",
        ),
    ] {
        invalid(
            &post(&site, "/api/v1/users", &owner, body).await,
            &[field],
            field,
        );
    }
    status(
        &post(
            &site,
            "/api/v1/users",
            &owner,
            json!({
                "login": "nina",
                "name": "Nina again",
                "email": "nina@example.org",
                "password": PASSWORD
            }),
        )
        .await,
        StatusCode::CONFLICT,
        "a login that is taken",
    );

    let me = site.get("/api/v1/me", &owner).await;
    let itself = delete(
        &site,
        &format!(
            "/api/v1/users/{}",
            me.body["user"]["id"].as_str().expect("an id")
        ),
        &owner,
    )
    .await;
    status(
        &itself,
        StatusCode::CONFLICT,
        "an owner deleting themselves",
    );
    assert_eq!(itself.body["error"]["code"], "invalid_state");
    status(
        &delete(&site, &format!("/api/v1/users/{}", id(&created)), &owner).await,
        StatusCode::NO_CONTENT,
        "the deletion of the user",
    );
    status(
        &site.get("/api/v1/me", &session).await,
        StatusCode::UNAUTHORIZED,
        "the session of a deleted user",
    );
}

#[tokio::test]
async fn a_token_is_listed_and_revoked_by_its_owner_alone() {
    let site = Site::new().await;
    let project = site.project().await;
    let (dana, _) = member(&site, "dana", "viewer", &project).await;
    let session = site.signed(dana, "dana").await;
    let (_, tom) = member(&site, "tom", "viewer", &project).await;
    let created = token(&site, &session, viewing(&[], 30)).await;
    let credentials = bearer(&created);
    let path = format!("/api/v1/tokens/{}", id(&created));

    let listed = site.get("/api/v1/tokens", &session).await;
    assert!(ids(&listed).contains(&id(&created)), "{listed:?}");
    assert!(
        !ids(&site.get("/api/v1/tokens", &tom).await).contains(&id(&created)),
        "tom lists dana's token"
    );
    absent(
        &delete(&site, &path, &tom).await,
        &format!("there is no token `{}`", id(&created)),
    );
    status(
        &site.get("/api/v1/me", &credentials).await,
        StatusCode::OK,
        "the token before it is revoked",
    );
    for attempt in ["the revocation", "a second revocation"] {
        status(
            &delete(&site, &path, &session).await,
            StatusCode::NO_CONTENT,
            attempt,
        );
    }
    let refused = site.get("/api/v1/me", &credentials).await;
    status(&refused, StatusCode::UNAUTHORIZED, "a revoked token");
    assert_eq!(
        refused.body["error"]["message"],
        "the bearer token was revoked"
    );
    let revoked = site.get("/api/v1/tokens", &session).await;
    let item = revoked.body["items"]
        .as_array()
        .expect("a page")
        .iter()
        .find(|item| item["id"] == created.body["id"])
        .cloned()
        .expect("the revoked token");
    assert!(item["revoked_at"].is_string(), "{item}");
    let shape = fixture("personal-token.json");
    assert_eq!(
        item.as_object().map(|item| item.keys().collect::<Vec<_>>()),
        shape
            .as_object()
            .map(|shape| shape.keys().collect::<Vec<_>>())
    );
}

#[tokio::test]
async fn a_token_restricted_to_a_project_that_is_gone_reaches_nothing() {
    let site = Site::new().await;
    let hidden = site.second_project("hidden").await;
    let owner = owner_credentials(&site).await;
    let created = token(&site, &owner, viewing(&["globalart/hidden"], 30)).await;
    let restricted = bearer(&created);
    assert_eq!(
        ids(&site.get("/api/v1/projects", &restricted).await),
        BTreeSet::from([hidden.id.to_string()])
    );
    project::Entity::delete_by_id(Uuid::from(hidden.id))
        .exec(site.db())
        .await
        .expect("the deletion of the project");
    assert_eq!(
        site.get("/api/v1/me", &restricted).await.body["permissions"],
        json!({ "installation": [], "organisations": {}, "projects": {} })
    );
    assert_eq!(
        ids(&site.get("/api/v1/projects", &restricted).await),
        BTreeSet::new()
    );
    let listed = site.get("/api/v1/tokens", &restricted).await;
    let item = listed.body["items"]
        .as_array()
        .expect("a page")
        .iter()
        .find(|item| item["id"] == created.body["id"])
        .expect("the restricted token");
    assert_eq!(item["restricted"], true, "{listed:?}");
    assert_eq!(item["projects"], json!([]));
    forbidden(
        &post(
            &site,
            "/api/v1/organisations",
            &restricted,
            json!({ "slug": "more", "name": "More" }),
        )
        .await,
        "an organisation created with a restricted token of an owner",
    );
}
