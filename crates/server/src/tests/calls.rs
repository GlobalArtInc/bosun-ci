use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bosun_db::entity::{attempt, job};
use bosun_db::installations::Account;
use bosun_db::pools::Access;
use bosun_db::queue::ClaimedJob;
use bosun_db::runs::{NewRun, Trust};
use bosun_forge::GitHubApp;
use bosun_proto::agent::v1::server_message::Message;
use bosun_proto::assignment::Documents;
use bosun_testkit::forge::{APP_ID, APP_KEY, FakeForge};
use bosun_types::action_policy::ActionPolicy;
use bosun_types::api::{Capability, Repository, TrustLevel};
use bosun_types::diagnostic::{Diagnostic, Severity};
use bosun_types::id::{JobId, RunId};
use bosun_types::ir;
use bosun_types::status::{Conclusion, JobStatus, RunConclusion, RunStatus};
use proptest::bool::weighted;
use proptest::collection::vec;
use proptest::prelude::{Strategy, any};
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use sea_orm::prelude::Uuid;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, TransactionTrait};
use serde::Deserialize;
use serde_json::{Map, json};

use super::journal::{Action, consistent, next_report, perform};
use super::queue::{LABELS, LEASE, claim, connected, connected_in, parse, pool};
use super::secrets::{secret, strings};
use super::{World, finished, keys, operator, run_cancel, site};
use crate::agents::progress::{self, JobEnd, Progress, Reporter};
use crate::callees;
use crate::clock::Clock;
use crate::forge::Forge;
use crate::forge_events;
use crate::forge_token::Tokens;
use crate::metrics::Meters;
use crate::outbox::{self, Outbox};
use crate::planner;
use crate::queue;

const OWNER: &str = "GlobalArt";

const KEEL: &str = "GlobalArt/keel";

const SHARED: &str = "GlobalArt/shared";

const LIB: &str = "ThirdParty/lib";

const CALLER: &str = ".github/workflows/ci.yml";

const BUILD: &str = ".github/workflows/build.yml";

const HELD: &[(&str, &str)] = &[
    ("checks", "write"),
    ("contents", "read"),
    ("metadata", "read"),
    ("statuses", "write"),
];

struct Scene {
    installation: u64,
    world: World,
    fake: FakeForge,
    forge: Arc<Forge>,
    tokens: Tokens,
    outbox: Outbox,
    replica: Uuid,
}

impl Scene {
    async fn new() -> Scene {
        let world = World::new().await;
        let fake = FakeForge::start().await;
        let installation = fake.install(OWNER, HELD);
        let db = world.db();
        let project = bosun_db::projects::find(db, world.workflow.project_id)
            .await
            .expect("a project")
            .expect("the project of the world");
        let repositories: Vec<Repository> = [KEEL, SHARED]
            .into_iter()
            .map(|full_name| Repository {
                id: fake.create_repository(installation, full_name),
                owner: OWNER.to_owned(),
                name: full_name.split_once('/').expect("an owner").1.to_owned(),
                default_branch: "main".to_owned(),
            })
            .collect();
        let strangers = fake.install("ThirdParty", HELD);
        fake.create_repository(strangers, LIB);
        let transaction = db.begin().await.expect("a transaction");
        bosun_db::installations::record(
            &transaction,
            installation,
            &Account {
                id: 9_001,
                login: OWNER,
            },
        )
        .await
        .expect("an installation");
        bosun_db::installations::map(&transaction, installation, project.organisation_id)
            .await
            .expect("a mapped installation");
        bosun_db::installations::sync(&transaction, installation, &repositories)
            .await
            .expect("a sync");
        transaction.commit().await.expect("a commit");
        bosun_db::projects::bind(db, project.id, Some(&repositories[0]))
            .await
            .expect("a binding");
        let app =
            GitHubApp::new(APP_ID, APP_KEY.as_bytes(), &fake.api_url()).expect("the App's client");
        let forge = Arc::new(Forge::of(app));
        let keys = Arc::new(keys());
        Scene {
            installation,
            tokens: Tokens::new(db.clone(), Some(Arc::clone(&forge)), Arc::clone(&keys)),
            outbox: Outbox {
                db: db.clone(),
                store: Arc::clone(world.store()),
                forge: Arc::clone(&forge),
                keys,
                site: site(),
                clock: Clock::system(),
                meters: Arc::new(Meters::default()),
            },
            forge,
            world,
            fake,
            replica: Uuid::now_v7(),
        }
    }

    fn commit(&self, repository: &str, files: &[(&str, &str)]) -> String {
        self.fake.commit(repository, "main", files.iter().copied())
    }

    async fn start(&self, caller: &str, files: &[(&str, &str)]) -> RunId {
        self.start_as(caller, files, Trust::Trusted).await
    }

    async fn start_as(&self, caller: &str, files: &[(&str, &str)], trust: Trust) -> RunId {
        let mut tree = vec![(CALLER, caller)];
        tree.extend_from_slice(files);
        let sha = self.commit(KEEL, &tree);
        self.launch_as(caller, &sha, trust).await
    }

    async fn loaded(&self, caller: &str, files: &[(&str, &str)]) -> callees::Loaded {
        let mut tree = vec![(CALLER, caller)];
        tree.extend_from_slice(files);
        let sha = self.commit(KEEL, &tree);
        let transaction = self.world.db().begin().await.expect("a transaction");
        forge_events::read_callees(
            &transaction,
            Some(&self.forge),
            &self.world.workflow,
            Some(&sha),
            &parse(caller),
        )
        .await
        .unwrap_or_else(|_| panic!("the callees were not read"))
    }

    fn release(&self, repository: &str, path: &str, text: &str) {
        let sha = self.commit(repository, &[(path, text)]);
        self.fake.tag(repository, "v1", &sha);
    }

    async fn launch(&self, caller: &str, sha: &str) -> RunId {
        self.launch_as(caller, sha, Trust::Trusted).await
    }

    async fn launch_as(&self, caller: &str, sha: &str, trust: Trust) -> RunId {
        let workflow = parse(caller);
        let transaction = self.world.db().begin().await.expect("a transaction");
        let loaded = forge_events::read_callees(
            &transaction,
            Some(&self.forge),
            &self.world.workflow,
            Some(sha),
            &workflow,
        )
        .await
        .unwrap_or_else(|_| panic!("the callees were not read"));
        let payload = json!({});
        let inputs = Map::new();
        let actor = operator();
        let new = |ir| NewRun {
            workflow: &self.world.workflow,
            revision_id: self.world.workflow.revision_id.expect("a revision"),
            ir,
            event: "push",
            git_ref: Some("refs/heads/main"),
            sha: Some(sha),
            payload: &payload,
            inputs: &inputs,
            actor: &actor,
            trust,
        };
        let run = match &loaded {
            callees::Loaded::Unchanged => {
                planner::dispatch(&transaction, new(&workflow), &[], &site()).await
            }
            callees::Loaded::Ready(ready, warnings) => {
                planner::dispatch(&transaction, new(ready), warnings, &site()).await
            }
            callees::Loaded::Refused(diagnostics) => {
                let failed = forge_events::bare(Some(&workflow.triggers));
                planner::fail_startup(&transaction, new(&failed), diagnostics).await
            }
        }
        .unwrap_or_else(|error| panic!("the run was not planned: {error}"));
        transaction.commit().await.expect("a commit");
        run.id
    }

    async fn rows(&self, run: RunId) -> Vec<job::Model> {
        rows(&self.world, run).await
    }

    async fn notices(&self, run: RunId) -> Vec<Diagnostic> {
        self.world
            .run(run)
            .await
            .notices
            .into_iter()
            .map(|notice| notice.diagnostic)
            .collect()
    }

    async fn claimed(&self, name: &str) -> ClaimedJob {
        self.claimed_by(name, &LABELS).await.1
    }

    async fn claimed_by(&self, name: &str, labels: &[&str]) -> (Reporter, ClaimedJob) {
        let agent = connected(&self.world, name, labels, self.replica).await;
        let claimed = claim(self.world.db(), agent.id, self.replica, LEASE, &[])
            .await
            .expect("a job for the agent");
        (agent, claimed)
    }

    async fn ran(&self, agent: &Reporter, claimed: &ClaimedJob, outputs: &[(&str, &str)]) {
        let accepted = progress::accepted(self.world.db(), agent, claimed.execution_id, &site())
            .await
            .expect("an acceptance");
        assert_eq!(accepted, Progress::Applied);
        let end = JobEnd {
            conclusion: Conclusion::Success,
            outputs: outputs
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        };
        let ended =
            progress::job_finished(self.world.db(), agent, claimed.execution_id, end, &site())
                .await
                .expect("a finished job");
        assert_eq!(ended, Progress::Applied);
    }

    async fn assigned(&self, claimed: ClaimedJob) -> Documents {
        let offer = queue::assignment(
            self.world.db(),
            claimed,
            &site(),
            &self.tokens,
            self.world.actions(),
        )
        .await
        .expect("an assignment")
        .expect("an offer");
        let Some(Message::Assignment(assignment)) = offer.message.message else {
            panic!("the offer holds no assignment");
        };
        Documents::read(&assignment).expect("documents the agent reads")
    }
}

async fn rows(world: &World, run: RunId) -> Vec<job::Model> {
    let attempts: Vec<_> = attempt::Entity::find()
        .filter(attempt::Column::RunId.eq(run.as_uuid()))
        .all(world.db())
        .await
        .expect("the attempts")
        .into_iter()
        .map(|attempt| attempt.id)
        .collect();
    job::Entity::find()
        .filter(job::Column::AttemptId.is_in(attempts))
        .order_by_asc(job::Column::Id)
        .all(world.db())
        .await
        .expect("the jobs")
}

fn named<'a>(jobs: &'a [job::Model], key: &str) -> Vec<&'a job::Model> {
    jobs.iter().filter(|job| job.key == key).collect()
}

fn one<'a>(jobs: &'a [job::Model], key: &str) -> &'a job::Model {
    let found = named(jobs, key);
    assert_eq!(found.len(), 1, "{key} in {jobs:#?}");
    found[0]
}

fn id(job: &job::Model) -> JobId {
    JobId::try_from(job.id).expect("a version 7 id")
}

fn status(job: &job::Model) -> JobStatus {
    super::name(&job.status)
}

fn composed(caller: &str, callee: &str) -> ir::Workflow {
    let mut workflow = parse(caller);
    let called = parse(callee);
    let calls = workflow
        .jobs
        .iter()
        .filter(|template| matches!(template.kind, ir::JobKind::Call(_)))
        .map(|template| (template.key.clone(), 0))
        .collect();
    workflow.calls = calls;
    workflow.callees = vec![ir::Reusable {
        repository: KEEL.to_owned(),
        path: BUILD.to_owned(),
        commit: "1".repeat(40),
        workflow: called,
    }];
    workflow
}

const BUILD_YML: &str = "\
on:
  workflow_call:
    inputs:
      label:
        type: string
        default: linux
      count:
        type: number
        default: 3
      fast:
        type: boolean
    secrets:
      token:
      opt:
    outputs:
      picked:
        value: ${{ jobs.build.outputs.picked }}
jobs:
  build:
    runs-on:
      - self-hosted
      - ${{ inputs.label }}
      - ${{ inputs.fast && 'fast' || 'slow' }}
      - n${{ inputs.count }}
    outputs:
      picked: ${{ steps.pick.outputs.picked }}
    steps:
      - id: pick
        run: make
  test:
    needs: build
    runs-on: self-hosted
    steps:
      - run: make test
";

const CALLER_YML: &str = "\
on: push
jobs:
  setup:
    runs-on: self-hosted
    steps:
      - run: make
  explicit:
    needs: setup
    uses: ./.github/workflows/build.yml
    with:
      label: mac
      count: 5
      fast: 'true'
  plain:
    needs: setup
    uses: ./.github/workflows/build.yml
  after:
    needs: [explicit, plain]
    runs-on: ${{ needs.explicit.outputs.picked }}
    steps:
      - run: make
";

fn labels(job: &job::Model) -> Vec<&str> {
    job.labels.iter().map(String::as_str).collect()
}

#[tokio::test]
async fn a_call_runs_the_jobs_of_its_callee_as_members_with_typed_inputs_and_hands_back_its_outputs()
 {
    let world = World::new().await;
    let run = world.dispatch(&composed(CALLER_YML, BUILD_YML)).await;
    world
        .finish(run, "setup", finished(Conclusion::Success))
        .await;
    let stored = rows(&world, run).await;
    let explicit = one(&stored, "explicit");
    let members = |group: &job::Model| -> Vec<(String, String)> {
        stored
            .iter()
            .filter(|job| job.group_id == Some(group.id))
            .map(|job| (job.key.clone(), job.name.clone()))
            .collect()
    };
    assert_eq!(
        members(explicit),
        [
            ("explicit/build".to_owned(), "explicit / build".to_owned()),
            ("explicit/test".to_owned(), "explicit / test".to_owned())
        ]
    );
    assert_eq!(status(explicit), JobStatus::Pending);
    let build = one(&stored, "explicit/build");
    assert_eq!(labels(build), ["self-hosted", "mac", "fast", "n5"]);
    assert_eq!(
        labels(one(&stored, "plain/build")),
        ["self-hosted", "linux", "slow", "n3"]
    );
    assert_eq!(status(one(&stored, "explicit/test")), JobStatus::Pending);
    assert_eq!(status(one(&stored, "after")), JobStatus::Pending);

    let picked = JobEnd {
        conclusion: Conclusion::Success,
        outputs: BTreeMap::from([("picked".to_owned(), "x".to_owned())]),
    };
    world.finish(run, "explicit/build", picked).await;
    world
        .finish(run, "explicit/test", finished(Conclusion::Success))
        .await;
    let stored = rows(&world, run).await;
    let explicit = one(&stored, "explicit");
    assert_eq!(
        (status(explicit), explicit.conclusion.as_deref()),
        (JobStatus::Completed, Some("success"))
    );
    assert_eq!(explicit.outputs, json!({ "picked": "x" }));
    assert_eq!(status(one(&stored, "after")), JobStatus::Pending);

    for key in ["plain/build", "plain/test"] {
        world.finish(run, key, finished(Conclusion::Success)).await;
    }
    let stored = rows(&world, run).await;
    let after = one(&stored, "after");
    assert_eq!(
        (status(after), labels(after)),
        (JobStatus::Queued, vec!["x"])
    );
}

const SECRETS_YML: &str = "\
on:
  workflow_call:
    secrets:
      token:
      opt:
jobs:
  use:
    runs-on: [self-hosted, linux]
    env:
      TOKEN: ${{ secrets.token }}
      OPT: ${{ secrets.opt }}
      DEPLOY: ${{ secrets.deploy }}
      OTHER: ${{ secrets.other }}
    steps:
      - run: make
";

const SECRETS_CALLER: &str = "\
on: push
jobs:
  explicit:
    uses: ./.github/workflows/secrets.yml
    secrets:
      token: ${{ secrets.DEPLOY }}
  inherited:
    uses: ./.github/workflows/secrets.yml
    secrets: inherit
";

#[tokio::test]
async fn secrets_cross_to_the_callee_only_as_the_caller_passes_them_and_the_token_is_always_there()
{
    let scene = Scene::new().await;
    let (_, in_project) = super::secrets::scopes(&scene.world).await;
    secret(&scene.world, in_project, "DEPLOY", "deploy-value").await;
    secret(&scene.world, in_project, "OTHER", "other-value").await;
    scene
        .start(
            SECRETS_CALLER,
            &[(".github/workflows/secrets.yml", SECRETS_YML)],
        )
        .await;
    let mut documents = BTreeMap::new();
    for name in ["runner1-1", "runner1-2"] {
        let claimed = scene.claimed(name).await;
        let key = claimed.key.to_string();
        documents.insert(key, scene.assigned(claimed).await);
    }
    let explicit = &documents["explicit/use"];
    let token = explicit.contexts.secrets["GITHUB_TOKEN"]
        .as_str()
        .expect("a token")
        .to_owned();
    assert!(token.starts_with("ghs_"), "{token}");
    assert_eq!(
        explicit.contexts.secrets,
        strings(&[
            ("GITHUB_TOKEN", &token),
            ("OPT", ""),
            ("TOKEN", "deploy-value")
        ])
    );
    assert_eq!(
        explicit.evaluated.env,
        [
            ("DEPLOY".to_owned(), String::new()),
            ("OPT".to_owned(), String::new()),
            ("OTHER".to_owned(), String::new()),
            ("TOKEN".to_owned(), "deploy-value".to_owned()),
        ]
        .into()
    );
    let inherited = &documents["inherited/use"];
    assert_eq!(
        inherited.evaluated.env,
        [
            ("DEPLOY".to_owned(), "deploy-value".to_owned()),
            ("OPT".to_owned(), String::new()),
            ("OTHER".to_owned(), "other-value".to_owned()),
            ("TOKEN".to_owned(), String::new()),
        ]
        .into()
    );
    assert!(
        inherited.contexts.secrets["GITHUB_TOKEN"]
            .as_str()
            .is_some_and(|token| token.starts_with("ghs_"))
    );
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Refusal {
    description: String,
    caller: String,
    files: BTreeMap<String, String>,
    #[serde(default)]
    shared: BTreeMap<String, String>,
    #[serde(default)]
    foreign: BTreeMap<String, String>,
    policy: Option<ActionPolicy>,
    expected: Expected,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    code: String,
    file: String,
    path: Vec<String>,
    line: u32,
    message: String,
}

fn refusals() -> Vec<(PathBuf, Refusal)> {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/fixtures/calls");
    let mut files: Vec<PathBuf> = fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|file| {
            let text = fs::read_to_string(&file)
                .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
            let case = serde_json::from_str(&text)
                .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
            (file, case)
        })
        .collect()
}

impl Scene {
    async fn organisation(&self) -> bosun_types::id::OrganisationId {
        bosun_db::projects::find(self.world.db(), self.world.workflow.project_id)
            .await
            .expect("a project")
            .expect("the project of the world")
            .organisation_id
    }

    async fn policy(&self, policy: &ActionPolicy) {
        bosun_db::organisations::set_action_policy(
            self.world.db(),
            self.organisation().await,
            policy,
        )
        .await
        .expect("a policy");
    }

    async fn failed(&self, run: RunId) -> Vec<Diagnostic> {
        let shown = self.world.run(run).await;
        assert_eq!(
            (shown.status, shown.conclusion),
            (RunStatus::Completed, Some(RunConclusion::StartupFailure)),
            "{shown:#?}"
        );
        assert!(self.rows(run).await.is_empty(), "a refused run has jobs");
        self.notices(run).await
    }
}

#[tokio::test]
async fn a_callee_that_is_missing_invalid_or_forbidden_ends_the_run_with_startup_failure_at_the_caller()
 {
    let scene = Scene::new().await;
    let cases = refusals();
    assert!(!cases.is_empty(), "contracts/fixtures/calls holds no case");
    for (file, case) in cases {
        let file = file.display();
        if !case.shared.is_empty() {
            let sha = scene.commit(
                SHARED,
                &case
                    .shared
                    .iter()
                    .map(|(path, text)| (path.as_str(), text.as_str()))
                    .collect::<Vec<_>>(),
            );
            scene.fake.tag(SHARED, "v1", &sha);
        }
        if !case.foreign.is_empty() {
            let sha = scene.commit(
                LIB,
                &case
                    .foreign
                    .iter()
                    .map(|(path, text)| (path.as_str(), text.as_str()))
                    .collect::<Vec<_>>(),
            );
            scene.fake.tag(LIB, "v1", &sha);
            scene.fake.publish(LIB);
        }
        scene
            .policy(case.policy.as_ref().unwrap_or(&ActionPolicy::All {}))
            .await;
        let files: Vec<(&str, &str)> = case
            .files
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str()))
            .collect();
        let run = scene.start(&case.caller, &files).await;
        let notices = scene.failed(run).await;
        let expected = &case.expected;
        let found = notices
            .iter()
            .find(|notice| notice.code == expected.code)
            .unwrap_or_else(|| {
                panic!(
                    "{file}: {}: no `{}` in {notices:#?}",
                    case.description, expected.code
                )
            });
        assert_eq!(found.severity, Severity::Error, "{file}");
        assert_eq!(found.file, expected.file, "{file}");
        assert_eq!(found.span.line, expected.line, "{file}");
        assert_eq!(
            serde_json::to_value(&found.path).expect("a path"),
            json!(expected.path),
            "{file}"
        );
        assert!(
            found.message.contains(&expected.message),
            "{file}: `{}` lacks `{}`",
            found.message,
            expected.message
        );
    }
}

fn release(job: &str) -> String {
    format!(
        "on: workflow_call\njobs:\n  {job}:\n    runs-on: self-hosted\n    steps:\n      - run: make\n"
    )
}

#[tokio::test]
async fn a_callee_of_another_repository_is_pinned_to_the_commit_its_tag_names_when_the_run_is_planned()
 {
    let scene = Scene::new().await;
    let first = scene.commit(
        SHARED,
        &[(".github/workflows/release.yml", &release("first"))],
    );
    scene.fake.tag(SHARED, "v1", &first);
    let caller = "\
on: push
jobs:
  setup:
    runs-on: self-hosted
    steps:
      - run: make
  release:
    needs: setup
    uses: GlobalArt/shared/.github/workflows/release.yml@v1
";
    let run = scene.start(caller, &[]).await;
    let second = scene.commit(
        SHARED,
        &[(".github/workflows/release.yml", &release("second"))],
    );
    scene.fake.tag(SHARED, "v1", &second);
    scene
        .world
        .finish(run, "setup", finished(Conclusion::Success))
        .await;
    let stored = scene.rows(run).await;
    assert_eq!(
        stored
            .iter()
            .map(|job| job.key.as_str())
            .collect::<Vec<_>>(),
        ["setup", "release", "release/first"]
    );
    let snapshot = bosun_db::entity::run::Entity::find_by_id(run.as_uuid())
        .one(scene.world.db())
        .await
        .expect("a run")
        .expect("the run");
    assert_eq!(snapshot.ir["callees"][0]["commit"], json!(first));
    assert_eq!(snapshot.ir["callees"][0]["repository"], json!(SHARED));
}

fn chain(links: usize) -> Vec<(String, String)> {
    (1..=links)
        .map(|number| {
            let body = if number == links {
                "    runs-on: self-hosted\n    steps:\n      - run: make\n".to_owned()
            } else {
                format!("    uses: ./.github/workflows/f{}.yml\n", number + 1)
            };
            (
                format!(".github/workflows/f{number}.yml"),
                format!("on: workflow_call\njobs:\n  next:\n{body}"),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_run_nests_reusable_workflows_ten_levels_deep_and_refuses_the_eleventh() {
    let scene = Scene::new().await;
    let caller = "on: push\njobs:\n  top:\n    uses: ./.github/workflows/f1.yml\n";
    for (links, deepest) in [(10, None), (11, Some(("f10", 11)))] {
        let files = chain(links);
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str()))
            .collect();
        let run = scene.start(caller, &files).await;
        match deepest {
            None => {
                let shown = scene.world.run(run).await;
                assert_eq!(shown.status, RunStatus::Queued, "{shown:#?}");
                let stored = scene.rows(run).await;
                let deepest = stored
                    .iter()
                    .max_by_key(|job| job.key.matches('/').count())
                    .expect("a job");
                assert_eq!(deepest.key.matches('/').count(), 10, "{stored:#?}");
                assert_eq!(status(deepest), JobStatus::Queued);
            }
            Some((file, level)) => {
                let notices = scene.failed(run).await;
                let refusal = notices
                    .iter()
                    .find(|notice| notice.code == "limit_exceeded")
                    .unwrap_or_else(|| panic!("no limit in {notices:#?}"));
                assert_eq!(refusal.file, format!(".github/workflows/{file}.yml"));
                assert!(
                    refusal
                        .message
                        .contains(&format!("at level {level} of nesting")),
                    "{}",
                    refusal.message
                );
            }
        }
    }
}

#[tokio::test]
async fn a_run_calls_fifty_distinct_reusable_workflows_and_refuses_the_next() {
    let scene = Scene::new().await;
    let files: Vec<(String, String)> = (1..=51)
        .map(|number| (format!(".github/workflows/w{number}.yml"), release("only")))
        .collect();
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    let calls = |count: usize| {
        let mut caller = "on: push\njobs:\n".to_owned();
        for number in 1..=count {
            caller.push_str(&format!(
                "  c{number}:\n    uses: ./.github/workflows/w{number}.yml\n"
            ));
        }
        caller
    };
    let run = scene.start(&calls(50), &files).await;
    assert_eq!(scene.world.run(run).await.status, RunStatus::Queued);
    let run = scene.start(&calls(51), &files).await;
    let notices = scene.failed(run).await;
    let refusal = notices
        .iter()
        .find(|notice| notice.code == "limit_exceeded")
        .unwrap_or_else(|| panic!("no limit in {notices:#?}"));
    assert!(
        refusal
            .message
            .contains("at most 50 distinct reusable workflows"),
        "{}",
        refusal.message
    );
}

const FAN_YML: &str = "\
on: push
jobs:
  fan:
    strategy:
      matrix:
        os: [a, b]
    uses: ./.github/workflows/build.yml
    with:
      label: ${{ matrix.os }}
  skipped:
    if: false
    uses: ./.github/workflows/build.yml
  after:
    needs: [fan, skipped]
    if: always()
    runs-on: self-hosted
    steps:
      - run: make
";

#[tokio::test]
async fn a_matrix_on_a_call_makes_a_group_per_member_and_dependents_wait_for_all_of_them() {
    let world = World::new().await;
    let run = world.dispatch(&composed(FAN_YML, BUILD_YML)).await;
    let stored = rows(&world, run).await;
    let skipped = one(&stored, "skipped");
    assert_eq!(
        (status(skipped), skipped.conclusion.as_deref()),
        (JobStatus::Completed, Some("skipped"))
    );
    assert!(
        stored.iter().all(|job| job.group_id != Some(skipped.id)),
        "a skipped call has no members"
    );
    let parents: Vec<&job::Model> = stored
        .iter()
        .filter(|job| job.key == "fan" && job.matrix.is_some())
        .collect();
    assert_eq!(parents.len(), 2, "{stored:#?}");
    let builds = named(&stored, "fan/build");
    assert_eq!(builds.len(), 2);
    assert_eq!(
        builds
            .iter()
            .map(|build| (build.name.as_str(), labels(build)[1]))
            .collect::<Vec<_>>(),
        [("fan (a) / build", "a"), ("fan (b) / build", "b")]
    );
    assert!(builds.iter().all(|build| {
        parents
            .iter()
            .any(|parent| build.group_id == Some(parent.id))
    }));
    assert_eq!(status(one(&stored, "after")), JobStatus::Pending);

    for (position, build) in builds.iter().enumerate() {
        world
            .finish_job(run, id(build), finished(Conclusion::Success))
            .await;
        let stored = rows(&world, run).await;
        let test = named(&stored, "fan/test")[position];
        world
            .finish_job(run, id(test), finished(Conclusion::Success))
            .await;
        let stored = rows(&world, run).await;
        let after = status(one(&stored, "after"));
        let expected = if position == 0 {
            JobStatus::Pending
        } else {
            JobStatus::Queued
        };
        assert_eq!(after, expected, "after member {position}");
    }
    let stored = rows(&world, run).await;
    let group = stored
        .iter()
        .find(|job| job.key == "fan" && job.matrix.is_none())
        .expect("the matrix group");
    assert_eq!(group.conclusion.as_deref(), Some("success"));
}

#[tokio::test]
async fn the_jobs_of_a_callee_are_reported_to_the_forge_as_check_runs_named_after_their_caller() {
    let scene = Scene::new().await;
    let caller = "\
on: push
jobs:
  Release:
    name: Ship it
    uses: ./.github/workflows/build.yml
  unit:
    runs-on: self-hosted
    steps:
      - run: make
";
    let run = scene.start(caller, &[(BUILD, BUILD_YML)]).await;
    while outbox::next(&scene.outbox)
        .await
        .unwrap_or_else(|error| panic!("the outbox failed: {error}"))
    {}
    let mut names: Vec<String> = scene
        .fake
        .check_runs()
        .into_iter()
        .map(|check| check.name)
        .collect();
    names.sort();
    assert_eq!(names, ["Ship it / build", "unit"], "run {run}");
}

const MOST_JOBS: usize = 5;

const MOST_ACTIONS: usize = 300;

const SHAPES: u8 = 3;

const NESTED: u8 = 1;

const MATRIX: u8 = 2;

#[derive(Debug, Clone)]
struct Drive {
    shape: u8,
    needs: Vec<Vec<bool>>,
    results: Vec<bool>,
    tolerated: Vec<bool>,
    commands: bool,
    choices: Vec<u32>,
}

fn drives() -> impl Strategy<Value = Drive> {
    (1..=MOST_JOBS)
        .prop_flat_map(|count| {
            let needs: Vec<_> = (0..count).map(|index| vec(any::<bool>(), index)).collect();
            (
                0..SHAPES,
                needs,
                vec(weighted(0.7), count),
                vec(weighted(0.25), count),
                any::<bool>(),
                vec(any::<u32>(), 0..96),
            )
        })
        .prop_map(
            |(shape, needs, results, tolerated, commands, choices)| Drive {
                shape,
                needs,
                results,
                tolerated,
                commands,
                choices,
            },
        )
}

fn callee_of(drive: &Drive) -> String {
    let mut source = "on:\n  workflow_call:\n    outputs:\n      first:\n        value: ${{ jobs.j0.result }}\njobs:\n".to_owned();
    for (index, needs) in drive.needs.iter().enumerate() {
        let needed: Vec<String> = needs
            .iter()
            .enumerate()
            .filter(|(_, needed)| **needed)
            .map(|(need, _)| format!("j{need}"))
            .collect();
        source.push_str(&format!(
            "  j{index}:\n    runs-on: self-hosted\n    continue-on-error: {}\n    needs: [{}]\n    if: always()\n    steps:\n      - run: make\n",
            drive.tolerated[index],
            needed.join(", ")
        ));
    }
    source
}

const WRAPPER_YML: &str = "\
on: push
jobs:
  call:
    uses: ./.github/workflows/build.yml
  after:
    needs: call
    if: always()
    runs-on: self-hosted
    steps:
      - run: make
";

const MATRIX_WRAPPER_YML: &str = "\
on: push
jobs:
  call:
    strategy:
      matrix:
        side: [a, b]
    uses: ./.github/workflows/build.yml
  after:
    needs: call
    if: always()
    runs-on: self-hosted
    steps:
      - run: make
";

const MIDDLE_CALL_YML: &str = "\
on: workflow_call
jobs:
  inner:
    uses: ./.github/workflows/leaf.yml
";

fn shaped(drive: &Drive) -> ir::Workflow {
    let leaf = callee_of(drive);
    match drive.shape {
        NESTED => {
            let mut workflow = composed(WRAPPER_YML, MIDDLE_CALL_YML);
            workflow.callees[0].workflow.calls = workflow.callees[0]
                .workflow
                .jobs
                .iter()
                .map(|template| (template.key.clone(), 1))
                .collect();
            workflow.callees.push(ir::Reusable {
                repository: KEEL.to_owned(),
                path: ".github/workflows/leaf.yml".to_owned(),
                commit: "2".repeat(40),
                workflow: parse(&leaf),
            });
            workflow
        }
        MATRIX => composed(MATRIX_WRAPPER_YML, &leaf),
        _ => composed(WRAPPER_YML, &leaf),
    }
}

fn leaf_index(key: &str) -> Option<usize> {
    key.rsplit('/')
        .next()
        .and_then(|local| local.strip_prefix('j'))
        .and_then(|index| index.parse().ok())
}

fn expected_conclusion(
    projection: &super::Projection,
    group: JobId,
    drive: &Drive,
) -> Result<Conclusion, TestCaseError> {
    let mut expected = Conclusion::Success;
    for (&member, row) in projection
        .jobs
        .iter()
        .filter(|(_, row)| row.group == Some(group))
    {
        let below = projection
            .jobs
            .values()
            .any(|other| other.group == Some(member));
        let (conclusion, tolerated) = if below {
            let settled = expected_conclusion(projection, member, drive)?;
            if row.conclusion != Some(settled) {
                return Err(TestCaseError::fail(format!(
                    "the group {} concluded {:?}, and its members say {settled:?}: {projection:#?}",
                    row.key, row.conclusion
                )));
            }
            (Some(settled), false)
        } else {
            (
                row.conclusion,
                leaf_index(&row.key).is_some_and(|index| drive.tolerated[index]),
            )
        };
        if matches!(conclusion, Some(Conclusion::Failure | Conclusion::TimedOut)) && !tolerated {
            expected = Conclusion::Failure;
        } else if conclusion == Some(Conclusion::Cancelled) && expected != Conclusion::Failure {
            expected = Conclusion::Cancelled;
        }
    }
    Ok(expected)
}

#[test]
fn the_journal_of_a_call_group_always_replays_to_the_state_of_its_rows() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let world = runtime.block_on(World::new());
    let mut runner = TestRunner::new(Config {
        cases: 64,
        failure_persistence: None,
        ..Config::default()
    });
    let result = runner.run(&drives(), |drive| {
        runtime.block_on(drive_call(&world, &drive))
    });
    runtime.block_on(async move { drop(world) });
    if let Err(failure) = result {
        panic!("{failure}");
    }
}

async fn drive_call(world: &World, drive: &Drive) -> Result<(), TestCaseError> {
    let run = world.dispatch(&shaped(drive)).await;
    let mut choices = drive.choices.iter().copied();
    let mut settled = false;
    for _ in 0..MOST_ACTIONS {
        let projection = consistent(world, run).await?;
        if !projection.jobs.values().any(|row| row.key == "call") {
            return Err(TestCaseError::fail("the call group is missing".to_owned()));
        }
        for (&group, row) in &projection.jobs {
            if row.status == JobStatus::Completed
                && projection.jobs.values().any(|member| {
                    member.group == Some(group) && member.status != JobStatus::Completed
                })
            {
                return Err(TestCaseError::fail(format!(
                    "the group {} completed before its members: {projection:#?}",
                    row.key
                )));
            }
        }
        let controls = world.controls().await;
        let mut progress = Vec::new();
        let mut extra = vec![Action::CancelRun];
        for (&job, row) in &projection.jobs {
            if row.status != JobStatus::Completed {
                extra.push(Action::CancelJob(job));
            }
            if row.status == JobStatus::Queued {
                progress.push(Action::Assign(job));
                extra.push(Action::TimeOut(job));
            }
        }
        for (&execution, row) in &projection.executions {
            match row.status {
                bosun_types::status::ExecutionStatus::Assigned => {
                    progress.push(Action::Accept(execution));
                    extra.push(Action::Expire(execution));
                    extra.push(Action::Reject(execution));
                }
                bosun_types::status::ExecutionStatus::InProgress => {
                    let result = leaf_index(&projection.jobs[&row.job].key)
                        .is_none_or(|index| drive.results[index]);
                    progress.push(next_report(&projection, execution, &[result]));
                    if controls.contains(&execution) {
                        progress.push(Action::FinishJob(execution, Conclusion::Cancelled));
                    }
                    extra.push(Action::Lose(execution));
                }
                _ => {}
            }
        }
        let Some(&first) = progress.first() else {
            settled = true;
            break;
        };
        let action = match choices.next() {
            Some(choice) if drive.commands && choice % 6 == 0 => {
                extra[(choice as usize / 6) % extra.len()]
            }
            Some(choice) => progress[(choice as usize / 6) % progress.len()],
            None => first,
        };
        perform(world, run, action).await?;
    }
    if !settled {
        return Err(TestCaseError::fail(format!(
            "the run was still going after {MOST_ACTIONS} actions"
        )));
    }
    let projection = consistent(world, run).await?;
    let run_row = projection.run.clone().expect("the run");
    if run_row.status != RunStatus::Completed {
        return Err(TestCaseError::fail(format!(
            "the run is stuck without anything left to do: {projection:#?}"
        )));
    }
    let (&call, group) = projection
        .jobs
        .iter()
        .find(|(_, row)| row.key == "call" && row.group.is_none())
        .expect("the call group");
    let expected = expected_conclusion(&projection, call, drive)?;
    if group.conclusion != Some(expected) {
        return Err(TestCaseError::fail(format!(
            "the group concluded {:?}, and its members say {expected:?}: {projection:#?}",
            group.conclusion
        )));
    }
    Ok(())
}

fn serial(fail_fast: bool) -> String {
    format!(
        "on: push\njobs:\n  fan:\n    strategy:\n      fail-fast: {fail_fast}\n      max-parallel: 1\n      matrix:\n        os: [a, b]\n    uses: ./.github/workflows/build.yml\n    with:\n      label: ${{{{ matrix.os }}}}\n"
    )
}

#[tokio::test]
async fn max_parallel_holds_back_the_next_group_of_a_call_matrix_and_fail_fast_cancels_it() {
    let world = World::new().await;
    let run = world.dispatch(&composed(&serial(false), BUILD_YML)).await;
    let stored = rows(&world, run).await;
    assert_eq!(named(&stored, "fan/build").len(), 1, "{stored:#?}");
    let first = id(named(&stored, "fan/build")[0]);
    world
        .finish_job(run, first, finished(Conclusion::Success))
        .await;
    let stored = rows(&world, run).await;
    let test = id(named(&stored, "fan/test")[0]);
    assert_eq!(named(&stored, "fan/build").len(), 1);
    world
        .finish_job(run, test, finished(Conclusion::Success))
        .await;
    let stored = rows(&world, run).await;
    assert_eq!(named(&stored, "fan/build").len(), 2, "{stored:#?}");

    let run = world.dispatch(&composed(&serial(true), BUILD_YML)).await;
    let stored = rows(&world, run).await;
    let first = id(named(&stored, "fan/build")[0]);
    world
        .finish_job(run, first, finished(Conclusion::Failure))
        .await;
    let stored = rows(&world, run).await;
    assert!(
        stored
            .iter()
            .filter(|job| job.key.starts_with("fan/"))
            .all(|job| status(job) == JobStatus::Completed),
        "{stored:#?}"
    );
    let shown = world.run(run).await;
    assert_eq!(
        (shown.status, shown.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Failure))
    );
}

#[tokio::test]
async fn the_token_of_a_callee_job_holds_no_more_than_the_permissions_of_its_call() {
    let scene = Scene::new().await;
    let caller = "\
on: push
jobs:
  release:
    uses: ./.github/workflows/wide.yml
    permissions:
      statuses: read
      checks: write
";
    let wide = "\
on: workflow_call
jobs:
  use:
    runs-on: [self-hosted, linux]
    permissions: write-all
    steps:
      - run: make
";
    scene
        .start(caller, &[(".github/workflows/wide.yml", wide)])
        .await;
    let claimed = scene.claimed("runner1-1").await;
    assert_eq!(claimed.ceilings.len(), 1);
    let documents = scene.assigned(claimed).await;
    let token = documents.contexts.secrets["GITHUB_TOKEN"]
        .as_str()
        .expect("a token")
        .to_owned();
    let minted = scene
        .fake
        .tokens()
        .into_iter()
        .find(|minted| minted.token == token)
        .expect("the token the fake forge minted");
    assert_eq!(
        minted.permissions,
        BTreeMap::from([
            ("checks".to_owned(), "write".to_owned()),
            ("statuses".to_owned(), "read".to_owned()),
        ])
    );
}

#[tokio::test]
async fn a_push_and_a_schedule_both_read_the_callees_of_their_commit() {
    let mut pushes = super::schedules::Pushes::new("2026-10-25T09:00:00Z").await;
    let caller = "\
on:
  push:
  schedule:
    - cron: '0 3 * * *'
jobs:
  nightly:
    uses: ./.github/workflows/build.yml
";
    pushes
        .push(
            "main",
            &[(".bosun/workflows/ci.yml", caller), (BUILD, BUILD_YML)],
            super::schedules::ROMAN,
        )
        .await;
    let runs = pushes.round("2026-10-26T03:00:00Z").await;
    assert_eq!(runs.len(), 1, "{runs:?}");
    let all = bosun_db::entity::run::Entity::find()
        .order_by_asc(bosun_db::entity::run::Column::Number)
        .all(pushes.world.db())
        .await
        .expect("the runs");
    assert_eq!(
        all.iter().map(|run| run.event.as_str()).collect::<Vec<_>>(),
        ["push", "schedule"]
    );
    for run in all {
        let run = RunId::try_from(run.id).expect("a version 7 id");
        let stored = rows(&pushes.world, run).await;
        let keys: Vec<&str> = stored.iter().map(|job| job.key.as_str()).collect();
        assert_eq!(keys, ["nightly", "nightly/build", "nightly/test"], "{run}");
    }
}

#[derive(Deserialize)]
struct Recorded {
    runs: Vec<RecordedRun>,
}

#[derive(Deserialize)]
struct RecordedRun {
    checks: Vec<String>,
    jobs: Vec<RecordedJob>,
}

#[derive(Deserialize)]
struct RecordedJob {
    name: String,
    #[serde(default)]
    outputs: BTreeMap<String, String>,
}

#[tokio::test]
async fn the_reusable_case_of_the_compatibility_corpus_holds_on_the_server() {
    let case = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../compat/cases/reusable");
    let read = |path: &str| {
        let file = case.join(path);
        fs::read_to_string(&file).unwrap_or_else(|error| panic!("{}: {error}", file.display()))
    };
    let expected: Recorded =
        serde_json::from_str(&read("expected.json")).expect("the expected result");
    let secrets: BTreeMap<String, String> =
        serde_json::from_str(&read("secrets.json")).expect("the secrets of the case");
    let ci = read("commits/02/.bosun/workflows/ci.yml");
    let build = read("commits/02/.bosun/workflows/build.yml");
    let scene = Scene::new().await;
    let (_, in_project) = super::secrets::scopes(&scene.world).await;
    for (name, value) in &secrets {
        secret(&scene.world, in_project, name, value).await;
    }
    let sha = scene.commit(
        KEEL,
        &[
            (".bosun/workflows/ci.yml", &ci),
            (".bosun/workflows/build.yml", &build),
        ],
    );
    let run = scene.launch(&ci, &sha).await;
    let alpha = ["self-hosted", "alpha", "linux", "x64", "shell"];
    let beta = ["self-hosted", "beta", "linux", "x64", "shell"];

    let (agent, claimed) = scene.claimed_by("alpha-1", &alpha).await;
    assert_eq!(claimed.key.as_str(), "prepare");
    scene.ran(&agent, &claimed, &[("version", "1.2.3")]).await;

    let mut documents = BTreeMap::new();
    for number in 2..=3 {
        let (agent, claimed) = scene.claimed_by(&format!("alpha-{number}"), &alpha).await;
        let key = claimed.key.to_string();
        let seen = scene.assigned(claimed.clone()).await;
        let artifact = format!(
            "bundle-{}",
            seen.contexts.inputs["version"].as_str().expect("a version")
        );
        scene
            .ran(&agent, &claimed, &[("artifact", &artifact)])
            .await;
        documents.insert(key, seen);
    }
    let release = &documents["release/build"];
    assert_eq!(
        release.contexts.inputs,
        strings_of(json!({ "version": "1.2.3", "jobs": 4, "verbose": true, "channel": "stable" }))
    );
    assert_eq!(release.evaluated.env["TOKEN"], "case-secret-value");
    assert_eq!(release.evaluated.env["EXTRA"], "");
    assert_eq!(release.evaluated.env["DEPLOY_KEY"], "");
    assert!(!release.evaluated.env["FORGE_TOKEN"].is_empty());
    let inherited = &documents["inherited/build"];
    assert_eq!(
        inherited.contexts.inputs,
        strings_of(json!({ "version": "2.0.0", "jobs": 2, "verbose": false, "channel": "stable" }))
    );
    assert_eq!(inherited.evaluated.env["TOKEN"], "");
    assert_eq!(inherited.evaluated.env["EXTRA"], "");
    assert_eq!(inherited.evaluated.env["DEPLOY_KEY"], "case-secret-value");
    assert!(!inherited.evaluated.env["FORGE_TOKEN"].is_empty());

    let (agent, claimed) = scene.claimed_by("beta-1", &beta).await;
    assert_eq!(claimed.key.as_str(), "report");
    let report = scene.assigned(claimed.clone()).await;
    assert_eq!(
        report.contexts.needs["release"]["outputs"],
        json!({ "artifact": "bundle-1.2.3" })
    );
    assert_eq!(
        report.contexts.needs["inherited"]["outputs"],
        json!({ "artifact": "bundle-2.0.0" })
    );
    scene.ran(&agent, &claimed, &[]).await;

    while outbox::next(&scene.outbox)
        .await
        .unwrap_or_else(|error| panic!("the outbox failed: {error}"))
    {}
    let [recorded] = expected.runs.as_slice() else {
        panic!("the case expects one run");
    };
    let mut checks: Vec<String> = scene
        .fake
        .check_runs()
        .into_iter()
        .map(|check| check.name)
        .collect();
    checks.sort();
    assert_eq!(checks, recorded.checks);
    let stored = scene.rows(run).await;
    for job in &recorded.jobs {
        let found = stored
            .iter()
            .find(|stored| stored.name == job.name)
            .unwrap_or_else(|| panic!("no job named {} in {stored:#?}", job.name));
        assert_eq!(found.conclusion.as_deref(), Some("success"), "{}", job.name);
        for (name, value) in &job.outputs {
            assert_eq!(found.outputs[name], json!(value), "{}", job.name);
        }
    }
    let shown = scene.world.run(run).await;
    assert_eq!(shown.conclusion, Some(RunConclusion::Success));
}

fn strings_of(value: serde_json::Value) -> Map<String, serde_json::Value> {
    match value {
        serde_json::Value::Object(fields) => fields,
        other => panic!("{other} is not an object"),
    }
}

const LOWER_CALLER: &str = "\
on: push
jobs:
  lower:
    uses: globalart/lower/.github/workflows/lower.yml@v1
    secrets: inherit
";

const FOREIGN_INHERIT: &str = "\
on: push
jobs:
  foreign:
    uses: ThirdParty/lib/.github/workflows/lib.yml@v1
    secrets: inherit
";

const FOREIGN_NAMED: &str = "\
on: push
jobs:
  foreign:
    uses: ThirdParty/lib/.github/workflows/lib.yml@v1
    secrets:
      token: ${{ secrets.DEPLOY }}
";

const LIB_YML: &str = "\
on:
  workflow_call:
    secrets:
      token:
jobs:
  only:
    runs-on: self-hosted
    steps:
      - run: make
";

#[tokio::test]
async fn secrets_inherit_reaches_only_a_callee_of_the_account_of_the_calling_file() {
    let scene = Scene::new().await;
    let lib = ".github/workflows/lib.yml";

    scene
        .fake
        .create_repository(scene.installation, "globalart/lower");
    scene.release(
        "globalart/lower",
        ".github/workflows/lower.yml",
        &release("only"),
    );
    let run = scene.start(LOWER_CALLER, &[]).await;
    let stored = scene.rows(run).await;
    assert_eq!(
        stored
            .iter()
            .map(|job| job.key.as_str())
            .collect::<Vec<_>>(),
        ["lower", "lower/only"],
        "the owner differs from the owner of the calling file only by case"
    );

    scene.release(LIB, lib, LIB_YML);
    let run = scene.start(FOREIGN_INHERIT, &[]).await;
    let notices = scene.failed(run).await;
    let refusal = notices
        .iter()
        .find(|notice| notice.code == "callee_forbidden")
        .unwrap_or_else(|| panic!("no refusal in {notices:#?}"));
    assert!(
        refusal
            .message
            .contains("`secrets: inherit` is allowed only"),
        "{}",
        refusal.message
    );
    assert!(
        scene
            .fake
            .requests()
            .iter()
            .all(|request| !request.path.contains("ThirdParty")),
        "a refused call made a request for its callee"
    );

    scene.fake.publish(LIB);
    let run = scene.start(FOREIGN_NAMED, &[]).await;
    let stored = scene.rows(run).await;
    assert_eq!(
        stored
            .iter()
            .map(|job| job.key.as_str())
            .collect::<Vec<_>>(),
        ["foreign", "foreign/only"],
        "named secrets go to a callee of another account"
    );
}

#[tokio::test]
async fn a_private_repository_outside_the_installation_is_a_missing_callee_and_a_public_one_is_read()
 {
    let scene = Scene::new().await;
    scene.release(LIB, ".github/workflows/lib.yml", LIB_YML);
    let caller = FOREIGN_NAMED;
    let run = scene.start(caller, &[]).await;
    let notices = scene.failed(run).await;
    let refusal = notices
        .iter()
        .find(|notice| notice.code == "callee_missing")
        .unwrap_or_else(|| panic!("no refusal in {notices:#?}"));
    assert!(
        refusal
            .message
            .contains("a repository outside the installation must be public"),
        "{}",
        refusal.message
    );

    scene.fake.publish(LIB);
    let run = scene.start(caller, &[]).await;
    assert_eq!(scene.world.run(run).await.status, RunStatus::Queued);
    assert_eq!(scene.rows(run).await.len(), 2);
}

const MIDDLE_YML: &str = "\
on:
  workflow_call:
    secrets:
      a:
jobs:
  next:
    uses: ./.github/workflows/leaf.yml
    secrets: inherit
";

const LEAF_YML: &str = "\
on:
  workflow_call:
    secrets:
      a:
      b:
jobs:
  use:
    runs-on: [self-hosted, linux]
    env:
      A: ${{ secrets.a }}
      B: ${{ secrets.b }}
    steps:
      - run: make
";

const TOP_YML: &str = "\
on: push
jobs:
  top:
    uses: ./.github/workflows/middle.yml
    secrets:
      a: ${{ secrets.A }}
";

#[tokio::test]
async fn a_secret_the_middle_workflow_did_not_receive_never_reaches_the_leaf() {
    let scene = Scene::new().await;
    let (_, in_project) = super::secrets::scopes(&scene.world).await;
    secret(&scene.world, in_project, "A", "a-value").await;
    secret(&scene.world, in_project, "B", "b-value").await;
    scene
        .start(
            TOP_YML,
            &[
                (".github/workflows/middle.yml", MIDDLE_YML),
                (".github/workflows/leaf.yml", LEAF_YML),
            ],
        )
        .await;
    let claimed = scene.claimed("runner1-1").await;
    assert_eq!(claimed.key.as_str(), "top/next/use");
    let documents = scene.assigned(claimed).await;
    assert_eq!(
        documents.evaluated.env,
        [
            ("A".to_owned(), "a-value".to_owned()),
            ("B".to_owned(), String::new()),
        ]
        .into()
    );
    assert!(documents.contexts.secrets.contains_key("GITHUB_TOKEN"));
    assert_eq!(documents.contexts.secrets["B"], json!(""));
    assert_eq!(documents.contexts.secrets["A"], json!("a-value"));
}

#[tokio::test]
async fn the_jobs_of_a_callee_in_a_run_from_a_fork_read_no_secrets() {
    let scene = Scene::new().await;
    let (_, in_project) = super::secrets::scopes(&scene.world).await;
    secret(&scene.world, in_project, "DEPLOY", "deploy-value").await;
    let forks = pool(
        &scene.world,
        "forks",
        TrustLevel::Untrusted,
        &[Access::AllProjects],
    )
    .await;
    scene
        .start_as(
            SECRETS_CALLER,
            &[(".github/workflows/secrets.yml", SECRETS_YML)],
            Trust::Untrusted { admitted: true },
        )
        .await;
    let agent = connected_in(
        &scene.world,
        forks,
        "runner2-1",
        &LABELS,
        &[Capability::Shell],
        scene.replica,
    )
    .await;
    let claimed = claim(scene.world.db(), agent.id, scene.replica, LEASE, &[])
        .await
        .expect("a job of the run from a fork");
    let documents = scene.assigned(claimed).await;
    assert!(
        documents.evaluated.env.values().all(String::is_empty),
        "{:?}",
        documents.evaluated.env
    );
}

#[tokio::test]
async fn a_secret_inside_other_text_in_the_secrets_of_a_call_is_invalid_value() {
    let world = World::new().await;
    let caller = "\
on: push
jobs:
  explicit:
    uses: ./.github/workflows/build.yml
    secrets:
      token: ${{ secrets.A }}-x
";
    let run = world.dispatch(&composed(caller, BUILD_YML)).await;
    let stored = rows(&world, run).await;
    let explicit = one(&stored, "explicit");
    assert_eq!(
        (status(explicit), explicit.conclusion.as_deref()),
        (JobStatus::Completed, Some("failure"))
    );
    let notices: Vec<Diagnostic> = world
        .run(run)
        .await
        .notices
        .into_iter()
        .map(|notice| notice.diagnostic)
        .collect();
    let refusal = notices
        .iter()
        .find(|notice| notice.code == "invalid_value")
        .unwrap_or_else(|| panic!("no refusal in {notices:#?}"));
    assert!(
        refusal.message.contains(
            "passes a secret inside other text or an expression, and a call passes a secret of \
             its caller only as `${{ secrets.NAME }}` alone"
        ),
        "{}",
        refusal.message
    );
}

const OUTER_YML: &str = "\
on: push
jobs:
  outer:
    name: Outer
    uses: ./.github/workflows/mid.yml
";

const MID_YML: &str = "\
on: workflow_call
jobs:
  inner:
    name: Inner
    uses: ./.github/workflows/leaf.yml
";

const DEEP_YML: &str = "\
on: workflow_call
jobs:
  job:
    name: Job
    runs-on: self-hosted
    steps:
      - run: make
  last:
    needs: job
    runs-on: self-hosted
    steps:
      - run: make
";

const TREE: [(&str, &str); 2] = [
    (".github/workflows/mid.yml", MID_YML),
    (".github/workflows/leaf.yml", DEEP_YML),
];

#[tokio::test]
async fn a_job_of_a_nested_call_is_reported_under_the_names_of_every_caller_above_it() {
    let scene = Scene::new().await;
    let run = scene.start(OUTER_YML, &TREE).await;
    let stored = scene.rows(run).await;
    assert_eq!(
        stored
            .iter()
            .map(|job| (job.key.as_str(), job.name.as_str()))
            .collect::<Vec<_>>(),
        [
            ("outer", "Outer"),
            ("outer/inner", "Outer / Inner"),
            ("outer/inner/job", "Outer / Inner / Job"),
            ("outer/inner/last", "Outer / Inner / last"),
        ]
    );
    while outbox::next(&scene.outbox)
        .await
        .unwrap_or_else(|error| panic!("the outbox failed: {error}"))
    {}
    let mut names: Vec<String> = scene
        .fake
        .check_runs()
        .into_iter()
        .map(|check| check.name)
        .collect();
    names.sort();
    assert_eq!(names, ["Outer / Inner / Job"]);
}

#[tokio::test]
async fn cancelling_a_run_cancels_the_jobs_of_its_nested_calls() {
    let scene = Scene::new().await;
    let run = scene.start(OUTER_YML, &TREE).await;
    let stored = scene.rows(run).await;
    let running = scene
        .world
        .start(run, id(one(&stored, "outer/inner/job")))
        .await;
    scene.world.command(run, &run_cancel()).await;
    let stored = scene.rows(run).await;
    assert_eq!(
        (
            status(one(&stored, "outer/inner/last")),
            one(&stored, "outer/inner/last").conclusion.as_deref()
        ),
        (JobStatus::Completed, Some("cancelled"))
    );
    assert_eq!(scene.world.controls().await, [running].into());
    scene
        .world
        .end(running, finished(Conclusion::Cancelled))
        .await;
    let stored = scene.rows(run).await;
    for key in [
        "outer",
        "outer/inner",
        "outer/inner/job",
        "outer/inner/last",
    ] {
        let job = one(&stored, key);
        assert_eq!(
            (status(job), job.conclusion.as_deref()),
            (JobStatus::Completed, Some("cancelled")),
            "{key}"
        );
    }
    let shown = scene.world.run(run).await;
    assert_eq!(
        (shown.status, shown.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Cancelled))
    );
}

fn jobs_of(count: usize) -> String {
    let mut source = "on: workflow_call\njobs:\n".to_owned();
    for number in 1..=count {
        source.push_str(&format!(
            "  j{number}:\n    runs-on: self-hosted\n    steps:\n      - run: make\n"
        ));
    }
    source
}

fn calls_of(count: usize) -> String {
    let mut source = "on: workflow_call\njobs:\n".to_owned();
    for number in 1..=count {
        source.push_str(&format!(
            "  c{number}:\n    uses: ./.github/workflows/leaf.yml\n"
        ));
    }
    source
}

fn fans(count: usize, width: usize) -> String {
    let axis: Vec<String> = (1..=width).map(|number| number.to_string()).collect();
    let mut source = "on: push\njobs:\n".to_owned();
    for number in 1..=count {
        source.push_str(&format!(
            "  fan{number}:\n    strategy:\n      matrix:\n        n: [{}]\n    uses: ./.github/workflows/build.yml\n",
            axis.join(", ")
        ));
    }
    source
}

#[tokio::test]
async fn a_chain_whose_calls_bring_more_than_500_jobs_ends_the_run_with_startup_failure() {
    let scene = Scene::new().await;
    let caller = "on: push\njobs:\n  top:\n    uses: ./.github/workflows/middle.yml\n";
    let leaf = jobs_of(4);
    let at_limit = calls_of(100);
    let files = [
        (".github/workflows/leaf.yml", leaf.as_str()),
        (".github/workflows/middle.yml", at_limit.as_str()),
    ];
    assert!(matches!(
        scene.loaded(caller, &files).await,
        callees::Loaded::Ready(..)
    ));
    let over = calls_of(101);
    let files = [
        (".github/workflows/leaf.yml", leaf.as_str()),
        (".github/workflows/middle.yml", over.as_str()),
    ];
    let callees::Loaded::Refused(notices) = scene.loaded(caller, &files).await else {
        panic!("a chain of 505 jobs was accepted");
    };
    let refusal = notices
        .iter()
        .find(|notice| notice.code == "limit_exceeded")
        .unwrap_or_else(|| panic!("no limit in {notices:#?}"));
    assert_eq!(refusal.file, "ci");
    assert_eq!(refusal.span.line, 4);
    assert!(
        refusal.message.contains("505 jobs") && refusal.message.contains("at most 500"),
        "{}",
        refusal.message
    );
}

#[tokio::test]
async fn a_matrix_on_a_call_that_would_cross_500_jobs_at_run_time_fails_the_call() {
    let world = World::new().await;
    let callee = release("only");
    let notices = |run: RunId| {
        let world = &world;
        async move {
            world
                .run(run)
                .await
                .notices
                .into_iter()
                .map(|notice| notice.diagnostic)
                .filter(|notice| notice.code == "limit_exceeded")
                .collect::<Vec<_>>()
        }
    };

    let run = world.dispatch(&composed(&fans(1, 250), &callee)).await;
    assert!(notices(run).await.is_empty());
    assert_eq!(named(&rows(&world, run).await, "fan1/only").len(), 250);
    assert_eq!(world.run(run).await.conclusion, None);

    let run = world.dispatch(&composed(&fans(1, 251), &callee)).await;
    let found = notices(run).await;
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(
        found[0].message.contains("would add 1 jobs to the 500")
            && found[0].message.contains("at most 500"),
        "{}",
        found[0].message
    );
    let shown = world.run(run).await;
    assert_eq!(
        (shown.status, shown.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Failure))
    );

    let run = world.dispatch(&composed(&fans(2, 256), &callee)).await;
    let found = notices(run).await;
    assert!(
        found
            .iter()
            .any(|notice| notice.message.contains("would add 256 jobs to the 256")),
        "{found:#?}"
    );
    let shown = world.run(run).await;
    assert_eq!(
        (shown.status, shown.conclusion),
        (RunStatus::Completed, Some(RunConclusion::Failure))
    );
}

#[tokio::test]
async fn concurrency_of_a_call_and_of_a_callee_workflow_is_a_warning_once_at_planning_and_a_parse_warning_of_a_callee_is_kept()
 {
    let scene = Scene::new().await;
    let caller = "\
on: push
jobs:
  fan:
    strategy:
      matrix:
        n: [1, 2]
    concurrency: first
    uses: ./.github/workflows/build.yml
  skipped:
    if: false
    concurrency: second
    uses: ./.github/workflows/gated.yml
";
    let gated = "\
on: workflow_call
concurrency: shared
jobs:
  only:
    runs-on: self-hosted
    env:
      ALL: ${{ toJSON(secrets) }}
    steps:
      - run: make
";
    let run = scene
        .start(
            caller,
            &[(BUILD, BUILD_YML), (".github/workflows/gated.yml", gated)],
        )
        .await;
    let notices = scene.notices(run).await;
    let gated_file = ".github/workflows/gated.yml";
    let codes: Vec<(&str, &str)> = notices
        .iter()
        .map(|notice| (notice.code.as_str(), notice.file.as_str()))
        .collect();
    assert_eq!(
        codes
            .iter()
            .filter(|(code, _)| *code == "not_implemented")
            .count(),
        3,
        "{notices:#?}"
    );
    for (message, file) in [
        ("the `concurrency` of job `fan`", "ci"),
        ("the `concurrency` of job `skipped`", "ci"),
        ("the `concurrency` of the reusable workflow", gated_file),
    ] {
        assert!(
            notices.iter().any(|notice| notice.code == "not_implemented"
                && notice.file == file
                && notice.message.contains(message)),
            "no warning `{message}` in {notices:#?}"
        );
    }
    assert!(codes.contains(&("all_secrets", gated_file)), "{notices:#?}");
}
