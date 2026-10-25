use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bosun_db::installations::Account;
use bosun_db::pools::Access;
use bosun_db::queue::ClaimedJob;
use bosun_forge::GitHubApp;
use bosun_proto::agent::v1::Assignment;
use bosun_proto::agent::v1::server_message::Message;
use bosun_proto::assignment::Documents;
use bosun_testkit::forge::{APP_ID, APP_KEY, FakeForge, MintedToken};
use bosun_types::api::{Capability, Notice, Repository, TrustLevel};
use bosun_types::diagnostic::Severity;
use bosun_types::id::{ExecutionId, RunId};
use bosun_types::status::{Conclusion, JobStatus, StatusReason};
use chrono::{TimeDelta, Utc};
use sea_orm::TransactionTrait;
use sea_orm::prelude::Uuid;
use serde_json::Value;

use super::queue::{LABELS, LEASE, claim, connected, connected_in, parse, pool};
use super::{World, finished, job_cancel, keys, site};
use crate::agents::progress::{self, Progress, Reporter};
use crate::clock::Clock;
use crate::forge::Forge;
use crate::forge_token::Tokens;
use crate::metrics::Meters;
use crate::outbox::{self, Outbox};
use crate::queue;

const OWNER: &str = "globalart";

const NAME: &str = "keel";

const HELD: &[(&str, &str)] = &[
    ("checks", "write"),
    ("contents", "write"),
    ("issues", "write"),
    ("metadata", "read"),
    ("pull_requests", "write"),
    ("statuses", "write"),
];

struct Scene {
    world: World,
    fake: FakeForge,
    tokens: Tokens,
    outbox: Outbox,
    replica: Uuid,
}

impl Scene {
    async fn new() -> Scene {
        let world = World::new().await;
        let fake = FakeForge::start().await;
        let installation = fake.install(OWNER, HELD);
        let repository_id = fake.create_repository(installation, &format!("{OWNER}/{NAME}"));
        let db = world.db();
        let project = bosun_db::projects::find(db, world.workflow.project_id)
            .await
            .expect("a project")
            .expect("the project of the world");
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
        bosun_db::installations::sync(
            &transaction,
            installation,
            &[Repository {
                id: repository_id,
                owner: OWNER.to_owned(),
                name: NAME.to_owned(),
                default_branch: "main".to_owned(),
            }],
        )
        .await
        .expect("a sync");
        transaction.commit().await.expect("a commit");
        bosun_db::projects::bind(
            db,
            project.id,
            Some(&Repository {
                id: repository_id,
                owner: OWNER.to_owned(),
                name: NAME.to_owned(),
                default_branch: "main".to_owned(),
            }),
        )
        .await
        .expect("a binding");
        let app =
            GitHubApp::new(APP_ID, APP_KEY.as_bytes(), &fake.api_url()).expect("the App's client");
        let forge = Arc::new(Forge::of(app));
        let keys = Arc::new(keys());
        Scene {
            tokens: Tokens::new(db.clone(), Some(Arc::clone(&forge)), Arc::clone(&keys)),
            outbox: Outbox {
                db: db.clone(),
                store: Arc::clone(world.store()),
                forge,
                keys,
                site: site(),
                clock: Clock::system(),
                meters: Arc::new(Meters::default()),
            },
            world,
            fake,
            replica: Uuid::now_v7(),
        }
    }

    async fn agent(&self, name: &str) -> Reporter {
        connected(&self.world, name, &LABELS, self.replica).await
    }

    async fn claim(&self, agent: &Reporter) -> ClaimedJob {
        claim(self.world.db(), agent.id, self.replica, LEASE, &[])
            .await
            .expect("a job for the agent")
    }

    async fn assign(&self, claimed: ClaimedJob) -> Option<(Assignment, Documents)> {
        let offer = queue::assignment(
            self.world.db(),
            claimed,
            &site(),
            &self.tokens,
            self.world.actions(),
        )
        .await
        .expect("an assignment")?;
        let Some(Message::Assignment(assignment)) = offer.message.message else {
            panic!("the offer holds no assignment");
        };
        let documents = Documents::read(&assignment).expect("documents the agent reads");
        Some((assignment, documents))
    }

    async fn assigned(&self, agent: &Reporter) -> (ExecutionId, Assignment, Documents) {
        let claimed = self.claim(agent).await;
        let execution = claimed.execution_id;
        let (assignment, documents) = self
            .assign(claimed)
            .await
            .expect("an assignment that reaches the agent");
        (execution, assignment, documents)
    }

    async fn accept(&self, agent: &Reporter, execution: ExecutionId) {
        let accepted = progress::accepted(self.world.db(), agent, execution, &site())
            .await
            .expect("an acceptance");
        assert_eq!(accepted, Progress::Applied);
    }

    async fn finish(&self, agent: &Reporter, execution: ExecutionId, conclusion: Conclusion) {
        let ended = progress::job_finished(
            self.world.db(),
            agent,
            execution,
            finished(conclusion),
            &site(),
        )
        .await
        .expect("a finished job");
        assert_eq!(ended, Progress::Applied);
    }

    async fn drain(&self) {
        while outbox::next(&self.outbox)
            .await
            .unwrap_or_else(|error| panic!("the outbox failed: {error}"))
        {}
    }

    fn minted(&self, token: &str) -> MintedToken {
        self.fake
            .tokens()
            .into_iter()
            .find(|minted| minted.token == token)
            .unwrap_or_else(|| panic!("the fake forge minted no such token"))
    }

    async fn notices(&self, run: RunId) -> Vec<Notice> {
        self.world.run(run).await.notices
    }

    async fn status_of(&self, token: &str) -> u16 {
        reqwest::Client::new()
            .get(format!("{}/installation/repositories", self.fake.api_url()))
            .bearer_auth(token)
            .header("user-agent", "bosun-test")
            .send()
            .await
            .expect("the fake forge answers")
            .status()
            .as_u16()
    }
}

fn levels(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(name, level)| ((*name).to_owned(), (*level).to_owned()))
        .collect()
}

fn text(value: Option<&Value>) -> &str {
    value.and_then(Value::as_str).unwrap_or_else(|| {
        panic!("{value:?} is not a string");
    })
}

#[tokio::test]
async fn a_job_receives_a_token_with_exactly_its_permissions_on_the_runs_repository_alone() {
    let scene = Scene::new().await;
    let run = scene
        .world
        .dispatch(&parse(
            "on: workflow_dispatch
env:
  WORKFLOW_TOKEN: ${{ github.token }}
  SECRET_TOKEN: ${{ secrets.GITHUB_TOKEN }}
jobs:
  build:
    runs-on: self-hosted
    permissions:
      contents: read
      issues: write
    env:
      JOB_TOKEN: x-access-token:${{ github.token }}
    steps:
      - run: gh issue list
",
        ))
        .await;
    let agent = scene.agent("runner1-1").await;
    let (_, assignment, documents) = scene.assigned(&agent).await;
    let token = assignment
        .forge_token
        .as_ref()
        .expect("the assignment carries the forge token");
    let expires_in = token.expires_in.expect("the token's lifetime");
    assert!(
        (3500..=3600).contains(&expires_in.seconds),
        "{expires_in:?}"
    );
    assert_eq!(text(documents.contexts.github.get("token")), token.token);
    assert_eq!(
        text(documents.contexts.secrets.get("GITHUB_TOKEN")),
        token.token
    );
    assert_eq!(
        documents.evaluated.env,
        BTreeMap::from([
            (
                "JOB_TOKEN".to_owned(),
                format!("x-access-token:{}", token.token)
            ),
            ("SECRET_TOKEN".to_owned(), token.token.clone()),
            ("WORKFLOW_TOKEN".to_owned(), token.token.clone()),
        ])
    );
    let minted = scene.minted(&token.token);
    assert_eq!(scene.fake.tokens().len(), 1);
    assert_eq!(minted.repository_selection, "selected");
    assert_eq!(minted.repositories, [NAME]);
    assert_eq!(
        minted.permissions,
        levels(&[("contents", "read"), ("issues", "write")])
    );
    assert!(!minted.revoked);
    assert_eq!(scene.notices(run).await, []);
}

#[tokio::test]
async fn without_permissions_a_job_receives_the_projects_token_permissions() {
    let scene = Scene::new().await;
    scene
        .world
        .dispatch(&parse(
            "on: workflow_dispatch
jobs:
  build:
    runs-on: self-hosted
    steps:
      - run: make
",
        ))
        .await;
    let agent = scene.agent("runner1-1").await;
    let (_, assignment, _) = scene.assigned(&agent).await;
    let token = assignment.forge_token.expect("a forge token");
    assert_eq!(
        scene.minted(&token.token).permissions,
        levels(&[("contents", "read"), ("metadata", "read")])
    );
}

#[tokio::test]
async fn write_all_is_intersected_with_the_installation_and_each_missing_scope_is_a_warning() {
    let scene = Scene::new().await;
    let run = scene
        .world
        .dispatch(&parse(
            "on: workflow_dispatch
permissions: write-all
jobs:
  build:
    runs-on: self-hosted
    steps:
      - run: make
",
        ))
        .await;
    let agent = scene.agent("runner1-1").await;
    let (_, assignment, _) = scene.assigned(&agent).await;
    let token = assignment.forge_token.expect("a forge token");
    assert_eq!(scene.minted(&token.token).permissions, levels(HELD));
    let notices = scene.notices(run).await;
    let missing: BTreeSet<&str> = [
        "actions",
        "attestations",
        "deployments",
        "discussions",
        "models",
        "packages",
        "pages",
        "repository_projects",
        "security_events",
    ]
    .into_iter()
    .collect();
    assert_eq!(notices.len(), missing.len(), "{notices:#?}");
    for notice in &notices {
        assert_eq!(notice.diagnostic.code, "permission_unavailable");
        assert_eq!(notice.diagnostic.severity, Severity::Warning);
        assert!(notice.job_id.is_some());
        assert!(
            notice
                .diagnostic
                .message
                .starts_with("the `permissions` of the workflow ask the forge token for `"),
            "{}",
            notice.diagnostic.message
        );
        assert!(
            notice.diagnostic.message.ends_with(
                "which the installation of the GitHub App does not hold, so the token of job \
                 `build` is minted without it"
            ),
            "{}",
            notice.diagnostic.message
        );
    }
    let named: BTreeSet<&str> = notices
        .iter()
        .filter_map(|notice| {
            missing
                .iter()
                .copied()
                .find(|name| notice.diagnostic.message.contains(&format!("`{name}: ")))
        })
        .collect();
    assert_eq!(named, missing);
}

#[tokio::test]
async fn an_untrusted_runs_token_is_read_only_whatever_its_workflow_asks() {
    let scene = Scene::new().await;
    let forks = pool(
        &scene.world,
        "forks",
        TrustLevel::Untrusted,
        &[Access::AllProjects],
    )
    .await;
    let run = scene
        .world
        .dispatch_from_fork(
            &parse(
                "on: workflow_dispatch
jobs:
  build:
    runs-on: self-hosted
    permissions:
      contents: write
      issues: write
      pull-requests: write
    steps:
      - run: make
",
            ),
            true,
        )
        .await;
    let outside = connected_in(
        &scene.world,
        forks,
        "runner2-1",
        &["self-hosted", "linux", "x64", "podman"],
        &[Capability::Podman],
        scene.replica,
    )
    .await;
    let (_, assignment, documents) = scene.assigned(&outside).await;
    let token = assignment.forge_token.expect("a forge token");
    assert_eq!(
        scene.minted(&token.token).permissions,
        levels(&[
            ("contents", "read"),
            ("issues", "read"),
            ("pull_requests", "read")
        ])
    );
    assert_eq!(
        documents.contexts.secrets,
        serde_json::Map::from_iter([("GITHUB_TOKEN".to_owned(), Value::String(token.token))])
    );
    assert_eq!(scene.notices(run).await, []);
}

#[tokio::test]
async fn the_token_is_revoked_when_its_execution_completes_and_after_a_cancel() {
    let scene = Scene::new().await;
    let source = "on: workflow_dispatch
jobs:
  build:
    runs-on: self-hosted
    steps:
      - run: make
";
    let agent = scene.agent("runner1-1").await;

    scene.world.dispatch(&parse(source)).await;
    let (execution, assignment, _) = scene.assigned(&agent).await;
    let done = assignment.forge_token.expect("a forge token").token;
    scene.accept(&agent, execution).await;
    scene.drain().await;
    assert!(!scene.minted(&done).revoked);
    scene.finish(&agent, execution, Conclusion::Success).await;
    scene.drain().await;
    assert!(scene.minted(&done).revoked);

    let run = scene.world.dispatch(&parse(source)).await;
    let (execution, assignment, _) = scene.assigned(&agent).await;
    let cancelled = assignment.forge_token.expect("a forge token").token;
    scene.accept(&agent, execution).await;
    let (job, _) = scene.world.project(run).await.job("build");
    scene.world.command(run, &job_cancel(job)).await;
    scene.drain().await;
    assert!(!scene.minted(&cancelled).revoked);
    scene.finish(&agent, execution, Conclusion::Cancelled).await;
    scene.drain().await;
    assert!(scene.minted(&cancelled).revoked);

    let run = scene.world.dispatch(&parse(source)).await;
    let (_, assignment, _) = scene.assigned(&agent).await;
    let unaccepted = assignment.forge_token.expect("a forge token").token;
    let (job, _) = scene.world.project(run).await.job("build");
    scene.world.command(run, &job_cancel(job)).await;
    scene.drain().await;
    assert!(scene.minted(&unaccepted).revoked);

    assert_eq!(
        scene.status_of(&done).await,
        401,
        "the forge still accepts a revoked token"
    );
    assert_eq!(
        super::queue::values(
            scene.world.db(),
            "SELECT id::text AS value FROM bosun.forge_token"
        )
        .await,
        Vec::<String>::new()
    );
}

#[tokio::test]
async fn a_renewed_token_is_fresh_the_forge_accepts_it_and_both_are_revoked_at_the_end() {
    let scene = Scene::new().await;
    scene.fake.set_token_lifetime(Duration::from_secs(2));
    scene
        .world
        .dispatch(&parse(
            "on: workflow_dispatch
jobs:
  build:
    runs-on: self-hosted
    permissions:
      contents: read
      issues: write
    steps:
      - run: sleep 3
      - run: gh issue list
",
        ))
        .await;
    let agent = scene.agent("runner1-1").await;
    let (execution, assignment, _) = scene.assigned(&agent).await;
    let first = assignment.forge_token.expect("a forge token").token;
    scene.accept(&agent, execution).await;
    assert_eq!(scene.status_of(&first).await, 200);

    let other = scene.agent("runner1-2").await;
    let refused = scene
        .tokens
        .renew(other.id, execution)
        .await
        .err()
        .expect("another agent renews nothing");
    assert!(refused.contains("does not run on this agent"), "{refused}");

    tokio::time::sleep(Duration::from_millis(2100)).await;
    assert_eq!(scene.status_of(&first).await, 401);
    let renewed = scene
        .tokens
        .renew(agent.id, execution)
        .await
        .unwrap_or_else(|error| panic!("the token was not renewed: {error}"));
    assert_ne!(renewed.token, first);
    assert!(renewed.expires_at > Utc::now());
    assert_eq!(scene.status_of(&renewed.token).await, 200);
    let minted = scene.minted(&renewed.token);
    assert_eq!(minted.repositories, [NAME]);
    assert_eq!(
        minted.permissions,
        levels(&[("contents", "read"), ("issues", "write")])
    );

    scene.fake.set_token_lifetime(Duration::from_secs(3600));
    let long = scene
        .tokens
        .renew(agent.id, execution)
        .await
        .unwrap_or_else(|error| panic!("the token was not renewed: {error}"));
    assert!(long.expires_at - Utc::now() > TimeDelta::minutes(59));
    let again = scene
        .tokens
        .renew(agent.id, execution)
        .await
        .unwrap_or_else(|error| panic!("the token was not renewed: {error}"));
    assert_eq!(
        again.token, long.token,
        "a token far from its expiry is handed out again"
    );
    assert_eq!(scene.fake.tokens().len(), 3);

    scene.finish(&agent, execution, Conclusion::Success).await;
    scene.drain().await;
    assert!(scene.minted(&long.token).revoked);
    assert_eq!(scene.status_of(&long.token).await, 401);
    let ended = scene
        .tokens
        .renew(agent.id, execution)
        .await
        .err()
        .expect("an ended execution renews nothing");
    assert!(ended.contains("has ended"), "{ended}");
}

#[tokio::test]
async fn a_job_of_a_project_without_a_repository_receives_an_empty_token() {
    let scene = Scene::new().await;
    let db = scene.world.db();
    bosun_db::projects::bind(db, scene.world.workflow.project_id, None)
        .await
        .expect("an unbound project");
    scene
        .world
        .dispatch(&parse(
            "on: workflow_dispatch
jobs:
  build:
    runs-on: self-hosted
    steps:
      - run: make
",
        ))
        .await;
    let agent = scene.agent("runner1-1").await;
    let (_, assignment, documents) = scene.assigned(&agent).await;
    assert_eq!(assignment.forge_token, None);
    assert_eq!(text(documents.contexts.github.get("token")), "");
    assert_eq!(text(documents.contexts.secrets.get("GITHUB_TOKEN")), "");
    assert_eq!(scene.fake.tokens(), []);
}

#[tokio::test]
async fn a_token_the_forge_does_not_mint_fails_the_job_at_its_assignment() {
    let scene = Scene::new().await;
    let run = scene
        .world
        .dispatch(&parse(
            "on: workflow_dispatch
jobs:
  build:
    runs-on: self-hosted
    steps:
      - run: make
",
        ))
        .await;
    let agent = scene.agent("runner1-1").await;
    let claimed = scene.claim(&agent).await;
    let execution = claimed.execution_id;
    scene.fake.fail(true);
    assert!(scene.assign(claimed).await.is_none());
    scene.fake.fail(false);
    let notices = scene.notices(run).await;
    assert_eq!(notices.len(), 1, "{notices:#?}");
    assert_eq!(notices[0].diagnostic.code, "token_unavailable");
    assert_eq!(notices[0].diagnostic.severity, Severity::Error);
    assert_eq!(
        notices[0].diagnostic.message,
        "the forge token of job `build` cannot be minted for the repository `globalart/keel`: \
         the forge answered 502: Server Error"
    );
    let projection = scene.world.project(run).await;
    let (_, job) = projection.job("build");
    assert_eq!(
        (job.status, job.conclusion),
        (JobStatus::Completed, Some(Conclusion::Failure))
    );
    assert_eq!(
        projection.executions[&execution].reason,
        Some(StatusReason::TokenUnavailable)
    );
    assert_eq!(scene.fake.tokens(), []);
}

#[tokio::test]
async fn a_token_minted_for_a_job_that_fails_at_its_assignment_is_revoked() {
    let scene = Scene::new().await;
    let run = scene
        .world
        .dispatch(&parse(
            "on: workflow_dispatch
env:
  BROKEN: ${{ fromJSON('not json') }}
jobs:
  build:
    runs-on: self-hosted
    steps:
      - run: make
",
        ))
        .await;
    let agent = scene.agent("runner1-1").await;
    let claimed = scene.claim(&agent).await;
    assert!(scene.assign(claimed).await.is_none());
    scene.drain().await;
    let tokens = scene.fake.tokens();
    assert_eq!(tokens.len(), 1);
    assert!(tokens[0].revoked);
    assert_eq!(
        scene.world.project(run).await.job("build").1.conclusion,
        Some(Conclusion::Failure)
    );
}

fn token_codes() -> BTreeSet<(String, String)> {
    let document = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/design/04-runs.md");
    let text = fs::read_to_string(&document)
        .unwrap_or_else(|error| panic!("{}: {error}", document.display()));
    let marker = "The codes of the notices about the forge token:";
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
async fn every_notice_code_about_the_forge_token_is_in_the_table_of_the_design() {
    let scene = Scene::new().await;
    let agent = scene.agent("runner1-1").await;
    let mut produced = BTreeSet::new();
    for (permissions, failing) in [("permissions: write-all\n", false), ("", true)] {
        let run = scene
            .world
            .dispatch(&parse(&format!(
                "on: workflow_dispatch\n{permissions}jobs:\n  build:\n    runs-on: self-hosted\n    \
                 steps:\n      - run: make\n"
            )))
            .await;
        let claimed = scene.claim(&agent).await;
        let execution = claimed.execution_id;
        scene.fake.fail(failing);
        if scene.assign(claimed).await.is_some() {
            scene.accept(&agent, execution).await;
            scene.finish(&agent, execution, Conclusion::Success).await;
        }
        scene.fake.fail(false);
        for notice in scene.notices(run).await {
            let severity = serde_json::to_value(notice.diagnostic.severity).expect("a severity");
            produced.insert((
                notice.diagnostic.code,
                severity.as_str().expect("a name").to_owned(),
            ));
        }
    }
    assert_eq!(produced, token_codes());
}
