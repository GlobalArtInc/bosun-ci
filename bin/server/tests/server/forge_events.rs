use std::collections::BTreeMap;

use bosun_testkit::TestDatabase;
use bosun_testkit::forge::{BOT, FakeForge, NO_COMMIT, Sender};
use reqwest::Method;
use serde_json::{Value, json};

use crate::api::{Api, fixture, send, text};
use crate::hooks::{ACCOUNT, PERMISSIONS, delivered, processed};
use crate::message;

const REPOSITORY: &str = "GlobalArtInc/ops";

const PROJECT: &str = "/api/v1/projects/globalartinc/ops";

const ROMAN: Sender<'static> = Sender {
    login: "roman",
    id: 7001,
};

struct Scene {
    database: TestDatabase,
    forge: FakeForge,
    api: Api,
    heads: BTreeMap<String, String>,
}

impl Scene {
    async fn new(files: &[(&str, &str)]) -> Scene {
        let database = TestDatabase::migrated().await;
        let forge = FakeForge::start().await;
        let installation = forge.install(ACCOUNT, PERMISSIONS);
        forge.create_repository(installation, REPOSITORY);
        let mut tree = vec![("README.md".to_owned(), "# ops\n".to_owned())];
        tree.extend(
            files
                .iter()
                .map(|(path, content)| ((*path).to_owned(), (*content).to_owned())),
        );
        let first = forge.commit(REPOSITORY, "main", tree);
        let api = Api::with_forge(&database, &forge).await;
        delivered(
            &api,
            &forge,
            "installation",
            &forge.installation_event(installation, "created"),
        )
        .await;
        processed(&database).await;
        let (status, project) = api
            .post(
                "/api/v1/projects",
                &json!({
                    "organisation": "globalartinc",
                    "slug": "ops",
                    "name": "Operations",
                    "repository": REPOSITORY
                }),
            )
            .await;
        assert_eq!(status, 201, "{project}");
        Scene {
            database,
            forge,
            api,
            heads: BTreeMap::from([("main".to_owned(), first)]),
        }
    }

    async fn push(&mut self, branch: &str, files: &[(&str, &str)]) -> String {
        self.push_as(branch, files, ROMAN).await
    }

    async fn push_as(
        &mut self,
        branch: &str,
        files: &[(&str, &str)],
        sender: Sender<'_>,
    ) -> String {
        let before = self
            .heads
            .get(branch)
            .cloned()
            .unwrap_or_else(|| NO_COMMIT.to_owned());
        let after = self.forge.commit(REPOSITORY, branch, files.iter().copied());
        self.heads.insert(branch.to_owned(), after.clone());
        let payload = self.forge.push_event(
            REPOSITORY,
            &format!("refs/heads/{branch}"),
            &before,
            &after,
            sender,
        );
        self.deliver("push", &payload).await;
        after
    }

    async fn deliver(&self, event: &str, payload: &Value) {
        delivered(&self.api, &self.forge, event, payload).await;
        processed(&self.database).await;
    }

    async fn runs(&self) -> Vec<Value> {
        let (status, page) = self.api.get("/api/v1/runs?limit=500").await;
        assert_eq!(status, 200, "{page}");
        let mut runs = page["items"].as_array().expect("a list of runs").clone();
        runs.reverse();
        runs
    }

    async fn workflow(&self, name: &str) -> Value {
        let (status, page) = self
            .api
            .get(&format!("{PROJECT}/workflows?name={name}"))
            .await;
        assert_eq!(status, 200, "{page}");
        page["items"][0].clone()
    }

    async fn names(&self) -> Vec<String> {
        self.runs()
            .await
            .iter()
            .map(|run| text(&run["display_name"]).to_owned())
            .collect()
    }

    async fn new_names(&self, seen: &mut usize) -> Vec<String> {
        let names = self.names().await;
        let new = names[*seen..].to_vec();
        *seen = names.len();
        new
    }
}

fn workflow(name: &str, on: &str) -> String {
    format!(
        "name: {name}\non:\n{on}jobs:\n  build:\n    runs-on: [self-hosted]\n    steps:\n      \
         - run: make\n"
    )
}

fn planned(lines: &[Value]) -> Vec<(u64, u64, u64)> {
    lines
        .iter()
        .filter(|line| message(line) == "planned the runs of an event")
        .map(|line| {
            (
                line["parsed"].as_u64().expect("a count of parsed files"),
                line["reused"].as_u64().expect("a count of reused files"),
                line["runs"].as_u64().expect("a count of runs"),
            )
        })
        .collect()
}

#[tokio::test]
async fn pushes_run_the_workflows_whose_refs_and_paths_they_match_and_parse_only_changed_files() {
    let src = workflow(
        "src",
        "  push:\n    branches: [main, 'feature/**']\n    paths: ['src/**']\n",
    );
    let docs = workflow(
        "docs",
        "  push:\n    branches: [main, 'feature/**']\n    paths: ['docs/**']\n",
    );
    let tags = workflow("tags", "  push:\n    tags: ['v*']\n");
    let mut scene = Scene::new(&[
        (".bosun/workflows/src.yml", &src),
        (".bosun/workflows/docs.yml", &docs),
        (".bosun/workflows/tags.yml", &tags),
    ])
    .await;
    let mut seen = 0;

    let changed = scene.push("main", &[("src/a.rs", "fn main() {}\n")]).await;
    assert_eq!(scene.new_names(&mut seen).await, ["src"]);

    scene.forge.tag(REPOSITORY, "v1.0.0", &changed);
    let tag = scene
        .forge
        .push_event(REPOSITORY, "refs/tags/v1.0.0", NO_COMMIT, &changed, ROMAN);
    scene.deliver("push", &tag).await;
    assert_eq!(scene.new_names(&mut seen).await, ["tags"]);

    scene
        .push("feature/guide", &[("docs/guide.md", "# guide\n")])
        .await;
    assert_eq!(scene.new_names(&mut seen).await, ["docs"]);

    scene.push("feature/empty", &[]).await;
    assert_eq!(scene.new_names(&mut seen).await, Vec::<String>::new());

    let runs = scene.runs().await;
    let shown: Vec<(&Value, &Value, &Value)> = runs
        .iter()
        .map(|run| (&run["event"], &run["status"], &run["actor"]))
        .collect();
    let actor = json!({ "kind": "forge", "id": ROMAN.id, "login": ROMAN.login });
    assert_eq!(
        shown,
        vec![(&json!("push"), &json!("queued"), &actor); 3],
        "{runs:#?}"
    );

    let Scene { api, .. } = scene;
    assert_eq!(
        planned(&api.stop().await),
        [(3, 0, 1), (0, 3, 1), (0, 3, 1), (0, 3, 0)]
    );
}

#[tokio::test]
async fn the_bot_of_the_github_app_starts_no_run_with_a_push_and_one_with_a_repository_dispatch() {
    let on_push = workflow("on push", "  push:\n");
    let on_dispatch = workflow(
        "on dispatch",
        "  repository_dispatch:\n    types: [deploy]\n",
    );
    let mut scene = Scene::new(&[
        (".bosun/workflows/push.yml", &on_push),
        (".bosun/workflows/dispatch.yml", &on_dispatch),
    ])
    .await;
    let mut seen = 0;
    scene
        .push_as("main", &[("src/a.rs", "fn main() {}\n")], BOT)
        .await;
    assert_eq!(scene.new_names(&mut seen).await, Vec::<String>::new());

    for event_type in ["deploy", "release"] {
        let dispatch = scene.forge.event(
            REPOSITORY,
            Some(event_type),
            json!({ "branch": "main", "client_payload": { "unit": false } }),
            BOT,
        );
        scene.deliver("repository_dispatch", &dispatch).await;
    }
    assert_eq!(scene.new_names(&mut seen).await, ["on dispatch"]);
    let runs = scene.runs().await;
    assert_eq!(runs[0]["event"], "repository_dispatch");
    assert_eq!(
        runs[0]["actor"],
        json!({ "kind": "forge", "id": BOT.id, "login": BOT.login })
    );

    scene.push("main", &[("src/b.rs", "fn b() {}\n")]).await;
    assert_eq!(scene.new_names(&mut seen).await, ["on push"]);
}

#[tokio::test]
async fn a_broken_file_whose_on_matches_ends_its_run_as_a_startup_failure_with_its_diagnostics() {
    let broken_on = "on:\n  push:\n    branchez: [main]\njobs:\n  build:\n    runs-on: shell\n    \
                     steps:\n      - run: make\n";
    let mut scene = Scene::new(&[(".bosun/workflows/broken-on.yml", broken_on)]).await;
    let mut seen = 0;

    scene
        .push(
            "main",
            &[(
                ".bosun/workflows/ci.yml",
                fixture("workflows/runs-on-steps-context.yml").as_str(),
            )],
        )
        .await;
    assert_eq!(
        scene.new_names(&mut seen).await,
        [".bosun/workflows/ci.yml"]
    );
    let run = scene.runs().await.pop().expect("the run of the broken job");
    let (status, shown) = scene
        .api
        .get(&format!("/api/v1/runs/{}", text(&run["id"])))
        .await;
    assert_eq!(status, 200, "{shown}");
    let expected: Value =
        serde_json::from_str(&fixture("api/run-startup-failure.json")).expect("a run");
    for field in [
        "display_name",
        "event",
        "ref",
        "status",
        "conclusion",
        "actor",
        "notices",
    ] {
        assert_eq!(shown[field], expected[field], "{field}: {shown}");
    }
    let (status, jobs) = scene
        .api
        .get(&format!(
            "/api/v1/runs/{}/attempts/1/jobs",
            text(&run["id"])
        ))
        .await;
    assert_eq!(status, 200, "{jobs}");
    assert_eq!(jobs["items"], json!([]));

    scene
        .push(
            "main",
            &[(".bosun/workflows/broken-on.yml", &format!("{broken_on}\n"))],
        )
        .await;
    let names = scene.new_names(&mut seen).await;
    assert_eq!(
        names,
        [".bosun/workflows/broken-on.yml", ".bosun/workflows/ci.yml"]
    );
    let runs = scene.runs().await;
    let broken = runs
        .iter()
        .find(|run| run["display_name"] == ".bosun/workflows/broken-on.yml")
        .expect("the run of the broken on");
    assert_eq!(broken["conclusion"], "startup_failure");
    let codes: Vec<&Value> = broken["notices"]
        .as_array()
        .expect("notices")
        .iter()
        .map(|notice| &notice["code"])
        .collect();
    assert_eq!(codes, [&json!("unknown_key")]);

    scene.push("main", &[("src/a.rs", "fn main() {}\n")]).await;
    assert_eq!(
        scene.new_names(&mut seen).await,
        [".bosun/workflows/ci.yml"]
    );

    scene
        .push(
            "main",
            &[(
                ".bosun/workflows/uses.yml",
                "name: uses\non: push\njobs:\n  build:\n    runs-on: shell\n    steps:\n      \
                 - uses: actions/checkout@v4\n",
            )],
        )
        .await;
    let runs = scene.runs().await;
    let uses = runs
        .iter()
        .find(|run| run["display_name"] == "uses")
        .expect("the run of the workflow with a `uses` step");
    assert_eq!(uses["conclusion"], "startup_failure");
    assert_eq!(uses["notices"][0]["code"], "not_implemented");
    assert_eq!(
        uses["notices"][0]["path"],
        json!(["jobs", "build", "steps", 0, "uses"])
    );
}

#[tokio::test]
async fn files_under_github_workflows_run_unless_bosun_workflows_has_their_name() {
    let extension = fixture("workflows/bosun-context-in-github-workflows.yml")
        .replace("on: workflow_dispatch", "on: push");
    let mut scene = Scene::new(&[
        (
            ".bosun/workflows/both.yml",
            &workflow("bosun both", "  push:\n"),
        ),
        (
            ".github/workflows/both.yml",
            &workflow("github both", "  push:\n"),
        ),
        (".github/workflows/ci.yml", &extension),
    ])
    .await;
    let mut seen = 0;
    scene.push("main", &[("src/a.rs", "fn main() {}\n")]).await;
    assert_eq!(scene.new_names(&mut seen).await, ["bosun both"]);

    let (status, settings) = send(
        scene
            .api
            .request(Method::PATCH, &format!("{PROJECT}/settings"))
            .header("if-match", "\"1\"")
            .json(&json!({ "github_workflows": true })),
    )
    .await;
    assert_eq!(status, 200, "{settings}");
    scene.push("main", &[("src/b.rs", "fn b() {}\n")]).await;
    assert_eq!(
        scene.new_names(&mut seen).await,
        ["bosun both", ".github/workflows/ci.yml"]
    );
    let runs = scene.runs().await;
    let warning: Value = serde_json::from_str(&fixture(
        "diagnostics/bosun-context-in-github-workflows.json",
    ))
    .expect("a diagnostic");
    let mut expected = json!({ "job_id": null });
    for (key, value) in warning.as_object().expect("a diagnostic") {
        expected[key] = value.clone();
    }
    assert_eq!(runs[2]["notices"], json!([expected]));
    assert_eq!(runs[2]["status"], "queued");
}

#[tokio::test]
async fn other_events_match_their_types_against_the_action_and_a_disabled_workflow_starts_nothing()
{
    let issues = workflow("issues", "  issues:\n    types: [opened]\n");
    let mut scene = Scene::new(&[
        (".bosun/workflows/issues.yml", &issues),
        (".bosun/workflows/push.yml", &workflow("push", "  push:\n")),
    ])
    .await;
    let mut seen = 0;
    for action in ["opened", "closed"] {
        let event = scene.forge.event(
            REPOSITORY,
            Some(action),
            json!({ "issue": { "number": 1, "title": "Flaky test" } }),
            ROMAN,
        );
        scene.deliver("issues", &event).await;
    }
    assert_eq!(scene.new_names(&mut seen).await, ["issues"]);

    scene.push("main", &[("src/a.rs", "fn main() {}\n")]).await;
    assert_eq!(scene.new_names(&mut seen).await, ["push"]);
    let push = scene.workflow(".bosun/workflows/push.yml").await;
    let (status, disabled) = scene
        .api
        .patch(
            &format!("/api/v1/workflows/{}", text(&push["id"])),
            &json!({ "enabled": false }),
        )
        .await;
    assert_eq!(status, 200, "{disabled}");
    assert_eq!(disabled["enabled"], false);
    scene.push("main", &[("src/b.rs", "fn b() {}\n")]).await;
    assert_eq!(scene.new_names(&mut seen).await, Vec::<String>::new());
}

#[tokio::test]
async fn a_workflow_carries_its_newest_run_of_the_default_branch_and_not_a_newer_run_of_another() {
    let ci = workflow("ci", "  push:\n");
    let mut scene = Scene::new(&[(".bosun/workflows/ci.yml", &ci)]).await;
    let mut seen = 0;
    let first = scene.push("main", &[("src/a.rs", "fn a() {}\n")]).await;
    let second = scene.push("main", &[("src/b.rs", "fn b() {}\n")]).await;
    scene
        .push("feature/guide", &[("docs/guide.md", "# guide\n")])
        .await;
    assert_eq!(scene.new_names(&mut seen).await, ["ci", "ci", "ci"]);
    let runs = scene.runs().await;
    let shown: Vec<(&Value, &Value)> = runs.iter().map(|run| (&run["ref"], &run["sha"])).collect();
    assert_eq!(
        shown,
        [
            (&json!("refs/heads/main"), &json!(first)),
            (&json!("refs/heads/main"), &json!(second)),
            (&json!("refs/heads/feature/guide"), &runs[2]["sha"]),
        ]
    );

    let listed = scene.workflow(".bosun/workflows/ci.yml").await;
    assert_eq!(listed["latest_run"], runs[1], "{listed}");
    let (status, shown) = scene
        .api
        .get(&format!("/api/v1/workflows/{}", text(&listed["id"])))
        .await;
    assert_eq!(status, 200, "{shown}");
    assert_eq!(shown, listed);

    let (status, newest) = scene
        .api
        .get(&format!(
            "/api/v1/runs?workflow={}&limit=1",
            text(&listed["id"])
        ))
        .await;
    assert_eq!(status, 200, "{newest}");
    assert_eq!(newest["items"], json!([runs[2]]));
    let (status, on_main) = scene
        .api
        .get(&format!(
            "/api/v1/runs?project=globalartinc/ops&branch=main&event=push&actor={}",
            ROMAN.login
        ))
        .await;
    assert_eq!(status, 200, "{on_main}");
    assert_eq!(on_main["items"], json!([runs[1], runs[0]]));
}

#[tokio::test]
async fn an_event_sent_through_the_api_runs_the_repository_dispatch_workflows_of_the_default_branch()
 {
    let on_dispatch = "name: deploy\non:\n  repository_dispatch:\n    types: [deploy]\njobs:\n  \
                       build:\n    runs-on: [self-hosted]\n    steps:\n      \
                       - run: echo ${{ github.event.client_payload.environment }}\n";
    let scene = Scene::new(&[(".bosun/workflows/deploy.yml", on_dispatch)]).await;
    let (status, planned) = scene
        .api
        .post(
            &format!("{PROJECT}/events"),
            &json!({ "event_type": "deploy", "client_payload": { "environment": "prod1" } }),
        )
        .await;
    assert_eq!(status, 201, "{planned}");
    let runs = planned["runs"].as_array().expect("the runs");
    assert_eq!(runs.len(), 1, "{planned}");
    assert_eq!(runs[0]["display_name"], "deploy");
    assert_eq!(runs[0]["event"], "repository_dispatch");
    assert_eq!(runs[0]["actor"]["kind"], "token");

    let (status, planned) = scene
        .api
        .post(
            &format!("{PROJECT}/events"),
            &json!({ "event_type": "rollback" }),
        )
        .await;
    assert_eq!(status, 201, "{planned}");
    assert_eq!(planned["runs"], json!([]));

    let crowded: serde_json::Map<String, Value> =
        (0..11).map(|n| (n.to_string(), json!(n))).collect();
    let (status, refused) = scene
        .api
        .post(
            &format!("{PROJECT}/events"),
            &json!({ "event_type": "", "client_payload": crowded }),
        )
        .await;
    assert_eq!(status, 422, "{refused}");
    let fields: Vec<&Value> = refused["error"]["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .map(|field| &field["field"])
        .collect();
    assert_eq!(fields, [&json!("event_type"), &json!("client_payload")]);
}

#[tokio::test]
async fn the_github_context_of_a_run_from_the_forge_holds_its_repository_ref_and_commit() {
    let named = "name: context\nrun-name: '${{ github.repository }} ${{ github.ref }} ${{ github.sha \
                 }} ${{ github.actor }} ${{ github.actor_id }} ${{ github.server_url }} ${{ \
                 github.graphql_url }}'\non: [push, issues]\njobs:\n  build:\n    runs-on: \
                 [self-hosted]\n    steps:\n      - run: make\n";
    let mut scene = Scene::new(&[(".bosun/workflows/context.yml", named)]).await;
    let mut server = scene.forge.api_url();
    server.set_path("");
    let server = server.as_str().trim_end_matches('/').to_owned();
    let pushed = scene
        .push("feature/context", &[("src/a.rs", "fn main() {}\n")])
        .await;
    let issue = scene.forge.event(
        REPOSITORY,
        Some("opened"),
        json!({ "issue": { "number": 2, "title": "Context" } }),
        ROMAN,
    );
    scene.deliver("issues", &issue).await;
    let deleted = scene.forge.push_event(
        REPOSITORY,
        "refs/heads/feature/context",
        &pushed,
        NO_COMMIT,
        ROMAN,
    );
    scene.deliver("push", &deleted).await;
    let main = scene.heads["main"].clone();
    let on_main =
        format!("{REPOSITORY} refs/heads/main {main} roman 7001 {server} {server}/api/graphql");
    assert_eq!(
        scene.names().await,
        [
            format!(
                "{REPOSITORY} refs/heads/feature/context {pushed} roman 7001 {server} \
                 {server}/api/graphql"
            ),
            on_main.clone(),
            on_main,
        ]
    );
}

#[tokio::test]
async fn repository_workflows_are_named_by_their_paths_follow_the_default_branch_and_are_not_saved()
{
    let mut scene = Scene::new(&[(".bosun/workflows/ci.yml", &workflow("ci", "  push:\n"))]).await;
    scene.push("main", &[("src/a.rs", "fn main() {}\n")]).await;
    scene
        .push(
            "feature/lint",
            &[(".bosun/workflows/lint.yml", &workflow("lint", "  push:\n"))],
        )
        .await;
    let (status, page) = scene.api.get(&format!("{PROJECT}/workflows")).await;
    assert_eq!(status, 200, "{page}");
    let listed: Vec<(&Value, &Value, bool)> = page["items"]
        .as_array()
        .expect("the workflows")
        .iter()
        .map(|workflow| {
            (
                &workflow["name"],
                &workflow["source"],
                workflow["revision_id"].is_string(),
            )
        })
        .collect();
    let repository = json!({ "kind": "repository" });
    assert_eq!(
        listed,
        [
            (&json!(".bosun/workflows/ci.yml"), &repository, true),
            (&json!(".bosun/workflows/lint.yml"), &repository, false),
        ]
    );
    let ci = text(&page["items"][0]["id"]).to_owned();
    let (status, revisions) = scene
        .api
        .get(&format!("/api/v1/workflows/{ci}/revisions"))
        .await;
    assert_eq!(status, 200, "{revisions}");
    assert_eq!(
        revisions["items"][0]["source"],
        json!(workflow("ci", "  push:\n"))
    );

    let (status, refused) = scene
        .api
        .put(
            &format!("/api/v1/workflows/{ci}/source"),
            &json!({ "source": workflow("ci", "  workflow_dispatch:\n") }),
        )
        .await;
    assert_eq!(status, 409, "{refused}");
    assert_eq!(
        refused["error"]["message"],
        "workflow `.bosun/workflows/ci.yml` is a file of the repository, whose source changes \
         with its commits"
    );
    let (status, refused) = scene
        .api
        .post(
            &format!("/api/v1/workflows/{ci}/dispatch"),
            &json!({ "ref": null, "inputs": {} }),
        )
        .await;
    assert_eq!(status, 409, "{refused}");
    assert_eq!(
        refused["error"]["message"],
        "workflow `.bosun/workflows/ci.yml` is a file of the repository, which a dispatch runs \
         from M1 step 30"
    );
}
