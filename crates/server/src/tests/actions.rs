use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::header::AUTHORIZATION;
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use bosun_blob::{Key, S3, Store};
use bosun_db::agents::{Connection, NewAgent, NewRegistrationToken, Registration};
use bosun_db::installations::Account;
use bosun_db::queue::{Claim, Claimed, ClaimedJob};
use bosun_forge::GitHubApp;
use bosun_proto::agent::v1::server_message::Message;
use bosun_proto::assignment::Documents;
use bosun_testkit::TestBucket;
use bosun_testkit::forge::{APP_ID, APP_KEY, FakeForge};
use bosun_types::api::{Capability, Notice};
use bosun_types::diagnostic::{PathSegment, Severity};
use bosun_types::id::{ExecutionId, RunId};
use bosun_types::ir::{ActionReference, Runs};
use bosun_types::status::{Conclusion, JobStatus, StatusReason};
use chrono::{TimeDelta, Utc};
use sea_orm::TransactionTrait;
use sea_orm::prelude::Uuid;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tower::ServiceExt;
use url::Url;

use super::{NODE, World, finished, keys, site, tools_settings};
use crate::actions::Actions;
use crate::agents::progress::{self, Progress, Reporter};
use crate::clock::Clock;
use crate::forge::Forge;
use crate::store::Backend;
use crate::token::{AGENT, Token};
use crate::tools::Tools;
use crate::{AppState, config, queue, runtime, sign_in};

const READ: &[(&str, &str)] = &[("contents", "read"), ("metadata", "read")];

const LABELS: [&str; 4] = ["self-hosted", "linux", "x64", "shell"];

const LEASE: Duration = Duration::from_secs(30);

const CHECKOUT: &str = "name: Checkout\ninputs:\n  fetch-depth:\n    default: 1\nruns:\n  using: \
                        node20\n  main: dist/index.js\n  post: dist/index.js\n";

const NODE16: &str = "name: Legacy\nruns:\n  using: node16\n  main: index.js\n";

const NODE24: &str = "name: Current\nruns:\n  using: node24\n  main: index.js\n";

const COMPOSITE: &str =
    "name: Composite\nruns:\n  using: composite\n  steps:\n    - run: echo\n      shell: bash\n";

struct Harness {
    world: World,
    forge: FakeForge,
    state: AppState,
    actions: Actions,
    replica: Uuid,
    directory: TempDir,
}

struct Agent {
    reporter: Reporter,
    credential: String,
}

impl Harness {
    async fn new() -> Harness {
        let harness = Harness::without_installation().await;
        let installation = harness.forge.install("globalart", READ);
        let project = bosun_db::projects::find(harness.db(), harness.world.workflow.project_id)
            .await
            .expect("a project")
            .expect("the project of the world");
        let transaction = harness.db().begin().await.expect("a transaction");
        bosun_db::installations::record(
            &transaction,
            installation,
            &Account {
                id: 9_001,
                login: "globalart",
            },
        )
        .await
        .expect("an installation");
        bosun_db::installations::map(&transaction, installation, project.organisation_id)
            .await
            .expect("a mapped installation");
        transaction.commit().await.expect("a commit");
        harness
    }

    async fn without_installation() -> Harness {
        let world = World::new().await;
        let forge = FakeForge::start().await;
        let directory = tempfile::tempdir().expect("a directory");
        let state = super::state(
            &world.database,
            directory.path(),
            Clock::system(),
            sign_in::Settings::new(Vec::new(), 1000, directory.path().join("data")),
        )
        .await;
        let actions = replica(&world, &forge, &state);
        Harness {
            world,
            forge,
            state,
            actions,
            replica: Uuid::now_v7(),
            directory,
        }
    }

    fn db(&self) -> &sea_orm::DatabaseConnection {
        self.world.db()
    }

    fn replica(&self) -> Actions {
        replica(&self.world, &self.forge, &self.state)
    }

    fn action(&self, full_name: &str, public: bool, files: &[(&str, &str)]) -> String {
        let owner = full_name.split_once('/').expect("an owner").0;
        let installation = self.forge.install(owner, READ);
        self.forge.create_repository(installation, full_name);
        if public {
            self.forge.publish(full_name);
        }
        self.commit(full_name, files)
    }

    fn commit(&self, full_name: &str, files: &[(&str, &str)]) -> String {
        let mut tree = vec![("dist/index.js", "console.log('run')\n")];
        tree.extend_from_slice(files);
        self.forge.commit(
            full_name,
            "main",
            tree.iter().map(|(path, text)| (*path, *text)),
        )
    }

    fn tag(&self, full_name: &str, tag: &str, commit: &str) {
        self.forge.tag(full_name, tag, commit);
    }

    fn downloads(&self, full_name: &str) -> Vec<String> {
        self.forge
            .downloads()
            .into_iter()
            .filter(|download| download.repository == full_name)
            .map(|download| download.commit)
            .collect()
    }

    async fn dispatch(&self, steps: &str) -> RunId {
        let source = format!(
            "on: workflow_dispatch\njobs:\n  build:\n    runs-on: self-hosted\n    steps:\n{steps}"
        );
        let workflow = bosun_workflow::parse("ci", &source)
            .unwrap_or_else(|diagnostics| panic!("{source}\n{diagnostics:#?}"))
            .workflow;
        self.world.dispatch(&workflow).await
    }

    async fn agent(&self, name: &str) -> Agent {
        let db = self.db();
        let pool = bosun_db::pools::default_pool(db)
            .await
            .expect("the default pool");
        let registration = Uuid::now_v7().as_bytes().repeat(2);
        bosun_db::agents::create_registration_token(
            db,
            NewRegistrationToken {
                pool_id: pool.id,
                sha256: registration.clone(),
                reusable: false,
                expires_at: Utc::now() + TimeDelta::hours(1),
            },
        )
        .await
        .expect("a registration token");
        let credential = Token::generate(AGENT).expect("a credential");
        let registered = bosun_db::agents::register(
            db,
            &registration,
            Utc::now(),
            NewAgent {
                name: name.to_owned(),
                labels: Vec::new(),
                credential_sha256: credential.sha256(),
            },
        )
        .await
        .expect("a registration");
        let Registration::Registered(registered) = registered else {
            panic!("the agent was not registered");
        };
        bosun_db::agents::connect(
            db,
            registered.id,
            Connection {
                replica_id: self.replica,
                version: "0.1.0".to_owned(),
                labels: LABELS.iter().map(|label| (*label).to_owned()).collect(),
                capabilities: vec![Capability::Shell],
            },
        )
        .await
        .expect("the agent connected");
        Agent {
            reporter: Reporter {
                id: registered.id,
                name: registered.name,
            },
            credential: credential.into_string(),
        }
    }

    async fn claim(&self, agent: &Agent) -> ClaimedJob {
        loop {
            let claimed = bosun_db::queue::claim(
                self.db(),
                Claim {
                    agent: agent.reporter.id,
                    replica_id: self.replica,
                    lease: LEASE,
                    passed_over: &[],
                },
            )
            .await
            .expect("a claim");
            match claimed {
                Claimed::Job(claimed) => return *claimed,
                Claimed::Nothing => panic!("agent {} found no job", agent.reporter.name),
                Claimed::Contended => tokio::task::yield_now().await,
            }
        }
    }

    async fn assign(&self, claimed: ClaimedJob) -> Option<Documents> {
        self.assign_through(&self.actions, claimed).await
    }

    async fn assign_through(&self, actions: &Actions, claimed: ClaimedJob) -> Option<Documents> {
        let offer = queue::assignment(self.db(), claimed, &site(), &keys(), actions)
            .await
            .expect("an assignment")?;
        let Some(Message::Assignment(assignment)) = offer.message.message else {
            panic!("the offer holds no assignment");
        };
        Some(Documents::read(&assignment).expect("documents the agent reads"))
    }

    async fn assigned(&self, agent: &Agent) -> (ExecutionId, Documents) {
        let claimed = self.claim(agent).await;
        let execution = claimed.execution_id;
        let documents = self
            .assign(claimed)
            .await
            .expect("an assignment that reaches the agent");
        (execution, documents)
    }

    async fn get(&self, path: &str, credential: Option<&str>) -> (StatusCode, Vec<u8>) {
        let router = runtime::routes().with_state(self.state.clone());
        request(router, path, credential).await
    }

    async fn notices(&self, run: RunId) -> Vec<Notice> {
        self.world.run(run).await.notices
    }
}

fn replica(world: &World, forge: &FakeForge, state: &AppState) -> Actions {
    let app =
        GitHubApp::new(APP_ID, APP_KEY.as_bytes(), &forge.api_url()).expect("the App's client");
    Actions::new(
        world.db().clone(),
        Arc::clone(&state.store),
        Some(Arc::new(Forge::of(app))),
        super::runtimes(),
    )
}

fn serving(state: &AppState, store: Arc<Backend>, settings: config::Tools) -> AppState {
    let mut state = state.clone();
    state.tools =
        Arc::new(Tools::new(state.db.clone(), Arc::clone(&store), settings).expect("the tools"));
    state.store = store;
    state
}

async fn request(router: Router, path: &str, credential: Option<&str>) -> (StatusCode, Vec<u8>) {
    let mut builder = Request::builder().uri(path);
    if let Some(credential) = credential {
        builder = builder.header(AUTHORIZATION, format!("Bearer {credential}"));
    }
    let response = router
        .oneshot(builder.body(Body::empty()).expect("a request"))
        .await
        .expect("an answer");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("a body");
    (status, body.to_vec())
}

fn message(body: &[u8]) -> String {
    let error: serde_json::Value = serde_json::from_slice(body).expect("an error envelope");
    error["error"]["message"]
        .as_str()
        .expect("a message")
        .to_owned()
}

fn uses(owner: &str, repository: &str, path: Option<&str>, r#ref: &str) -> ActionReference {
    ActionReference {
        owner: owner.to_owned(),
        repository: repository.to_owned(),
        path: path.map(str::to_owned),
        r#ref: r#ref.to_owned(),
    }
}

fn at(steps: u64) -> Vec<PathSegment> {
    ["jobs", "build", "steps"]
        .into_iter()
        .map(|segment| PathSegment::Key(segment.to_owned()))
        .chain([
            PathSegment::Index(steps),
            PathSegment::Key("uses".to_owned()),
        ])
        .collect()
}

#[tokio::test]
async fn two_assignments_of_an_action_download_it_once_and_a_moved_tag_leaves_the_job_its_commit() {
    let harness = Harness::new().await;
    let first = harness.action("actions/checkout", true, &[("action.yml", CHECKOUT)]);
    harness.tag("actions/checkout", "v4", &first);
    let steps = "      - uses: actions/checkout@v4\n      - run: make\n";
    let early = harness.dispatch(steps).await;
    let second = harness.dispatch(steps).await;
    let runner = harness.agent("runner1-1").await;
    let other = harness.agent("runner1-2").await;

    let (rejected, documents) = harness.assigned(&runner).await;
    assert_eq!(documents.actions.len(), 1, "{:#?}", documents.actions);
    let action = &documents.actions[0];
    assert_eq!(action.uses, uses("actions", "checkout", None, "v4"));
    assert_eq!(action.commit, first);
    assert_eq!(
        action.download,
        format!("/_runtime/actions/actions/checkout/{first}.tar.gz")
    );
    assert_eq!(action.node, NODE);
    let Runs::Node(runs) = &action.metadata.runs;
    assert_eq!(runs.main, "dist/index.js");
    assert_eq!(action.metadata.name.as_deref(), Some("Checkout"));
    assert_eq!(
        harness.downloads("actions/checkout"),
        std::slice::from_ref(&first)
    );

    let claimed = harness.claim(&other).await;
    assert_eq!(claimed.run_id, second);
    let documents = harness
        .assign(claimed)
        .await
        .expect("the second assignment");
    assert_eq!(documents.actions[0].commit, first);
    assert_eq!(
        harness.downloads("actions/checkout"),
        std::slice::from_ref(&first)
    );

    let moved = harness.commit(
        "actions/checkout",
        &[("action.yml", &CHECKOUT.replace("Checkout", "Moved"))],
    );
    harness.tag("actions/checkout", "v4", &moved);
    assert_eq!(
        progress::rejected(harness.db(), &runner.reporter, rejected)
            .await
            .expect("a rejection"),
        Progress::Applied
    );
    let again = harness.claim(&runner).await;
    assert_eq!(again.run_id, early);
    let documents = harness.assign(again).await.expect("the assignment again");
    assert_eq!(documents.actions[0].commit, first);
    assert_eq!(
        documents.actions[0].metadata.name.as_deref(),
        Some("Checkout")
    );
    assert_eq!(
        harness.downloads("actions/checkout"),
        std::slice::from_ref(&first)
    );

    let later = harness.dispatch(steps).await;
    let third = harness.agent("runner1-3").await;
    let claimed = harness.claim(&third).await;
    assert_eq!(claimed.run_id, later);
    let documents = harness
        .assign(claimed)
        .await
        .expect("an assignment after the move");
    assert_eq!(documents.actions[0].commit, moved);
    assert_eq!(documents.actions[0].metadata.name.as_deref(), Some("Moved"));
    assert_eq!(harness.downloads("actions/checkout"), [first, moved]);
}

#[tokio::test]
async fn two_replicas_that_assign_the_same_new_action_at_once_download_it_once() {
    let harness = Harness::new().await;
    let commit = harness.action("actions/checkout", true, &[("action.yml", CHECKOUT)]);
    harness.tag("actions/checkout", "v4", &commit);
    let steps = "      - uses: actions/checkout@v4\n";
    harness.dispatch(steps).await;
    harness.dispatch(steps).await;
    let first = harness.agent("runner1-1").await;
    let second = harness.agent("runner2-1").await;
    let (one, two) = (harness.claim(&first).await, harness.claim(&second).await);
    let other = harness.replica();
    let (one, two) = tokio::join!(
        harness.assign_through(&harness.actions, one),
        harness.assign_through(&other, two),
    );
    for documents in [one, two] {
        let documents = documents.expect("an assignment that reaches its agent");
        assert_eq!(documents.actions.len(), 1, "{:#?}", documents.actions);
        assert_eq!(documents.actions[0].commit, commit);
        assert_eq!(
            documents.actions[0].metadata.name.as_deref(),
            Some("Checkout")
        );
    }
    assert_eq!(
        harness.downloads("actions/checkout"),
        std::slice::from_ref(&commit)
    );
}

#[tokio::test]
async fn an_agent_downloads_only_the_actions_of_an_execution_it_holds() {
    let harness = Harness::new().await;
    let commit = harness.action("actions/checkout", true, &[("action.yml", CHECKOUT)]);
    harness.tag("actions/checkout", "v4", &commit);
    harness
        .dispatch("      - uses: actions/checkout@v4\n")
        .await;
    let holder = harness.agent("runner1-1").await;
    let stranger = harness.agent("runner1-2").await;
    let (execution, documents) = harness.assigned(&holder).await;
    let path = documents.actions[0].download.clone();

    let (status, body) = harness.get(&path, Some(&holder.credential)).await;
    assert_eq!(status, StatusCode::OK);
    let stored = bosun_db::actions::find(harness.db(), "actions", "checkout", &commit)
        .await
        .expect("a query")
        .expect("the stored action");
    assert_eq!(Sha256::digest(&body).to_vec(), stored.sha256);
    assert_eq!(body.len() as u64, stored.size);
    assert_eq!(
        stored.metadata.keys().collect::<Vec<_>>(),
        ["action.yml"],
        "the metadata files of the tarball"
    );

    let (status, body) = harness.get(&path, Some(&stranger.credential)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        message(&body),
        format!(
            "agent `runner1-2` holds no execution that uses the action actions/checkout at \
             {commit}"
        )
    );
    for (credential, expected) in [
        (None, "the request carries no agent credential"),
        (
            Some("bst_personal"),
            "the bearer token is not a well-formed agent credential",
        ),
        (
            Some(Token::generate(AGENT).expect("a credential").as_str()),
            "the agent credential is not known",
        ),
    ] {
        let (status, body) = harness.get(&path, credential).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{credential:?}");
        assert_eq!(message(&body), expected);
    }

    progress::accepted(harness.db(), &holder.reporter, execution, &site())
        .await
        .expect("an acceptance");
    let (status, _) = harness.get(&path, Some(&holder.credential)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a job in progress keeps its actions"
    );
    progress::job_finished(
        harness.db(),
        &holder.reporter,
        execution,
        finished(Conclusion::Success),
        &site(),
    )
    .await
    .expect("a finished job");
    let (status, _) = harness.get(&path, Some(&holder.credential)).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a finished job holds nothing"
    );
}

#[tokio::test]
async fn an_unseen_action_a_composite_action_and_oversized_metadata_fail_the_job_before_an_agent() {
    let harness = Harness::new().await;
    let private = harness.action("other/private", false, &[("action.yml", NODE24)]);
    harness.tag("other/private", "v1", &private);
    let composite = harness.action("acme/composite", true, &[("action.yml", COMPOSITE)]);
    harness.tag("acme/composite", "v1", &composite);
    let oversized = format!("{NODE24}{}", " ".repeat(1 << 20));
    let huge = harness.action("acme/huge", true, &[("action.yml", oversized.as_str())]);
    harness.tag("acme/huge", "v1", &huge);
    let runner = harness.agent("runner1-1").await;
    for (steps, code, expected) in [
        (
            "      - run: make\n      - uses: other/private@v1\n",
            "action_unresolved",
            "the action `other/private@v1` cannot be resolved: the installation of the GitHub \
             App of organisation `globalart` sees no repository `other/private` with a ref `v1`"
                .to_owned(),
        ),
        (
            "      - run: make\n      - uses: acme/composite@v1\n",
            "not_implemented",
            "the action `acme/composite@v1` cannot run: `using: composite` in `runs` is not \
             supported yet; it arrives in M2 (action.yml 3:10)"
                .to_owned(),
        ),
        (
            "      - run: make\n      - uses: acme/huge@v1\n",
            "action_invalid",
            format!(
                "the metadata action.yml of the action `acme/huge@v1` at {huge} is {} bytes, more \
                 than the 1 MiB the server reads",
                oversized.len()
            ),
        ),
    ] {
        let run = harness.dispatch(steps).await;
        let claimed = harness.claim(&runner).await;
        assert_eq!(claimed.run_id, run);
        let execution = claimed.execution_id;
        assert!(
            harness.assign(claimed).await.is_none(),
            "a job with {expected} reached an agent"
        );
        let projection = harness.world.project(run).await;
        let (_, job) = projection.job("build");
        assert_eq!(job.status, JobStatus::Completed);
        assert_eq!(job.conclusion, Some(Conclusion::Failure));
        let execution = &projection.executions[&execution];
        assert_eq!(execution.reason, Some(StatusReason::ActionUnavailable));
        let notices = harness.notices(run).await;
        assert_eq!(notices.len(), 1, "{notices:#?}");
        let notice = &notices[0].diagnostic;
        assert_eq!(
            (
                notice.severity,
                notice.code.as_str(),
                notice.message.as_str()
            ),
            (Severity::Error, code, expected.as_str())
        );
        assert_eq!(notice.file, "ci");
        assert_eq!(notice.path, at(1));
        assert_eq!(notices[0].job_id, Some(projection.job("build").0));
    }
    assert!(harness.downloads("other/private").is_empty());
}

#[tokio::test]
async fn an_action_declared_node16_is_assigned_node_24_and_the_run_notices_say_so_once() {
    let harness = Harness::new().await;
    let legacy = harness.action("acme/legacy", true, &[("action.yml", NODE16)]);
    harness.tag("acme/legacy", "v1", &legacy);
    let tools = harness.action("acme/tools", true, &[("setup/action.yaml", NODE24)]);
    harness.tag("acme/tools", "v2", &tools);
    let run = harness
        .dispatch("      - uses: acme/legacy@v1\n      - uses: acme/tools/setup@v2\n")
        .await;
    let runner = harness.agent("runner1-1").await;
    let (execution, documents) = harness.assigned(&runner).await;
    let assigned: Vec<(ActionReference, String)> = documents
        .actions
        .iter()
        .map(|action| (action.uses.clone(), action.node.clone()))
        .collect();
    assert_eq!(
        assigned,
        [
            (uses("acme", "legacy", None, "v1"), NODE.to_owned()),
            (uses("acme", "tools", Some("setup"), "v2"), NODE.to_owned()),
        ]
    );
    assert_eq!(
        documents.actions[1].metadata.name.as_deref(),
        Some("Current")
    );
    let expected = (
        Severity::Warning,
        "node_substituted".to_owned(),
        format!(
            "the action `acme/legacy@v1` declares `node16`, which this server runs on Node {NODE}"
        ),
        at(0),
    );
    let noticed = |notices: Vec<Notice>| -> Vec<_> {
        notices
            .into_iter()
            .map(|notice| {
                (
                    notice.diagnostic.severity,
                    notice.diagnostic.code,
                    notice.diagnostic.message,
                    notice.diagnostic.path,
                )
            })
            .collect()
    };
    assert_eq!(
        noticed(harness.notices(run).await),
        std::slice::from_ref(&expected)
    );

    progress::rejected(harness.db(), &runner.reporter, execution)
        .await
        .expect("a rejection");
    let (_, documents) = harness.assigned(&runner).await;
    assert_eq!(documents.actions[0].node, NODE);
    assert_eq!(noticed(harness.notices(run).await), [expected]);
}

#[tokio::test]
async fn a_public_action_resolves_without_an_installation_and_a_private_one_names_the_reason() {
    let harness = Harness::without_installation().await;
    let public = harness.action("actions/checkout", true, &[("action.yml", CHECKOUT)]);
    harness.tag("actions/checkout", "v4", &public);
    let private = harness.action("other/private", false, &[("action.yml", NODE24)]);
    harness.tag("other/private", "v1", &private);
    let runner = harness.agent("runner1-1").await;

    harness
        .dispatch("      - uses: actions/checkout@v4\n")
        .await;
    let (_, documents) = harness.assigned(&runner).await;
    assert_eq!(documents.actions[0].commit, public);
    assert_eq!(harness.downloads("actions/checkout"), [public]);
    assert!(
        harness.forge.tokens().is_empty(),
        "an installation token was minted: {:#?}",
        harness.forge.tokens()
    );

    let run = harness.dispatch("      - uses: other/private@v1\n").await;
    let runner = harness.agent("runner1-2").await;
    let claimed = harness.claim(&runner).await;
    assert!(harness.assign(claimed).await.is_none());
    let notices = harness.notices(run).await;
    assert_eq!(
        notices
            .iter()
            .map(|notice| notice.diagnostic.message.as_str())
            .collect::<Vec<_>>(),
        [
            "the action `other/private@v1` cannot be resolved: organisation `globalart` has no \
             installation of the GitHub App, so only public actions resolve, and no public \
             repository `other/private` has a ref `v1`"
        ]
    );
}

#[tokio::test]
async fn a_server_without_a_github_app_fails_every_job_that_uses_an_action() {
    let world = World::new().await;
    let workflow = bosun_workflow::parse(
        "ci",
        "on: workflow_dispatch\njobs:\n  build:\n    runs-on: self-hosted\n    steps:\n      - \
         uses: actions/checkout@v4\n",
    )
    .expect("a workflow")
    .workflow;
    let run = world.dispatch(&workflow).await;
    let replica = Uuid::now_v7();
    let agent = super::queue::connected(&world, "runner1-1", &LABELS, replica).await;
    let claimed = match bosun_db::queue::claim(
        world.db(),
        Claim {
            agent: agent.id,
            replica_id: replica,
            lease: LEASE,
            passed_over: &[],
        },
    )
    .await
    .expect("a claim")
    {
        Claimed::Job(claimed) => *claimed,
        other => panic!("no job was claimed: {other:?}"),
    };
    assert!(
        queue::assignment(world.db(), claimed, &site(), &keys(), world.actions())
            .await
            .expect("a failed assignment")
            .is_none()
    );
    let notices = world.run(run).await.notices;
    assert_eq!(
        notices
            .iter()
            .map(|notice| notice.diagnostic.message.as_str())
            .collect::<Vec<_>>(),
        [
            "the action `actions/checkout@v4` cannot be resolved: this server has no GitHub App \
             to resolve actions with"
        ]
    );
}

fn action_codes() -> BTreeSet<(String, String)> {
    let document = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/design/04-runs.md");
    let text = fs::read_to_string(&document)
        .unwrap_or_else(|error| panic!("{}: {error}", document.display()));
    let marker = "The codes of the notices about actions:";
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|line| line.contains(marker))
        .unwrap_or_else(|| panic!("no `{marker}` in docs/design/04-runs.md"));
    lines[start..]
        .iter()
        .skip_while(|line| !line.starts_with('|'))
        .take_while(|line| line.starts_with('|'))
        .skip(2)
        .map(|row| {
            let cells: Vec<&str> = row.trim_matches('|').split('|').map(str::trim).collect();
            (cells[0].trim_matches('`').to_owned(), cells[1].to_owned())
        })
        .collect()
}

#[tokio::test]
async fn every_notice_code_about_actions_is_in_the_table_of_the_design() {
    let harness = Harness::new().await;
    for (full_name, files) in [
        ("acme/legacy", vec![("action.yml", NODE16)]),
        ("acme/composite", vec![("action.yml", COMPOSITE)]),
        (
            "acme/broken",
            vec![("action.yml", "name: Broken\nruns:\n  using: node20\n")],
        ),
        ("acme/bare", vec![("README.md", "no metadata\n")]),
    ] {
        let commit = harness.action(full_name, true, &files);
        harness.tag(full_name, "v1", &commit);
    }
    let mut produced = BTreeSet::new();
    for (index, steps) in [
        "      - uses: acme/legacy@v1\n",
        "      - uses: acme/composite@v1\n",
        "      - uses: acme/broken@v1\n",
        "      - uses: acme/bare@v1\n",
        "      - uses: acme/missing@v1\n",
    ]
    .into_iter()
    .enumerate()
    {
        let run = harness.dispatch(steps).await;
        let runner = harness.agent(&format!("runner{index}-1")).await;
        let claimed = harness.claim(&runner).await;
        harness.assign(claimed).await;
        let notices = harness.notices(run).await;
        assert_eq!(notices.len(), 1, "{steps}: {notices:#?}");
        let severity = serde_json::to_value(notices[0].diagnostic.severity).expect("a severity");
        produced.insert((
            notices[0].diagnostic.code.clone(),
            severity.as_str().expect("a name").to_owned(),
        ));
    }
    assert_eq!(produced, action_codes());
}

async fn upstream(
    files: BTreeMap<String, Vec<u8>>,
) -> (Url, Arc<Mutex<Vec<String>>>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a port for the upstream");
    let address = listener.local_addr().expect("an address");
    let requested = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requested);
    let files = Arc::new(files);
    let router = Router::new().fallback(move |uri: Uri| {
        let files = Arc::clone(&files);
        let seen = Arc::clone(&seen);
        async move {
            seen.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(uri.path().to_owned());
            match files.get(uri.path()) {
                Some(body) => (StatusCode::OK, body.clone()).into_response(),
                None => StatusCode::NOT_FOUND.into_response(),
            }
        }
    });
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    let url = Url::parse(&format!("http://{address}/dist")).expect("an address");
    (url, requested, server)
}

fn checksums(entries: &[(&str, &[u8])]) -> Vec<u8> {
    entries
        .iter()
        .map(|(name, body)| format!("{}  {name}\n", crate::digest::hex(&Sha256::digest(body))))
        .collect::<String>()
        .into_bytes()
}

#[tokio::test]
async fn a_node_archive_whose_checksum_differs_is_refused_and_a_stored_one_is_served_again() {
    let harness = Harness::new().await;
    let first = harness.agent("runner1-1").await;
    let second = harness.agent("runner1-2").await;
    let version = harness
        .directory
        .path()
        .join("upstream")
        .join(format!("v{NODE}"));
    fs::create_dir_all(&version).expect("an upstream directory");
    let archive = format!("node-v{NODE}-linux-x64.tar.xz");
    let content = b"the packed Node runtime for linux-x64".to_vec();
    fs::write(version.join(&archive), &content).expect("an archive");
    fs::write(
        version.join("SHASUMS256.txt"),
        checksums(&[(archive.as_str(), b"another archive")]),
    )
    .expect("a checksum list");
    let path = format!("/_runtime/tools/node/{NODE}/linux-x64.tar.xz");

    let (status, body) = harness.get(&path, Some(&first.credential)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let refused = message(&body);
    assert!(
        refused.starts_with(&format!(
            "Node {NODE} for linux-x64 cannot be fetched: {archive} from the upstream was not \
             stored: its SHA-256 is {}",
            crate::digest::hex(&Sha256::digest(&content))
        )),
        "{refused}"
    );
    let key = Key::new(format!("tools/node/{NODE}/linux-x64.tar.xz")).expect("a key");
    assert!(
        harness.state.store.get(&key, ..).await.is_err(),
        "a refused archive was stored"
    );
    assert_eq!(
        bosun_db::tools::find(harness.db(), "node", NODE, "linux-x64")
            .await
            .expect("a query"),
        None
    );

    fs::write(
        version.join("SHASUMS256.txt"),
        checksums(&[(archive.as_str(), &content)]),
    )
    .expect("a checksum list");
    let (status, body) = harness.get(&path, Some(&first.credential)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, content);
    fs::remove_dir_all(&version).expect("the upstream removed");
    let (status, body) = harness.get(&path, Some(&second.credential)).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(body, content);

    for (path, credential, status) in [
        (path.as_str(), None, StatusCode::UNAUTHORIZED),
        (
            "/_runtime/tools/node/20.19.5/linux-x64.tar.xz",
            Some(first.credential.as_str()),
            StatusCode::NOT_FOUND,
        ),
        (
            &*format!("/_runtime/tools/node/{NODE}/darwin-arm64.tar.xz"),
            Some(first.credential.as_str()),
            StatusCode::NOT_FOUND,
        ),
    ] {
        assert_eq!(harness.get(path, credential).await.0, status, "{path}");
    }
}

#[tokio::test]
async fn a_node_archive_comes_from_its_http_upstream_once_and_musl_from_the_musl_upstream() {
    let harness = Harness::new().await;
    let agents = [
        harness.agent("runner1-1").await,
        harness.agent("runner1-2").await,
    ];
    let glibc = format!("node-v{NODE}-linux-arm64.tar.xz");
    let musl = format!("node-v{NODE}-linux-arm64-musl.tar.xz");
    let (glibc_body, musl_body) = (b"arm64".to_vec(), b"arm64 on musl".to_vec());
    let files = BTreeMap::from([
        (format!("/dist/v{NODE}/{glibc}"), glibc_body.clone()),
        (
            format!("/dist/v{NODE}/SHASUMS256.txt"),
            checksums(&[(glibc.as_str(), &glibc_body)]),
        ),
        (format!("/musl/v{NODE}/{musl}"), musl_body.clone()),
        (
            format!("/musl/v{NODE}/SHASUMS256.txt"),
            checksums(&[(musl.as_str(), &musl_body)]),
        ),
    ]);
    let (dist, requested, _server) = upstream(files).await;
    let mut settings = tools_settings(&dist);
    settings.node_musl_upstream = dist.join("musl").expect("the musl upstream");
    let state = serving(&harness.state, Arc::clone(&harness.state.store), settings);
    for agent in &agents {
        for (platform, expected) in [
            ("linux-arm64", &glibc_body),
            ("linux-arm64-musl", &musl_body),
        ] {
            let router = runtime::routes().with_state(state.clone());
            let (status, body) = request(
                router,
                &format!("/_runtime/tools/node/{NODE}/{platform}.tar.xz"),
                Some(&agent.credential),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{platform}");
            assert_eq!(&body, expected, "{platform}");
        }
    }
    let requested = requested
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    assert_eq!(
        requested,
        [
            format!("/dist/v{NODE}/SHASUMS256.txt"),
            format!("/dist/v{NODE}/{glibc}"),
            format!("/musl/v{NODE}/SHASUMS256.txt"),
            format!("/musl/v{NODE}/{musl}"),
        ]
    );
}

#[tokio::test]
async fn two_replicas_asked_for_the_same_new_node_archive_at_once_fetch_it_once() {
    let harness = Harness::new().await;
    let agents = [
        harness.agent("runner1-1").await,
        harness.agent("runner2-1").await,
    ];
    let archive = format!("node-v{NODE}-linux-x64.tar.xz");
    let content = b"x64".to_vec();
    let files = BTreeMap::from([
        (format!("/dist/v{NODE}/{archive}"), content.clone()),
        (
            format!("/dist/v{NODE}/SHASUMS256.txt"),
            checksums(&[(archive.as_str(), &content)]),
        ),
    ]);
    let (dist, requested, _server) = upstream(files).await;
    let replicas = [0, 1].map(|_| {
        serving(
            &harness.state,
            Arc::clone(&harness.state.store),
            tools_settings(&dist),
        )
    });
    let path = format!("/_runtime/tools/node/{NODE}/linux-x64.tar.xz");
    let (one, two) = tokio::join!(
        request(
            runtime::routes().with_state(replicas[0].clone()),
            &path,
            Some(&agents[0].credential),
        ),
        request(
            runtime::routes().with_state(replicas[1].clone()),
            &path,
            Some(&agents[1].credential),
        ),
    );
    for (status, body) in [one, two] {
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        assert_eq!(body, content);
    }
    assert_eq!(
        requested
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone(),
        [
            format!("/dist/v{NODE}/SHASUMS256.txt"),
            format!("/dist/v{NODE}/{archive}"),
        ]
    );
}

#[tokio::test]
async fn a_node_archive_whose_checksum_differs_leaves_nothing_in_an_s3_store() {
    let harness = Harness::new().await;
    let agent = harness.agent("runner1-1").await;
    let bucket = TestBucket::create().await;
    let small = b"the packed Node runtime for linux-x64".to_vec();
    let large = vec![7_u8; 9 << 20];
    let archives = [("linux-x64", &small), ("linux-arm64", &large)]
        .map(|(platform, content)| (platform, format!("node-v{NODE}-{platform}.tar.xz"), content));
    let mut files: BTreeMap<String, Vec<u8>> = archives
        .iter()
        .map(|(_, archive, content)| (format!("/dist/v{NODE}/{archive}"), (*content).clone()))
        .collect();
    files.insert(
        format!("/dist/v{NODE}/SHASUMS256.txt"),
        checksums(
            &archives
                .iter()
                .map(|(_, archive, _)| (archive.as_str(), &b"another archive"[..]))
                .collect::<Vec<_>>(),
        ),
    );
    let (dist, _, _server) = upstream(files).await;
    let store = Arc::new(Backend::S3(
        S3::new(bucket.settings().clone()).expect("an S3 store"),
    ));
    let state = serving(&harness.state, Arc::clone(&store), tools_settings(&dist));
    for (platform, archive, content) in &archives {
        let (status, body) = request(
            runtime::routes().with_state(state.clone()),
            &format!("/_runtime/tools/node/{NODE}/{platform}.tar.xz"),
            Some(&agent.credential),
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{platform}");
        let refused = message(&body);
        assert!(
            refused.starts_with(&format!(
                "Node {NODE} for {platform} cannot be fetched: {archive} from the upstream was \
                 not stored: its SHA-256 is {}",
                crate::digest::hex(&Sha256::digest(content.as_slice()))
            )),
            "{refused}"
        );
        let key = Key::new(format!("tools/node/{NODE}/{platform}.tar.xz")).expect("a key");
        assert!(
            matches!(
                store.get(&key, ..).await,
                Err(bosun_blob::Error::NotFound(_))
            ),
            "a refused archive for {platform} was stored"
        );
        assert_eq!(
            bosun_db::tools::find(harness.db(), "node", NODE, platform)
                .await
                .expect("a query"),
            None
        );
    }
    assert_eq!(
        bucket
            .store()
            .uploads()
            .await
            .expect("the uploads in progress"),
        Vec::new(),
        "the multipart upload of the larger archive was not aborted"
    );
}

#[tokio::test]
async fn the_notices_about_actions_are_those_of_the_api_fixture() {
    let harness = Harness::new().await;
    let legacy = harness.action("acme/legacy", true, &[("action.yml", NODE16)]);
    harness.tag("acme/legacy", "v1", &legacy);
    let source = "on: workflow_dispatch\njobs:\n  legacy:\n    runs-on: self-hosted\n    steps:\n      \
                  - uses: acme/legacy@v1\n  private:\n    runs-on: self-hosted\n    steps:\n      \
                  - uses: other/private@v1\n";
    let workflow = bosun_workflow::parse("ci", source)
        .expect("a workflow")
        .workflow;
    let run = harness.world.dispatch(&workflow).await;
    for name in ["runner1-1", "runner1-2"] {
        let runner = harness.agent(name).await;
        let claimed = harness.claim(&runner).await;
        harness.assign(claimed).await;
    }
    let projection = harness.world.project(run).await;
    let shown = serde_json::to_value(harness.world.run(run).await).expect("a run as JSON");
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/fixtures/api/run-actions.json");
    let text =
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let mut expected: serde_json::Value = serde_json::from_str(&text).expect("a run");
    let jobs = ["legacy", "private"].map(|key| projection.job(key).0.to_string());
    for (notice, job) in expected["notices"]
        .as_array_mut()
        .expect("the notices of the fixture")
        .iter_mut()
        .zip(jobs)
    {
        notice["job_id"] = serde_json::Value::String(job);
    }
    assert_eq!(
        shown["notices"], expected["notices"],
        "{:#}",
        shown["notices"]
    );
}
