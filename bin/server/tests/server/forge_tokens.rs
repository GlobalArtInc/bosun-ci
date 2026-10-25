use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bosun_proto::agent::v1::agent_message::Message as Report;
use bosun_proto::agent::v1::{Accept, Conclusion, JobFinished};
use bosun_testkit::TestDatabase;
use bosun_testkit::agent::FakeAgent;
use bosun_testkit::forge::{FakeForge, MintedToken, Sender};
use serde_json::json;
use tokio::time::sleep;

use crate::api::{Api, text};
use crate::hooks::{ACCOUNT, delivered, processed};
use crate::{Launch, WAIT, owner};

const REPOSITORY: &str = "GlobalArtInc/ops";

const HELD: &[(&str, &str)] = &[
    ("checks", "write"),
    ("contents", "read"),
    ("issues", "write"),
    ("metadata", "read"),
    ("statuses", "write"),
];

const ROMAN: Sender<'static> = Sender {
    login: "roman",
    id: 7001,
};

const WORKFLOW: &str = "name: ci
on: push
jobs:
  build:
    runs-on: [self-hosted]
    permissions:
      contents: read
      issues: write
    steps:
      - run: sleep 3
      - run: gh issue list
";

fn minted(forge: &FakeForge, token: &str) -> MintedToken {
    forge
        .tokens()
        .into_iter()
        .find(|minted| minted.token == token)
        .unwrap_or_else(|| panic!("the fake forge minted no such token"))
}

async fn status_of(forge: &FakeForge, token: &str) -> u16 {
    reqwest::Client::new()
        .get(format!("{}/installation/repositories", forge.api_url()))
        .bearer_auth(token)
        .header("user-agent", "bosun-test")
        .send()
        .await
        .expect("the fake forge answers")
        .status()
        .as_u16()
}

#[tokio::test]
async fn an_agent_renews_its_forge_token_on_its_stream_and_the_last_one_is_revoked_at_the_end() {
    let database = TestDatabase::migrated().await;
    let forge = FakeForge::start().await;
    let installation = forge.install(ACCOUNT, HELD);
    forge.create_repository(installation, REPOSITORY);
    let first = forge.commit(REPOSITORY, "main", [("README.md", "# ops\n")]);
    let token = owner(&database.url(), "roman").await;
    let api = Api::serve(Launch::serve(&database.url()).github(&forge), token).await;
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
    let (status, registration) = api
        .post(
            "/api/v1/registration-tokens",
            &json!({ "pool": "default", "reusable": false, "expires_at": null }),
        )
        .await;
    assert_eq!(status, 201, "{registration}");
    let agent =
        FakeAgent::register(api.address, text(&registration["token"]), "runner1", &[]).await;
    let mut connection = agent.connect(api.address).await;
    forge.set_token_lifetime(Duration::from_secs(3));
    let after = forge.commit(REPOSITORY, "main", [(".bosun/workflows/ci.yml", WORKFLOW)]);
    let push = forge.push_event(REPOSITORY, "refs/heads/main", &first, &after, ROMAN);
    delivered(&api, &forge, "push", &push).await;
    processed(&database).await;
    connection.idle();
    let assignment = connection.assignment().await;
    let execution = assignment.execution_id.clone();
    let assigned = assignment
        .forge_token
        .expect("the assignment carries a forge token");
    let recorded = minted(&forge, &assigned.token);
    assert_eq!(recorded.repositories, ["ops"]);
    assert_eq!(
        recorded.permissions,
        BTreeMap::from([
            ("contents".to_owned(), "read".to_owned()),
            ("issues".to_owned(), "write".to_owned()),
        ])
    );
    connection.send(Report::Accept(Accept {
        execution_id: execution.clone(),
    }));

    sleep(Duration::from_millis(3100)).await;
    assert_eq!(status_of(&forge, &assigned.token).await, 401);
    let renewed = connection.renew_forge_token(&execution).await;
    assert_eq!(renewed.execution_id, execution);
    assert_eq!(renewed.error, "");
    let fresh = renewed.token.expect("a fresh token");
    assert_ne!(fresh.token, assigned.token);
    assert_eq!(status_of(&forge, &fresh.token).await, 200);
    assert_eq!(
        minted(&forge, &fresh.token).permissions,
        recorded.permissions
    );

    forge.set_token_lifetime(Duration::from_secs(3600));
    let lasting = connection
        .renew_forge_token(&execution)
        .await
        .token
        .expect("a token that lasts");
    assert!(
        lasting.expires_in.expect("its lifetime").seconds > 3500,
        "{lasting:?}"
    );
    let again = connection.renew_forge_token(&execution).await;
    assert_eq!(
        again.token.map(|token| token.token),
        Some(lasting.token.clone())
    );
    let unknown = connection
        .renew_forge_token("0199a5f2-2c6f-7a3e-8a3b-5f0d4c1e2b3a")
        .await;
    assert_eq!(unknown.token, None);
    assert!(
        unknown.error.contains("does not run on this agent"),
        "{unknown:?}"
    );

    connection.send(Report::JobFinished(JobFinished {
        execution_id: execution.clone(),
        conclusion: Conclusion::Success.into(),
        outputs: Default::default(),
    }));
    let deadline = Instant::now() + WAIT;
    while !minted(&forge, &lasting.token).revoked {
        assert!(
            Instant::now() < deadline,
            "the last forge token was not revoked"
        );
        sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(status_of(&forge, &lasting.token).await, 401);
    let ended = connection.renew_forge_token(&execution).await;
    assert!(ended.error.contains("has ended"), "{ended:?}");
}
