use std::collections::BTreeSet;
use std::time::Duration;

use bosun_proto::agent::v1::agent_message::Message as Report;
use bosun_proto::agent::v1::{Accept, Capability, Conclusion};
use bosun_testkit::TestDatabase;
use bosun_testkit::agent::{Connection, FakeAgent};
use bosun_testkit::forge::{FakeForge, PULL_REQUEST_FILES, RecordedStatus, Sender};
use reqwest::Method;
use sea_orm::ConnectionTrait;
use serde_json::{Value, json};
use tokio::time::{Instant, sleep, timeout};

use crate::api::{Api, send, text};
use crate::hooks::{ACCOUNT, delivered, processed};
use crate::{WAIT, strings};

const REPOSITORY: &str = "GlobalArtInc/ops";

const PROJECT: &str = "/api/v1/projects/globalartinc/ops";

const PERMISSIONS: &[(&str, &str)] = &[
    ("checks", "write"),
    ("contents", "read"),
    ("metadata", "read"),
    ("pull_requests", "read"),
    ("statuses", "write"),
];

const QUIET: Duration = Duration::from_millis(500);

const ROMAN: Sender<'static> = Sender {
    login: "roman",
    id: 7001,
};

const MALLORY: Sender<'static> = Sender {
    login: "mallory",
    id: 9001,
};

struct Scene {
    database: TestDatabase,
    forge: FakeForge,
    api: Api,
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
        forge.commit(REPOSITORY, "main", tree);
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
        }
    }

    async fn fork_policy(&self, policy: &str) {
        let (status, settings) = send(
            self.api
                .request(Method::PATCH, &format!("{PROJECT}/settings"))
                .header("if-match", "\"1\"")
                .json(&json!({ "fork_policy": policy })),
        )
        .await;
        assert_eq!(status, 200, "{settings}");
    }

    async fn deliver(&self, event: &str, payload: &Value) {
        delivered(&self.api, &self.forge, event, payload).await;
        processed(&self.database).await;
    }

    async fn pull_request(&self, number: u64, action: &str, sender: Sender<'_>) {
        let payload = self
            .forge
            .pull_request_event(REPOSITORY, number, action, sender);
        self.deliver("pull_request", &payload).await;
    }

    async fn failed_once(&self) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let failed = strings(
                self.database.connection(),
                "SELECT last_error AS value FROM bosun.webhook_delivery \
                 WHERE processed_at IS NULL AND last_error IS NOT NULL",
            )
            .await;
            if let [error] = failed.as_slice() {
                return error.clone();
            }
            assert!(
                Instant::now() < deadline,
                "no delivery failed within {WAIT:?}: {failed:?}"
            );
            sleep(Duration::from_millis(50)).await;
        }
    }

    async fn summary(&self) -> Vec<(String, String, String, String)> {
        self.runs()
            .await
            .iter()
            .map(|run| {
                (
                    text(&run["display_name"]).to_owned(),
                    text(&run["event"]).to_owned(),
                    text(&run["ref"]).to_owned(),
                    text(&run["sha"]).to_owned(),
                )
            })
            .collect()
    }

    async fn runs(&self) -> Vec<Value> {
        let (status, page) = self.api.get("/api/v1/runs?limit=500").await;
        assert_eq!(status, 200, "{page}");
        let mut runs = page["items"].as_array().expect("a list of runs").clone();
        runs.reverse();
        runs
    }

    async fn named(&self, name: &str) -> Vec<Value> {
        self.runs()
            .await
            .into_iter()
            .filter(|run| run["display_name"] == name)
            .collect()
    }

    async fn jobs(&self, run: &Value) -> Vec<Value> {
        let (status, page) = self
            .api
            .get(&format!(
                "/api/v1/runs/{}/attempts/1/jobs",
                text(&run["id"])
            ))
            .await;
        assert_eq!(status, 200, "{page}");
        page["items"].as_array().expect("a list of jobs").clone()
    }

    async fn reported(&self) {
        let deadline = Instant::now() + WAIT;
        loop {
            let pending = bosun_db::outbox::pending(self.database.connection())
                .await
                .expect("the outbox backlog");
            if pending == 0 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "{pending} outbox rows were not delivered within {WAIT:?}"
            );
            sleep(Duration::from_millis(50)).await;
        }
    }

    async fn agent(&self, pool: &str, name: &str, capabilities: &[Capability]) -> Connection {
        let (status, token) = self
            .api
            .post(
                "/api/v1/registration-tokens",
                &json!({ "pool": pool, "reusable": false, "expires_at": null }),
            )
            .await;
        assert_eq!(status, 201, "{token}");
        let connection =
            FakeAgent::register(self.api.address, text(&token["token"]), name, &["build"])
                .await
                .with_capabilities(capabilities)
                .connect(self.api.address)
                .await;
        connection.idle();
        connection
    }
}

fn workflow(name: &str, on: &str) -> String {
    format!(
        "name: {name}\non:\n{on}jobs:\n  build:\n    runs-on: [self-hosted]\n    steps:\n      \
         - run: make\n"
    )
}

async fn offered_nothing(connection: &mut Connection, what: &str) {
    assert!(
        timeout(QUIET, connection.assignment()).await.is_err(),
        "{what} was offered a job"
    );
}

#[tokio::test]
async fn a_fork_pull_request_under_the_default_policy_creates_no_run_and_gets_a_status_saying_why()
{
    let scene = Scene::new(&[
        (
            ".bosun/workflows/ci.yml",
            &workflow("ci", "  pull_request:\n"),
        ),
        (".bosun/workflows/push.yml", &workflow("push", "  push:\n")),
    ])
    .await;
    let head = scene.forge.commit(
        REPOSITORY,
        "patch",
        [
            ("src/a.rs", "fn main() {}\n"),
            (
                ".bosun/workflows/fork.yml",
                workflow("fork", "  pull_request:\n").as_str(),
            ),
        ],
    );
    let fork = scene
        .forge
        .open_fork_pull_request(REPOSITORY, MALLORY.login, "main", "patch");
    scene.pull_request(fork, "opened", MALLORY).await;
    scene.pull_request(fork, "labeled", MALLORY).await;
    scene.reported().await;
    assert_eq!(scene.runs().await, Vec::<Value>::new());
    assert_eq!(
        scene.forge.statuses(),
        [RecordedStatus {
            repository: REPOSITORY.to_owned(),
            sha: head,
            context: "bosun/fork-policy".to_owned(),
            state: "error".to_owned(),
            description: Some(
                "Bosun runs no workflow for pull requests from forks in this project".to_owned()
            ),
            target_url: None,
        }]
    );
    let (status, workflows) = scene.api.get(&format!("{PROJECT}/workflows")).await;
    assert_eq!(status, 200, "{workflows}");
    let names: Vec<&Value> = workflows["items"]
        .as_array()
        .expect("the workflows")
        .iter()
        .map(|workflow| &workflow["name"])
        .collect();
    assert_eq!(
        names,
        [
            &json!(".bosun/workflows/ci.yml"),
            &json!(".bosun/workflows/push.yml")
        ]
    );

    scene
        .forge
        .commit(REPOSITORY, "own", [("src/b.rs", "fn b() {}\n")]);
    let own = scene.forge.open_pull_request(REPOSITORY, "main", "own");
    scene.pull_request(own, "opened", ROMAN).await;
    let runs = scene.runs().await;
    assert_eq!(runs.len(), 1, "{runs:#?}");
    assert_eq!(runs[0]["display_name"], "ci");
    assert_eq!(runs[0]["ref"], format!("refs/pull/{own}/merge"));
    assert_eq!(runs[0]["status"], "queued");
}

#[tokio::test]
async fn under_approval_a_fork_waits_for_admission_and_then_runs_on_an_untrusted_pool_alone() {
    let scene = Scene::new(&[(
        ".bosun/workflows/ci.yml",
        "name: ci\non: pull_request\njobs:\n  build:\n    runs-on: [self-hosted, build]\n    \
         env:\n      TOKEN: ${{ secrets.DEPLOY_TOKEN }}\n    steps:\n      \
         - run: echo ${{ vars.GREETING }}\n",
    )])
    .await;
    scene.fork_policy("approval").await;
    for (collection, name, value) in [
        ("secrets", "DEPLOY_TOKEN", "Zx9TheDeployTokenOfOps"),
        ("variables", "GREETING", "hello"),
    ] {
        let (status, body) = scene
            .api
            .post(
                &format!("/api/v1/{collection}?project=globalartinc/ops"),
                &json!({ "name": name, "value": value }),
            )
            .await;
        assert_eq!(status, 201, "{body}");
    }
    let everyone = json!({ "all_projects": true, "organisations": [], "projects": [] });
    for (name, trust) in [("inside", "trusted"), ("forks", "untrusted")] {
        let (status, pool) = scene
            .api
            .post(
                "/api/v1/pools",
                &json!({ "name": name, "trust": trust, "access": everyone }),
            )
            .await;
        assert_eq!(status, 201, "{pool}");
    }
    let mut inside = scene
        .agent("inside", "runner1-1", &[Capability::Shell])
        .await;
    let mut outside = scene
        .agent("forks", "runner2-1", &[Capability::Podman])
        .await;

    let head = scene
        .forge
        .commit(REPOSITORY, "patch", [("src/a.rs", "fn main() {}\n")]);
    let number = scene
        .forge
        .open_fork_pull_request(REPOSITORY, MALLORY.login, "main", "patch");
    scene.pull_request(number, "opened", MALLORY).await;
    let runs = scene.runs().await;
    let [run] = runs.as_slice() else {
        panic!("not one run: {runs:#?}");
    };
    assert_eq!(run["status"], "waiting");
    let jobs = scene.jobs(run).await;
    assert_eq!(
        (&jobs[0]["status"], &jobs[0]["waiting_reason"]),
        (&json!("waiting"), &json!("untrusted"))
    );
    offered_nothing(
        &mut outside,
        "the agent of the untrusted pool before the admission",
    )
    .await;
    offered_nothing(&mut inside, "the agent of the trusted pool").await;
    let id = text(&run["id"]).to_owned();
    assert_eq!(
        strings(
            scene.database.connection(),
            &format!("SELECT trusted::text AS value FROM bosun.run WHERE id = '{id}'"),
        )
        .await,
        ["false"]
    );

    let (status, accepted) = scene
        .api
        .post(
            &format!("/api/v1/runs/{id}/commands"),
            &serde_json::from_str::<Value>(&crate::api::fixture("api/command-run-admit.json"))
                .expect("a command"),
        )
        .await;
    assert_eq!(status, 201, "{accepted}");
    assert_eq!(accepted["kind"], "command.accepted");

    let (assignment, documents) = outside.job().await;
    assert_eq!(assignment.job_id, text(&jobs[0]["id"]));
    let token = text(&documents.contexts.github["token"]).to_owned();
    assert_eq!(
        Value::Object(documents.contexts.secrets.clone()),
        json!({ "GITHUB_TOKEN": token })
    );
    let minted = scene
        .forge
        .tokens()
        .into_iter()
        .find(|minted| minted.token == token)
        .expect("the run from a fork has a forge token");
    assert!(
        minted.permissions.values().all(|level| level == "read"),
        "{:?}",
        minted.permissions
    );
    assert_eq!(documents.contexts.vars["GREETING"], "hello");
    assert_eq!(documents.evaluated.env["TOKEN"], "");
    assert_eq!(
        documents.contexts.github["sha"],
        scene.forge.merge_commit(REPOSITORY, number)
    );
    assert_eq!(
        documents.contexts.github["ref"],
        format!("refs/pull/{number}/merge")
    );
    assert_eq!(documents.contexts.github["head_ref"], "patch");
    outside.send(Report::Accept(Accept {
        execution_id: assignment.execution_id.clone(),
    }));
    outside.finish_job(&assignment.execution_id, Conclusion::Success, &[]);
    inside.idle();
    offered_nothing(
        &mut inside,
        "the agent of the trusted pool after the admission",
    )
    .await;

    let deadline = Instant::now() + WAIT;
    loop {
        let (_, shown) = scene.api.get(&format!("/api/v1/runs/{id}")).await;
        if shown["status"] == "completed" {
            assert_eq!(shown["conclusion"], "success", "{shown}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the run did not complete: {shown}"
        );
        sleep(Duration::from_millis(50)).await;
    }
    scene.reported().await;
    let check_runs = scene.forge.check_runs();
    assert!(
        !check_runs.is_empty()
            && check_runs
                .iter()
                .all(|check_run| check_run.head_sha == head),
        "{check_runs:#?}"
    );
}

#[tokio::test]
async fn pull_requests_filter_paths_by_their_files_and_pull_request_target_runs_the_base_file() {
    let scene = Scene::new(&[
        (
            ".bosun/workflows/src.yml",
            &workflow("src", "  pull_request:\n    paths: ['src/**']\n"),
        ),
        (
            ".bosun/workflows/target.yml",
            &workflow("target from main", "  pull_request_target:\n"),
        ),
        (
            ".bosun/workflows/review.yml",
            &workflow("review", "  pull_request_review:\n    types: [submitted]\n"),
        ),
    ])
    .await;
    let main = scene
        .forge
        .commit(REPOSITORY, "main", [("CHANGES.md", "1\n")]);
    let docs_head = scene.forge.commit(
        REPOSITORY,
        "docs",
        [
            ("docs/guide.md", "# guide\n"),
            (
                ".bosun/workflows/target.yml",
                workflow("target from the head", "  pull_request_target:\n").as_str(),
            ),
        ],
    );
    let docs = scene.forge.open_pull_request(REPOSITORY, "main", "docs");
    scene.pull_request(docs, "opened", ROMAN).await;
    let target = scene
        .forge
        .pull_request_event(REPOSITORY, docs, "opened", ROMAN);
    scene.deliver("pull_request_target", &target).await;
    let runs = scene.runs().await;
    let shown: Vec<(&Value, &Value, &Value, &Value)> = runs
        .iter()
        .map(|run| {
            (
                &run["display_name"],
                &run["event"],
                &run["ref"],
                &run["sha"],
            )
        })
        .collect();
    assert_eq!(
        shown,
        [(
            &json!("target from main"),
            &json!("pull_request_target"),
            &json!("refs/heads/main"),
            &json!(main)
        )]
    );

    let code_head = scene
        .forge
        .commit(REPOSITORY, "code", [("src/a.rs", "fn main() {}\n")]);
    let code = scene.forge.open_pull_request(REPOSITORY, "main", "code");
    scene.pull_request(code, "opened", ROMAN).await;
    scene.pull_request(code, "closed", ROMAN).await;
    let src = scene.named("src").await;
    let [src] = src.as_slice() else {
        panic!("not one run of src: {src:#?}");
    };
    assert_eq!(src["ref"], format!("refs/pull/{code}/merge"));
    assert_eq!(src["sha"], scene.forge.merge_commit(REPOSITORY, code));

    let mut review = scene
        .forge
        .pull_request_event(REPOSITORY, code, "submitted", ROMAN);
    review["review"] = json!({ "id": 80, "state": "approved", "body": "Ship it" });
    scene.deliver("pull_request_review", &review).await;
    let reviewed = scene.named("review").await;
    let [reviewed] = reviewed.as_slice() else {
        panic!("not one run of review: {reviewed:#?}");
    };
    assert_eq!(reviewed["event"], "pull_request_review");
    assert_eq!(reviewed["ref"], format!("refs/pull/{code}/merge"));

    scene.reported().await;
    let heads: BTreeSet<String> = scene
        .forge
        .check_runs()
        .into_iter()
        .map(|check_run| check_run.head_sha)
        .collect();
    assert_eq!(heads, BTreeSet::from([docs_head, code_head]));
}

#[tokio::test]
async fn a_merge_group_runs_the_workflows_whose_branches_match_the_base_ref_of_its_queue() {
    let scene = Scene::new(&[(
        ".bosun/workflows/queue.yml",
        &workflow("queue", "  merge_group:\n    branches: [main]\n"),
    )])
    .await;
    for base in ["main", "release"] {
        let branch =
            format!("gh-readonly-queue/{base}/pr-7-929f8209d40f77f4abc622a499c93a83babdbe64");
        let head = scene
            .forge
            .commit(REPOSITORY, &branch, [("src/a.rs", "fn main() {}\n")]);
        let event = scene.forge.event(
            REPOSITORY,
            Some("checks_requested"),
            json!({
                "merge_group": {
                    "head_sha": head,
                    "head_ref": format!("refs/heads/{branch}"),
                    "base_sha": head,
                    "base_ref": format!("refs/heads/{base}"),
                }
            }),
            ROMAN,
        );
        scene.deliver("merge_group", &event).await;
    }
    let runs = scene.runs().await;
    let [run] = runs.as_slice() else {
        panic!("not one run: {runs:#?}");
    };
    assert_eq!(run["event"], "merge_group");
    assert_eq!(
        run["ref"],
        "refs/heads/gh-readonly-queue/main/pr-7-929f8209d40f77f4abc622a499c93a83babdbe64"
    );
    let jobs = scene.jobs(run).await;
    assert_eq!(jobs[0]["status"], "queued");
}

#[tokio::test]
async fn a_merged_pull_request_runs_on_its_base_branch_at_its_merge_commit() {
    let scene = Scene::new(&[(
        ".bosun/workflows/merged.yml",
        &workflow("merged", "  pull_request:\n    types: [closed]\n"),
    )])
    .await;
    scene
        .forge
        .commit(REPOSITORY, "code", [("src/a.rs", "fn main() {}\n")]);
    let number = scene.forge.open_pull_request(REPOSITORY, "main", "code");
    let merge = scene
        .forge
        .commit(REPOSITORY, "main", [("src/a.rs", "fn main() {}\n")]);
    let mut closed = scene
        .forge
        .pull_request_event(REPOSITORY, number, "closed", ROMAN);
    closed["pull_request"]["state"] = json!("closed");
    closed["pull_request"]["merged"] = json!(true);
    closed["pull_request"]["merge_commit_sha"] = json!(merge);
    scene.deliver("pull_request", &closed).await;
    assert_eq!(
        scene.summary().await,
        [(
            "merged".to_owned(),
            "pull_request".to_owned(),
            "refs/heads/main".to_owned(),
            merge.clone()
        )]
    );
    assert_ne!(scene.forge.merge_commit(REPOSITORY, number), merge);
}

#[tokio::test]
async fn a_pull_request_the_forge_made_no_merge_commit_of_runs_only_pull_request_target() {
    let scene = Scene::new(&[
        (
            ".bosun/workflows/ci.yml",
            &workflow("ci", "  pull_request:\n"),
        ),
        (
            ".bosun/workflows/target.yml",
            &workflow("target", "  pull_request_target:\n"),
        ),
    ])
    .await;
    let main = scene
        .forge
        .commit(REPOSITORY, "main", [("CHANGES.md", "1\n")]);
    scene.forge.hold_merges(REPOSITORY, true);
    scene
        .forge
        .commit(REPOSITORY, "code", [("src/a.rs", "fn main() {}\n")]);
    let number = scene.forge.open_pull_request(REPOSITORY, "main", "code");
    scene.pull_request(number, "opened", ROMAN).await;
    assert_eq!(
        scene.summary().await,
        [(
            "target".to_owned(),
            "pull_request_target".to_owned(),
            "refs/heads/main".to_owned(),
            main
        )]
    );
}

#[tokio::test]
async fn a_pull_request_runs_at_the_merge_of_its_new_head_and_never_at_a_stale_one() {
    let scene = Scene::new(&[
        (
            ".bosun/workflows/ci.yml",
            &workflow("ci", "  pull_request:\n"),
        ),
        (
            ".bosun/workflows/target.yml",
            &workflow("target", "  pull_request_target:\n"),
        ),
    ])
    .await;
    scene
        .forge
        .commit(REPOSITORY, "code", [("src/a.rs", "1\n")]);
    let number = scene.forge.open_pull_request(REPOSITORY, "main", "code");
    scene.pull_request(number, "opened", ROMAN).await;
    let opened = scene.forge.merge_commit(REPOSITORY, number);

    scene.forge.hold_merges(REPOSITORY, true);
    let head = scene
        .forge
        .commit(REPOSITORY, "code", [("src/a.rs", "2\n")]);
    let pushed = scene
        .forge
        .pull_request_event(REPOSITORY, number, "synchronize", ROMAN);
    assert_eq!(pushed["pull_request"]["head"]["sha"], head);
    delivered(&scene.api, &scene.forge, "pull_request", &pushed).await;
    let error = scene.failed_once().await;
    assert!(
        error.contains(&format!(
            "the merge ref of pull request #{number} names {opened}, which the forge has not \
             made of its head {head} yet"
        )),
        "{error}"
    );
    assert_eq!(scene.runs().await.len(), 2);
    scene.forge.hold_merges(REPOSITORY, false);
    processed(&scene.database).await;
    let synchronized = scene.forge.merge_commit(REPOSITORY, number);
    assert_ne!(synchronized, opened);
    let ci: Vec<Value> = scene
        .named("ci")
        .await
        .iter()
        .map(|run| run["sha"].clone())
        .collect();
    assert_eq!(ci, [json!(opened), json!(synchronized)]);

    scene.forge.hold_merges(REPOSITORY, true);
    scene
        .forge
        .commit(REPOSITORY, "code", [("src/a.rs", "3\n")]);
    let conflicted = scene
        .forge
        .pull_request_event(REPOSITORY, number, "synchronize", ROMAN);
    delivered(&scene.api, &scene.forge, "pull_request", &conflicted).await;
    scene.failed_once().await;
    scene
        .database
        .connection()
        .execute_unprepared(
            "UPDATE bosun.webhook_delivery SET attempts = 9, next_attempt_at = now() \
             WHERE processed_at IS NULL",
        )
        .await
        .expect("the attempts of the delivery");
    processed(&scene.database).await;
    assert_eq!(scene.named("ci").await.len(), 2);
    assert_eq!(scene.named("target").await.len(), 3);
}

#[tokio::test]
async fn a_pull_request_with_more_files_than_the_forge_lists_meets_every_path_filter() {
    let scene = Scene::new(&[(
        ".bosun/workflows/src.yml",
        &workflow("src", "  pull_request:\n    paths: ['src/**']\n"),
    )])
    .await;
    scene.forge.commit(
        REPOSITORY,
        "docs",
        [("docs/guide.md".to_owned(), "# guide\n".to_owned())],
    );
    let docs = scene.forge.open_pull_request(REPOSITORY, "main", "docs");
    scene.pull_request(docs, "opened", ROMAN).await;
    assert_eq!(scene.runs().await, Vec::<Value>::new());

    scene.forge.commit(
        REPOSITORY,
        "large",
        (0..PULL_REQUEST_FILES)
            .map(|index| (format!("docs/{index:04}.md"), "x".to_owned()))
            .chain([("src/a.rs".to_owned(), "fn main() {}\n".to_owned())]),
    );
    let large = scene.forge.open_pull_request(REPOSITORY, "main", "large");
    scene.pull_request(large, "opened", ROMAN).await;
    let runs = scene.runs().await;
    let [run] = runs.as_slice() else {
        panic!("not one run: {runs:#?}");
    };
    assert_eq!(run["ref"], format!("refs/pull/{large}/merge"));
}
