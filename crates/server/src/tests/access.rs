use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::net::SocketAddr;
use std::path::Path;

use axum::Router;
use axum::body::{self, Body};
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use bosun_db::agents::NewRegistrationToken;
use bosun_db::entity::{organisation, project, role};
use bosun_db::roles::{NewBinding, Subject};
use bosun_db::tokens::NewToken;
use bosun_db::workflows::NewWorkflow;
use bosun_types::api::{Permissions, Project, Workflow};
use bosun_types::id::{
    ExecutionId, JobId, OrganisationId, ProjectId, RoleId, RunId, StepId, TeamId, TokenId, UserId,
};
use bosun_types::permission::Permission;
use chrono::{TimeDelta, Utc};
use sea_orm::prelude::Uuid;
use sea_orm::{ActiveModelTrait, ActiveValue, ColumnTrait, EntityTrait, QueryFilter};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use super::{World, template, workflow};
use crate::agents::progress;
use crate::clock::Clock;
use crate::commands::{self, COMMANDS};
use crate::site::Site as Public;
use crate::token::{PERSONAL, REGISTRATION, Token};
use crate::{AppState, api, sign_in};

pub(super) const PASSWORD: &str = "correct horse battery staple";

pub(super) const NEW_PASSWORD: &str = "correct horse battery stapler";

const SOURCE: &str = concat!(
    "name: deploy\n",
    "on: workflow_dispatch\n",
    "jobs:\n",
    "  deploy:\n",
    "    runs-on: [self-hosted]\n",
    "    steps:\n",
    "      - run: echo deploy\n",
);

const OWNER: &str = "owner";

const ENDPOINTS: &str = "| Operation | Permission | On |";

const ROLES: &str = "| Role | Permissions |";

const COMMAND_TABLE: &str = "| Command | Valid when |";

pub(super) fn document(path: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn table(text: &str, marker: &str) -> Vec<Vec<String>> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.starts_with(marker))
        .unwrap_or_else(|| panic!("no table `{marker}`"));
    lines[start + 2..]
        .iter()
        .take_while(|line| line.starts_with('|'))
        .map(|line| {
            line.trim()
                .trim_start_matches('|')
                .trim_end_matches('|')
                .split('|')
                .map(|cell| cell.trim().to_owned())
                .collect()
        })
        .collect()
}

fn quoted(cell: &str) -> Vec<String> {
    cell.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_owned)
        .collect()
}

fn key(text: &str) -> Permission {
    Permission::from_key(text).unwrap_or_else(|| panic!("`{text}` is not a permission key"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Needs {
    Anyone,
    Authenticated,
    Installation(Permission),
    Organisation(Permission),
    Project(Permission),
    Items(Permission),
    Command,
}

impl Needs {
    fn declared(declaration: &Value) -> Needs {
        let on = declaration["on"].as_str().expect("an `on`");
        let held = || key(declaration["key"].as_str().expect("a `key`"));
        match on {
            "anyone" => Needs::Anyone,
            "authenticated" => Needs::Authenticated,
            "installation" => Needs::Installation(held()),
            "organisation" => Needs::Organisation(held()),
            "project" if declaration.get("by_command").is_some() => Needs::Command,
            "project" => Needs::Project(held()),
            "items" => Needs::Items(held()),
            other => panic!("`{other}` is not a scope of x-bosun-permission"),
        }
    }

    fn documented(permission: &str, on: &str) -> Needs {
        let keys = quoted(permission);
        let scope = quoted(on);
        let [scope] = scope.as_slice() else {
            panic!("`{on}` does not name one scope");
        };
        let held = || match keys.as_slice() {
            [one] => key(one),
            _ => panic!("`{permission}` does not name one permission"),
        };
        match scope.as_str() {
            "anyone" | "authenticated" => {
                assert_eq!(permission, "—", "`{scope}` takes no permission");
                if scope == "anyone" {
                    Needs::Anyone
                } else {
                    Needs::Authenticated
                }
            }
            "installation" => Needs::Installation(held()),
            "organisation" => Needs::Organisation(held()),
            "project" if permission.starts_with("the command's") => Needs::Command,
            "project" => Needs::Project(held()),
            "items" => Needs::Items(held()),
            other => panic!("`{other}` is not a scope of the endpoint table"),
        }
    }
}

struct Endpoint {
    method: String,
    path: String,
    needs: Needs,
}

fn endpoints() -> Vec<Endpoint> {
    table(&document("docs/design/11-api.md"), ENDPOINTS)
        .iter()
        .map(|row| {
            let operation = quoted(&row[0]);
            let (method, path) = operation
                .first()
                .and_then(|operation| operation.split_once(' '))
                .unwrap_or_else(|| panic!("`{}` is not an operation", row[0]));
            Endpoint {
                method: method.to_owned(),
                path: format!("/api/v1{path}"),
                needs: Needs::documented(&row[1], &row[2]),
            }
        })
        .collect()
}

fn roles() -> BTreeMap<String, BTreeSet<Permission>> {
    let mut roles: BTreeMap<String, BTreeSet<Permission>> = BTreeMap::new();
    for row in table(&document("docs/design/08-access.md"), ROLES) {
        let name = quoted(&row[0]).pop().expect("a role name");
        let mut keys = BTreeSet::new();
        for item in row[1].split(',').map(str::trim) {
            match item
                .strip_prefix('`')
                .and_then(|item| item.strip_suffix('`'))
            {
                Some(pattern) => match pattern.strip_suffix('*') {
                    Some(prefix) => keys.extend(
                        Permission::ALL
                            .into_iter()
                            .filter(|permission| permission.key().starts_with(prefix)),
                    ),
                    None => {
                        keys.insert(key(pattern));
                    }
                },
                None => keys.extend(
                    roles
                        .get(item)
                        .unwrap_or_else(|| panic!("`{item}` names no role above `{name}`"))
                        .clone(),
                ),
            }
        }
        roles.insert(name, keys);
    }
    roles
}

fn command_permissions() -> BTreeMap<String, Permission> {
    table(&document("docs/design/04-runs.md"), COMMAND_TABLE)
        .iter()
        .filter_map(|row| {
            let command = quoted(&row[0]).pop()?;
            let permission = quoted(&row[3]).first().map(|text| key(text))?;
            Some((command, permission))
        })
        .collect()
}

fn committed() -> Value {
    serde_json::from_str(&document("contracts/openapi.json")).expect("the document is JSON")
}

#[derive(Debug)]
pub(super) struct Answer {
    pub(super) status: StatusCode,
    pub(super) headers: HeaderMap,
    pub(super) body: Value,
}

pub(super) enum Credentials {
    Bearer(String),
    Session { cookie: String, csrf: String },
}

struct Fixture {
    project: Project,
    deploy: Workflow,
    run: RunId,
    job: JobId,
    execution: ExecutionId,
    step: StepId,
    team: TeamId,
}

pub(super) struct Site {
    world: World,
    state: AppState,
    router: Router,
    _directory: TempDir,
}

impl Site {
    pub(super) async fn new() -> Site {
        let world = World::new().await;
        let directory = tempfile::tempdir().expect("a directory");
        let state = super::state(
            &world.database,
            directory.path(),
            Clock::system(),
            sign_in::Settings::new(Vec::new(), 1000, directory.path().join("data")),
        )
        .await;
        let router = api().into_router(&state).with_state(state.clone());
        Site {
            world,
            state,
            router,
            _directory: directory,
        }
    }

    pub(super) fn db(&self) -> &sea_orm::DatabaseConnection {
        self.world.db()
    }

    pub(super) async fn project(&self) -> Project {
        bosun_db::projects::find(self.db(), self.world.workflow.project_id)
            .await
            .expect("a project")
            .expect("the project of the world")
    }

    pub(super) async fn second_project(&self, slug: &str) -> Project {
        let project = self.project().await;
        bosun_db::projects::create(self.db(), project.organisation_id, slug, slug)
            .await
            .expect("a project")
    }

    pub(super) async fn workflow(&self, project: ProjectId, name: &str) -> Workflow {
        bosun_db::workflows::create(
            self.db(),
            NewWorkflow {
                project_id: project,
                name,
                source: SOURCE,
                sha256: vec![7; 32],
            },
        )
        .await
        .expect("a workflow")
    }

    pub(super) async fn user(&self, login: &str) -> (UserId, Credentials) {
        let user = bosun_db::users::create(self.db(), login)
            .await
            .expect("a user");
        let token = Token::generate(PERSONAL).expect("random bytes");
        bosun_db::tokens::create(
            self.db(),
            user.id,
            NewToken {
                name: "cli".to_owned(),
                sha256: token.sha256(),
                expires_at: Utc::now() + TimeDelta::days(30),
            },
        )
        .await
        .expect("a token");
        (user.id, Credentials::Bearer(token.into_string()))
    }

    pub(super) async fn owner(&self, login: &str) -> UserId {
        let token = Token::generate(PERSONAL).expect("random bytes");
        bosun_db::users::create_owner(
            self.db(),
            login,
            NewToken {
                name: "cli".to_owned(),
                sha256: token.sha256(),
                expires_at: Utc::now() + TimeDelta::days(30),
            },
        )
        .await
        .expect("an owner")
        .id
    }

    pub(super) async fn bind(
        &self,
        role: &str,
        subject: Subject,
        organisation: OrganisationId,
        project: Option<ProjectId>,
    ) {
        let role_id = bosun_db::roles::builtin(self.db(), role)
            .await
            .expect("a role")
            .unwrap_or_else(|| panic!("no built-in role `{role}`"));
        bosun_db::roles::bind(
            self.db(),
            NewBinding {
                role_id,
                subject,
                organisation_id: organisation,
                project_id: project,
            },
        )
        .await
        .expect("a binding");
    }

    pub(super) async fn signed(&self, user: UserId, login: &str) -> Credentials {
        let hash = self
            .state
            .sign_in
            .hasher
            .hash(PASSWORD.to_owned())
            .await
            .expect("a hash");
        bosun_db::users::set_password(self.db(), user, hash)
            .await
            .expect("a password");
        let (answer, session) = self.sign_in(login, PASSWORD).await;
        assert_eq!(
            answer.status,
            StatusCode::OK,
            "{login} signs in: {answer:?}"
        );
        session.expect("a session")
    }

    pub(super) async fn sign_in(
        &self,
        login: &str,
        password: &str,
    ) -> (Answer, Option<Credentials>) {
        let response = self
            .router
            .clone()
            .oneshot(
                request(Method::POST, "/api/v1/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({ "login": login, "password": password }).to_string(),
                    ))
                    .expect("a request"),
            )
            .await
            .expect("an answer");
        let status = response.status();
        let headers = response.headers().clone();
        let body = read(response.into_body()).await;
        let session = headers
            .get("set-cookie")
            .and_then(|cookie| cookie.to_str().ok())
            .and_then(|cookie| cookie.split(';').next())
            .zip(body["csrf_token"].as_str())
            .map(|(cookie, csrf)| Credentials::Session {
                cookie: cookie.to_owned(),
                csrf: csrf.to_owned(),
            });
        (
            Answer {
                status,
                headers,
                body,
            },
            session,
        )
    }

    pub(super) async fn send(
        &self,
        method: Method,
        path: &str,
        credentials: &Credentials,
        body: Option<Value>,
    ) -> Answer {
        self.send_with(method, path, credentials, &[], body).await
    }

    pub(super) async fn send_with(
        &self,
        method: Method,
        path: &str,
        credentials: &Credentials,
        headers: &[(&str, String)],
        body: Option<Value>,
    ) -> Answer {
        let mut builder = request(method.clone(), path);
        for (name, value) in headers {
            builder = builder.header(*name, value);
        }
        match credentials {
            Credentials::Bearer(token) => {
                builder = builder.header("authorization", format!("Bearer {token}"));
            }
            Credentials::Session { cookie, csrf } => {
                builder = builder.header("cookie", cookie);
                if method != Method::GET {
                    builder = builder.header("x-bosun-csrf", csrf);
                }
            }
        }
        let body = match body {
            Some(body) => {
                builder = builder.header("content-type", "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let response = self
            .router
            .clone()
            .oneshot(builder.body(body).expect("a request"))
            .await
            .expect("an answer");
        let status = response.status();
        let headers = response.headers().clone();
        Answer {
            status,
            headers,
            body: read(response.into_body()).await,
        }
    }

    pub(super) async fn get(&self, path: &str, credentials: &Credentials) -> Answer {
        self.send(Method::GET, path, credentials, None).await
    }

    pub(super) async fn dispatch(&self, workflow: &Workflow, credentials: &Credentials) -> Answer {
        self.send(
            Method::POST,
            &format!("/api/v1/workflows/{}/dispatch", workflow.id),
            credentials,
            Some(json!({ "ref": null, "inputs": {} })),
        )
        .await
    }

    pub(super) async fn run(&self) -> (RunId, JobId) {
        let run = self
            .world
            .dispatch(&workflow(vec![template("build", &[], 1)]))
            .await;
        let (job, _) = self.world.project(run).await.job("build");
        (run, job)
    }

    async fn fixture(&self) -> Fixture {
        let project = self.project().await;
        let deploy = self.workflow(project.id, "deploy").await;
        let (run, job) = self.run().await;
        let execution = self.world.assign(run, job).await;
        progress::accepted(self.db(), self.world.agent(), execution, &Public::default())
            .await
            .expect("an acceptance");
        progress::step_started(self.db(), self.world.agent(), execution, 0, None)
            .await
            .expect("a started step");
        let (step, _) = self
            .world
            .project(run)
            .await
            .steps
            .into_iter()
            .find(|(_, step)| step.execution == execution)
            .expect("the started step");
        let team = self.team(project.organisation_id, "fixture").await;
        Fixture {
            project,
            deploy,
            run,
            job,
            execution,
            step,
            team,
        }
    }

    async fn team(&self, organisation: OrganisationId, slug: &str) -> TeamId {
        bosun_db::teams::create(self.db(), organisation, slug, slug)
            .await
            .expect("a team")
            .id
    }

    async fn custom_role(&self, organisation: OrganisationId, name: &str) -> RoleId {
        bosun_db::roles::create(
            self.db(),
            organisation,
            name,
            &BTreeSet::from([Permission::ProjectView]),
        )
        .await
        .expect("a custom role")
        .id
    }

    async fn personal_token(&self, user: UserId) -> TokenId {
        bosun_db::tokens::create(
            self.db(),
            user,
            NewToken {
                name: "spare".to_owned(),
                sha256: Token::generate(PERSONAL).expect("random bytes").sha256(),
                expires_at: Utc::now() + TimeDelta::days(1),
            },
        )
        .await
        .expect("a token")
    }

    async fn registration_token(&self) -> String {
        let pool = bosun_db::pools::default_pool(self.db())
            .await
            .expect("the default pool");
        let token = Token::generate(REGISTRATION).expect("random bytes");
        bosun_db::agents::create_registration_token(
            self.db(),
            NewRegistrationToken {
                pool_id: pool.id,
                sha256: token.sha256(),
                reusable: false,
                expires_at: Utc::now() + TimeDelta::hours(1),
            },
        )
        .await
        .expect("a registration token");
        token.into_string()
    }
}

fn request(method: Method, path: &str) -> axum::http::request::Builder {
    let peer: SocketAddr = "192.0.2.10:41234".parse().expect("a peer");
    Request::builder()
        .method(method)
        .uri(path)
        .extension(ConnectInfo(peer))
}

async fn read(body: Body) -> Value {
    let bytes = body::to_bytes(body, usize::MAX).await.expect("a body");
    if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("a JSON body")
    }
}

pub(super) fn forbidden(answer: &Answer, context: &str) {
    assert_eq!(
        answer.status,
        StatusCode::FORBIDDEN,
        "{context}: {answer:?}"
    );
    assert_eq!(answer.body["error"]["code"], "forbidden", "{context}");
}

pub(super) fn absent(answer: &Answer, message: &str) {
    assert_eq!(answer.status, StatusCode::NOT_FOUND, "{answer:?}");
    assert_eq!(answer.body["error"]["code"], "not_found");
    assert_eq!(answer.body["error"]["message"], message);
}

pub(super) fn ids(page: &Answer) -> BTreeSet<String> {
    assert_eq!(page.status, StatusCode::OK, "{page:?}");
    page.body["items"]
        .as_array()
        .expect("a page")
        .iter()
        .map(|item| item["id"].as_str().expect("an id").to_owned())
        .collect()
}

#[test]
fn the_endpoint_table_is_the_permissions_of_the_document() {
    let document = committed();
    let mut operations = BTreeMap::new();
    for (path, item) in document["paths"].as_object().expect("paths") {
        for (method, operation) in item.as_object().expect("a path item") {
            let declaration = operation
                .get("x-bosun-permission")
                .unwrap_or_else(|| panic!("{method} {path} declares no permission"));
            operations.insert(
                (method.to_uppercase(), path.clone()),
                Needs::declared(declaration),
            );
        }
    }
    let documented: BTreeMap<(String, String), Needs> = endpoints()
        .into_iter()
        .map(|endpoint| ((endpoint.method, endpoint.path), endpoint.needs))
        .collect();
    assert_eq!(
        documented.keys().collect::<Vec<_>>(),
        operations.keys().collect::<Vec<_>>(),
        "the endpoint table of docs/design/11-api.md lists other operations than \
         contracts/openapi.json"
    );
    for (operation, needs) in &documented {
        assert_eq!(
            Some(needs),
            operations.get(operation),
            "{operation:?} needs another permission in docs/design/11-api.md than in \
             contracts/openapi.json"
        );
    }

    let commands = &document["paths"]["/api/v1/runs/{run}/commands"]["post"];
    let declared: BTreeMap<String, Permission> = commands["x-bosun-permission"]["by_command"]
        .as_object()
        .expect("the permissions of the commands")
        .iter()
        .map(|(command, permission)| {
            (
                command.clone(),
                key(permission.as_str().expect("a permission key")),
            )
        })
        .collect();
    let accepted: BTreeSet<String> = document["components"]["schemas"]["CommandRequest"]["oneOf"]
        .as_array()
        .expect("the commands of the request")
        .iter()
        .map(|variant| {
            variant["properties"]["type"]["const"]
                .as_str()
                .expect("a command type")
                .to_owned()
        })
        .collect();
    assert_eq!(
        declared.keys().cloned().collect::<BTreeSet<_>>(),
        accepted,
        "the endpoint declares the permissions of other commands than it takes"
    );
    let table = command_permissions();
    for (command, permission) in &declared {
        assert_eq!(
            table.get(command),
            Some(permission),
            "`{command}` needs another permission in docs/design/04-runs.md"
        );
    }
}

#[tokio::test]
async fn the_built_in_roles_hold_the_keys_of_the_role_table() {
    let site = Site::new().await;
    let stored: BTreeMap<String, BTreeSet<Permission>> = role::Entity::find()
        .filter(role::Column::OrganisationId.is_null())
        .all(site.db())
        .await
        .expect("the built-in roles")
        .into_iter()
        .map(|found| {
            (
                found.name,
                found.permissions.iter().map(|text| key(text)).collect(),
            )
        })
        .collect();
    assert_eq!(stored, roles());
    assert_eq!(
        roles()["admin"],
        Permission::ALL.into_iter().collect::<BTreeSet<_>>()
    );
}

#[tokio::test]
async fn every_built_in_role_is_allowed_or_refused_as_the_two_tables_say() {
    let site = Site::new().await;
    let fixture = site.fixture().await;
    let organisation = fixture.project.organisation_id;
    let commands = command_permissions();
    let mut endpoints = endpoints();
    endpoints.sort_by_key(|endpoint| match endpoint.path.as_str() {
        "/api/v1/auth/password" => 1,
        "/api/v1/auth/logout" => 2,
        _ => 0,
    });
    let mut subjects: Vec<(String, BTreeSet<Permission>)> = roles().into_iter().collect();
    subjects.push((OWNER.to_owned(), Permission::ALL.into_iter().collect()));

    for (name, held) in subjects {
        let login = format!("as-{name}");
        let user = if name == OWNER {
            site.owner(&login).await
        } else {
            let (user, _) = site.user(&login).await;
            site.bind(&name, Subject::User(user), organisation, None)
                .await;
            user
        };
        let session = site.signed(user, &login).await;
        for endpoint in &endpoints {
            let calls = calls(&site, &fixture, endpoint, &name, &login).await;
            assert!(!calls.is_empty(), "{} {}", endpoint.method, endpoint.path);
            for call in calls {
                let allowed = match &endpoint.needs {
                    Needs::Anyone | Needs::Authenticated => true,
                    Needs::Installation(_) => name == OWNER,
                    Needs::Organisation(key) | Needs::Project(key) | Needs::Items(key) => {
                        held.contains(key)
                    }
                    Needs::Command => {
                        let command = call.command.as_deref().expect("a command");
                        held.contains(&commands[command])
                    }
                };
                let answer = site
                    .send_with(
                        call.method.clone(),
                        &call.path,
                        &session,
                        &call.headers,
                        call.body,
                    )
                    .await;
                let context = format!(
                    "{name}: {} {} {}",
                    call.method,
                    call.path,
                    call.command.unwrap_or_default()
                );
                if allowed {
                    assert!(answer.status.is_success(), "{context}: {answer:?}");
                } else {
                    forbidden(&answer, &context);
                }
            }
        }
    }
}

struct Call {
    method: Method,
    path: String,
    headers: Vec<(&'static str, String)>,
    body: Option<Value>,
    command: Option<String>,
}

async fn calls(
    site: &Site,
    fixture: &Fixture,
    endpoint: &Endpoint,
    name: &str,
    login: &str,
) -> Vec<Call> {
    let method: Method = endpoint.method.parse().expect("a method");
    let call = |path: String, body: Option<Value>| Call {
        method: method.clone(),
        path,
        headers: Vec::new(),
        body,
        command: None,
    };
    let workflow = fixture.deploy.id;
    let organisation = fixture.project.organisation_id;
    match (endpoint.method.as_str(), endpoint.path.as_str()) {
        ("GET", "/api/v1/teams" | "/api/v1/roles" | "/api/v1/role-bindings") => {
            return vec![call(
                format!("{}?organisation=globalart", endpoint.path),
                None,
            )];
        }
        ("DELETE", "/api/v1/users/{user}") => {
            let (user, _) = site.user(&format!("deleted-{name}")).await;
            return vec![call(format!("/api/v1/users/{user}"), None)];
        }
        ("DELETE", "/api/v1/teams/{team}") => {
            let team = site.team(organisation, &format!("gone-{name}")).await;
            return vec![call(format!("/api/v1/teams/{team}"), None)];
        }
        ("POST", "/api/v1/teams/{team}/members") => {
            let login = format!("member-{name}");
            site.user(&login).await;
            return vec![call(
                format!("/api/v1/teams/{}/members", fixture.team),
                Some(json!({ "user": login })),
            )];
        }
        ("DELETE", "/api/v1/teams/{team}/members/{user}") => {
            let (user, _) = site.user(&format!("leaver-{name}")).await;
            join(site, fixture.team, user).await;
            return vec![call(
                format!("/api/v1/teams/{}/members/{user}", fixture.team),
                None,
            )];
        }
        ("PATCH", "/api/v1/roles/{role}") => {
            let role = site
                .custom_role(organisation, &format!("patched-{name}"))
                .await;
            return vec![call(
                format!("/api/v1/roles/{role}"),
                Some(json!({ "permissions": ["project.view", "run.cancel"] })),
            )];
        }
        ("DELETE", "/api/v1/roles/{role}") => {
            let role = site
                .custom_role(organisation, &format!("deleted-{name}"))
                .await;
            return vec![call(format!("/api/v1/roles/{role}"), None)];
        }
        ("DELETE", "/api/v1/role-bindings/{binding}") => {
            let (user, _) = site.user(&format!("unbound-{name}")).await;
            let role_id = bosun_db::roles::builtin(site.db(), "viewer")
                .await
                .expect("a role")
                .expect("the role viewer");
            let binding = bosun_db::roles::bind(
                site.db(),
                NewBinding {
                    role_id,
                    subject: Subject::User(user),
                    organisation_id: organisation,
                    project_id: None,
                },
            )
            .await
            .expect("a binding");
            return vec![call(format!("/api/v1/role-bindings/{}", binding.id), None)];
        }
        ("DELETE", "/api/v1/tokens/{token}") => {
            let user = bosun_db::users::find_by_login(site.db(), login)
                .await
                .expect("a lookup")
                .expect("the user of the session");
            let token = site.personal_token(user.id).await;
            return vec![call(format!("/api/v1/tokens/{token}"), None)];
        }
        _ => {}
    }
    let path = match endpoint.path.as_str() {
        "/api/v1/runs/{run}/commands" => {
            let mut calls = Vec::new();
            for command in COMMANDS {
                let (run, job) = site.run().await;
                let name = commands::name(&command);
                let target = if name.starts_with("run.") {
                    json!({ "attempt": null, "job": null, "step": null })
                } else {
                    json!({ "attempt": null, "job": job, "step": null })
                };
                calls.push(Call {
                    method: method.clone(),
                    path: format!("/api/v1/runs/{run}/commands"),
                    headers: Vec::new(),
                    body: Some(json!({ "type": name, "target": target, "reason": null })),
                    command: Some(name.to_owned()),
                });
            }
            return calls;
        }
        other => other
            .replace("{organisation}", "globalart")
            .replace("{project}", &format!("globalart/{}", fixture.project.slug))
            .replace("{workflow}", &workflow.to_string())
            .replace("{run}", &fixture.run.to_string())
            .replace("{attempt}", "1")
            .replace("{job}", &fixture.job.to_string())
            .replace("{execution}", &fixture.execution.to_string())
            .replace("{step}", &fixture.step.to_string())
            .replace("{team}", &fixture.team.to_string()),
    };
    let body = match (endpoint.method.as_str(), endpoint.path.as_str()) {
        ("GET", _) => None,
        ("POST", "/api/v1/auth/login") => Some(json!({ "login": login, "password": PASSWORD })),
        ("POST", "/api/v1/auth/logout") => None,
        ("POST", "/api/v1/auth/password") => Some(json!({
            "current_password": PASSWORD,
            "new_password": NEW_PASSWORD
        })),
        ("POST", "/api/v1/organisations") => {
            Some(json!({ "slug": format!("org-{name}"), "name": "Organisation" }))
        }
        ("POST", "/api/v1/projects") => Some(json!({
            "organisation": "globalart",
            "slug": format!("project-{name}"),
            "name": "Project"
        })),
        ("PATCH", "/api/v1/organisations/{organisation}") => Some(json!({})),
        ("PATCH", "/api/v1/projects/{project}") => Some(json!({ "repository": null })),
        ("POST", "/api/v1/projects/{project}/workflows") => {
            Some(json!({ "name": format!("workflow-{name}"), "source": SOURCE }))
        }
        ("PUT", "/api/v1/workflows/{workflow}/source") => Some(json!({ "source": SOURCE })),
        ("PATCH", "/api/v1/projects/{project}/settings") => {
            let stored = bosun_db::projects::settings(site.db(), fixture.project.id)
                .await
                .expect("a read")
                .expect("the settings of the project");
            return vec![Call {
                headers: vec![("if-match", format!("\"{}\"", stored.version))],
                ..call(path, Some(json!({ "fork_policy": "approval" })))
            }];
        }
        ("POST", "/api/v1/workflows/{workflow}/dispatch") => {
            Some(json!({ "ref": null, "inputs": {} }))
        }
        ("POST", "/api/v1/registration-tokens") => Some(json!({
            "pool": "default",
            "reusable": false,
            "expires_at": null
        })),
        ("POST", "/api/v1/pools") => Some(json!({
            "name": format!("pool-{name}"),
            "trust": "trusted",
            "access": { "all_projects": false, "organisations": ["globalart"], "projects": [] }
        })),
        ("POST", "/api/v1/label-aliases") => Some(json!({
            "label": format!("label-{name}"),
            "organisation": "globalart",
            "pool": "default",
            "capability": null,
            "image": null
        })),
        ("POST", "/api/v1/users") => Some(json!({
            "login": format!("user-{name}"),
            "name": "A user",
            "email": format!("user-{name}@example.org"),
            "password": PASSWORD
        })),
        ("POST", "/api/v1/teams") => Some(json!({
            "organisation": "globalart",
            "slug": format!("team-{name}"),
            "name": "A team"
        })),
        ("POST", "/api/v1/roles") => Some(json!({
            "organisation": "globalart",
            "name": format!("role-{name}"),
            "permissions": ["project.view"]
        })),
        ("POST", "/api/v1/role-bindings") => {
            let login = format!("bound-{name}");
            site.user(&login).await;
            Some(json!({
                "role": "viewer",
                "user": login,
                "team": null,
                "organisation": "globalart",
                "project": null
            }))
        }
        ("POST", "/api/v1/tokens") => Some(json!({
            "name": format!("cli-{name}"),
            "permissions": ["project.view"],
            "organisations": [],
            "projects": [],
            "expires_at": Utc::now() + TimeDelta::days(1)
        })),
        ("POST", "/api/v1/agents/register") => Some(json!({
            "token": site.registration_token().await,
            "name": format!("agent-{name}"),
            "labels": []
        })),
        (method, path) => panic!("the test has no request for {method} {path}; add one"),
    };
    vec![call(path, body)]
}

#[tokio::test]
async fn a_viewer_cannot_dispatch_and_an_organisation_binding_reaches_every_project() {
    let site = Site::new().await;
    let first = site.project().await;
    let second = site.second_project("ops").await;
    let organisation = first.organisation_id;
    let deploys = [
        site.workflow(first.id, "deploy").await,
        site.workflow(second.id, "deploy").await,
    ];
    let (viewer, viewing) = site.user("vera").await;
    site.bind("viewer", Subject::User(viewer), organisation, None)
        .await;
    let (developer, developing) = site.user("dana").await;
    site.bind("developer", Subject::User(developer), organisation, None)
        .await;

    for (deploy, project) in deploys.iter().zip([&first, &second]) {
        let refused = site.dispatch(deploy, &viewing).await;
        forbidden(&refused, "a viewer's dispatch");
        assert_eq!(
            refused.body["error"]["message"],
            format!(
                "`vera` does not hold `run.dispatch` on project `{}`",
                project.slug
            )
        );
        assert_eq!(
            site.get(&format!("/api/v1/workflows/{}", deploy.id), &viewing)
                .await
                .status,
            StatusCode::OK
        );
        let dispatched = site.dispatch(deploy, &developing).await;
        assert_eq!(dispatched.status, StatusCode::CREATED, "{dispatched:?}");
        assert_eq!(dispatched.body["project_id"], json!(project.id));
    }
}

#[tokio::test]
async fn a_team_binding_gives_its_members_the_role_until_they_leave_the_team() {
    let site = Site::new().await;
    let project = site.project().await;
    let deploy = site.workflow(project.id, "deploy").await;
    let team = bosun_db::teams::create(site.db(), project.organisation_id, "platform", "Platform")
        .await
        .expect("a team")
        .id;
    site.bind(
        "developer",
        Subject::Team(team),
        project.organisation_id,
        None,
    )
    .await;
    let (member, credentials) = site.user("tom").await;
    site.bind(
        "viewer",
        Subject::User(member),
        project.organisation_id,
        Some(project.id),
    )
    .await;
    forbidden(
        &site.dispatch(&deploy, &credentials).await,
        "before joining the team",
    );

    join(&site, team, member).await;
    let dispatched = site.dispatch(&deploy, &credentials).await;
    assert_eq!(dispatched.status, StatusCode::CREATED, "{dispatched:?}");

    assert!(
        bosun_db::teams::remove_member(site.db(), team, member)
            .await
            .expect("a removal")
    );
    forbidden(
        &site.dispatch(&deploy, &credentials).await,
        "after leaving the team",
    );
    assert_eq!(
        site.get(&format!("/api/v1/workflows/{}", deploy.id), &credentials)
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_permission_key_the_server_does_not_know_is_ignored() {
    let site = Site::new().await;
    let project = site.project().await;
    let deploy = site.workflow(project.id, "deploy").await;
    let (user, credentials) = site.user("nova").await;
    for (name, keys, on) in [
        (
            "newer",
            vec!["future.thing", "project.view"],
            Some(project.id),
        ),
        ("newest", vec!["future.other"], None),
    ] {
        let stored = role::ActiveModel {
            id: ActiveValue::Set(Uuid::now_v7()),
            organisation_id: ActiveValue::Set(Some(project.organisation_id.into())),
            name: ActiveValue::Set(name.to_owned()),
            permissions: ActiveValue::Set(keys.into_iter().map(str::to_owned).collect()),
            created_at: ActiveValue::NotSet,
        }
        .insert(site.db())
        .await
        .expect("a role that a newer version wrote");
        bosun_db::roles::bind(
            site.db(),
            NewBinding {
                role_id: RoleId::try_from(stored.id).expect("an id"),
                subject: Subject::User(user),
                organisation_id: project.organisation_id,
                project_id: on,
            },
        )
        .await
        .expect("a binding");
    }

    let me = site.get("/api/v1/me", &credentials).await;
    assert_eq!(me.status, StatusCode::OK, "{me:?}");
    assert_eq!(
        me.body["permissions"],
        json!({
            "installation": [],
            "organisations": {},
            "projects": { project.id.to_string(): ["project.view"] }
        })
    );
    assert_eq!(
        site.get(&format!("/api/v1/workflows/{}", deploy.id), &credentials)
            .await
            .status,
        StatusCode::OK
    );
    forbidden(
        &site.dispatch(&deploy, &credentials).await,
        "with an unknown key and project.view",
    );
}

async fn join(site: &Site, team: TeamId, user: UserId) {
    bosun_db::teams::add_member(site.db(), team, user)
        .await
        .expect("a member");
}

#[tokio::test]
async fn a_viewer_of_one_project_sees_only_it_and_another_project_answers_as_missing() {
    let site = Site::new().await;
    let seen = site.project().await;
    let hidden = site.second_project("hidden").await;
    let hidden_workflow = site.workflow(hidden.id, "deploy").await;
    let (seen_run, _) = site.run().await;
    let hidden_run = site
        .dispatch(&hidden_workflow, &owner_credentials(&site).await)
        .await;
    assert_eq!(hidden_run.status, StatusCode::CREATED, "{hidden_run:?}");
    let hidden_run: RunId = hidden_run.body["id"]
        .as_str()
        .expect("a run id")
        .parse()
        .expect("a run id");
    let (viewer, credentials) = site.user("vera").await;
    site.bind(
        "viewer",
        Subject::User(viewer),
        seen.organisation_id,
        Some(seen.id),
    )
    .await;

    assert_eq!(
        ids(&site.get("/api/v1/projects", &credentials).await),
        BTreeSet::from([seen.id.to_string()])
    );
    assert_eq!(
        ids(&site.get("/api/v1/runs", &credentials).await),
        BTreeSet::from([seen_run.to_string()])
    );
    let everything = owner_credentials(&site).await;
    let newest = site.get("/api/v1/runs?limit=1", &everything).await;
    assert_eq!(ids(&newest), BTreeSet::from([hidden_run.to_string()]));
    let older = site
        .get(
            &format!(
                "/api/v1/runs?limit=1&cursor={}",
                newest.body["next"].as_str().expect("a next page")
            ),
            &everything,
        )
        .await;
    assert_eq!(ids(&older), BTreeSet::from([seen_run.to_string()]));
    assert_eq!(older.body["next"], Value::Null);
    let oldest = site.get("/api/v1/projects?limit=1", &everything).await;
    assert_eq!(ids(&oldest), BTreeSet::from([seen.id.to_string()]));
    let newer = site
        .get(
            &format!(
                "/api/v1/projects?limit=1&cursor={}",
                oldest.body["next"].as_str().expect("a next page")
            ),
            &everything,
        )
        .await;
    assert_eq!(ids(&newer), BTreeSet::from([hidden.id.to_string()]));

    let nothing = RunId::try_from(Uuid::now_v7()).expect("an id");
    for run in [hidden_run, nothing] {
        absent(
            &site.get(&format!("/api/v1/runs/{run}"), &credentials).await,
            &format!("there is no run `{run}`"),
        );
    }
    absent(
        &site
            .get(
                &format!("/api/v1/workflows/{}", hidden_workflow.id),
                &credentials,
            )
            .await,
        &format!("there is no workflow `{}`", hidden_workflow.id),
    );
    absent(
        &site
            .get("/api/v1/projects/globalart/hidden", &credentials)
            .await,
        "there is no project `globalart/hidden`",
    );
    absent(
        &site
            .get("/api/v1/organisations/globalart", &credentials)
            .await,
        "there is no organisation `globalart`",
    );
    let command = site
        .send(
            Method::POST,
            &format!("/api/v1/runs/{hidden_run}/commands"),
            &credentials,
            Some(json!({
                "type": "run.cancel",
                "target": { "attempt": null, "job": null, "step": null },
                "reason": null
            })),
        )
        .await;
    absent(&command, &format!("there is no run `{hidden_run}`"));
    let created = site
        .send(
            Method::POST,
            "/api/v1/projects",
            &credentials,
            Some(json!({ "organisation": "globalart", "slug": "more", "name": "More" })),
        )
        .await;
    assert_eq!(
        created.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{created:?}"
    );
    assert_eq!(
        created.body["error"]["fields"],
        json!([{ "field": "organisation", "message": "there is no organisation `globalart`" }])
    );
    assert_eq!(
        site.get(&format!("/api/v1/runs/{seen_run}"), &credentials)
            .await
            .status,
        StatusCode::OK
    );
}

pub(super) async fn owner_credentials(site: &Site) -> Credentials {
    let login = format!("owner-{}", Uuid::now_v7().simple());
    let user = site.owner(&login).await;
    site.signed(user, &login).await
}

fn fixed(number: u8) -> Uuid {
    Uuid::from_bytes([
        0x01, 0x92, 0xf3, 0xa4, 0x5b, 0x6c, 0x7d, 0x8e, 0x9f, 0x01, 0, 0, 0, 0, 0, number,
    ])
}

#[tokio::test]
async fn me_lists_the_permissions_in_the_shape_of_the_fixture() {
    let site = Site::new().await;
    let db = site.db();
    organisation::ActiveModel {
        id: ActiveValue::Set(fixed(0x01)),
        slug: ActiveValue::Set("acme".to_owned()),
        name: ActiveValue::Set("Acme".to_owned()),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("an organisation");
    for (number, slug) in [(0x02, "ops"), (0x0d, "keel")] {
        project::ActiveModel {
            id: ActiveValue::Set(fixed(number)),
            organisation_id: ActiveValue::Set(fixed(0x01)),
            slug: ActiveValue::Set(slug.to_owned()),
            name: ActiveValue::Set(slug.to_owned()),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("a project");
    }
    let organisation = OrganisationId::try_from(fixed(0x01)).expect("an id");
    let ops = ProjectId::try_from(fixed(0x02)).expect("an id");
    let keel = ProjectId::try_from(fixed(0x0d)).expect("an id");
    let (dana, credentials) = site.user("dana").await;
    site.bind("developer", Subject::User(dana), organisation, None)
        .await;
    let team = bosun_db::teams::create(db, organisation, "platform", "Platform")
        .await
        .expect("a team")
        .id;
    join(&site, team, dana).await;
    site.bind("maintainer", Subject::Team(team), organisation, Some(ops))
        .await;
    let releaser = bosun_db::roles::create(
        db,
        organisation,
        "releaser",
        &BTreeSet::from([Permission::RunCancel, Permission::RunDispatch]),
    )
    .await
    .expect("a custom role")
    .id;
    bosun_db::roles::bind(
        db,
        NewBinding {
            role_id: releaser,
            subject: Subject::User(dana),
            organisation_id: organisation,
            project_id: Some(keel),
        },
    )
    .await
    .expect("a binding");

    let me = site.get("/api/v1/me", &credentials).await;
    assert_eq!(me.status, StatusCode::OK, "{me:?}");
    let fixture: Value =
        serde_json::from_str(&document("contracts/fixtures/api/me.json")).expect("a fixture");
    assert_eq!(me.body["permissions"], fixture["permissions"]);
    assert_eq!(me.body["user"]["login"], fixture["user"]["login"]);
    assert_eq!(me.body["user"]["owner"], fixture["user"]["owner"]);
    let shaped: Permissions =
        serde_json::from_value(me.body["permissions"].clone()).expect("the shape of permissions");
    assert_eq!(
        shaped.on_project(organisation, keel),
        BTreeSet::from([
            Permission::ProjectView,
            Permission::RunDispatch,
            Permission::RunCancel,
            Permission::RunRerun,
            Permission::RunControl,
        ])
    );

    let owner = owner_credentials(&site).await;
    let me = site.get("/api/v1/me", &owner).await;
    assert_eq!(
        me.body["permissions"],
        json!({
            "installation": Permission::ALL,
            "organisations": {},
            "projects": {}
        })
    );
}

#[tokio::test]
async fn neither_a_developer_nor_an_administrator_of_an_organisation_creates_a_pool_or_an_alias() {
    let site = Site::new().await;
    let project = site.project().await;
    for (login, role) in [("dana", "developer"), ("ada", "admin")] {
        let (user, credentials) = site.user(login).await;
        site.bind(role, Subject::User(user), project.organisation_id, None)
            .await;
        let refusal = format!(
            "`{login}` does not hold `agent.manage` on this installation, which only its owners do"
        );
        let pool = site
            .send(
                Method::POST,
                "/api/v1/pools",
                &credentials,
                Some(json!({
                    "name": "linux",
                    "trust": "trusted",
                    "access": { "all_projects": true, "organisations": [], "projects": [] }
                })),
            )
            .await;
        forbidden(&pool, &format!("a pool created by the {role}"));
        assert_eq!(pool.body["error"]["message"], refusal);
        let alias = site
            .send(
                Method::POST,
                "/api/v1/label-aliases",
                &credentials,
                Some(json!({
                    "label": "ubuntu-latest",
                    "organisation": "globalart",
                    "pool": "default",
                    "capability": null,
                    "image": null
                })),
            )
            .await;
        forbidden(&alias, &format!("an alias set by the {role}"));
        assert_eq!(alias.body["error"]["message"], refusal);
    }
    let owner = owner_credentials(&site).await;
    let pools = site.get("/api/v1/pools", &owner).await;
    assert_eq!(
        pools.body["items"].as_array().map(|items| items
            .iter()
            .map(|pool| pool["name"].clone())
            .collect::<Vec<_>>()),
        Some(vec![json!("default")])
    );
    assert_eq!(
        site.get("/api/v1/label-aliases", &owner).await.body["items"],
        json!([])
    );
}
