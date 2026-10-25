use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bosun_testkit::TestDatabase;
use bosun_testkit::forge::{FakeForge, Webhook};
use serde_json::{Value, json};

use crate::api::{Api, send, text};
use crate::{Launch, WAIT, client, errors, message, strings};

pub const PERMISSIONS: &[(&str, &str)] = &[
    ("checks", "write"),
    ("contents", "read"),
    ("metadata", "read"),
    ("statuses", "write"),
];

pub const ACCOUNT: &str = "GlobalArtInc";

async fn deliver(
    api: &Api,
    webhook: &Webhook,
    signature: Option<&str>,
    body: &[u8],
) -> (u16, Value) {
    let mut request = client()
        .post(api.hooks())
        .header("content-type", "application/json")
        .header("x-github-event", webhook.event())
        .header("x-github-delivery", webhook.delivery())
        .body(body.to_vec());
    if let Some(signature) = signature {
        request = request.header("x-hub-signature-256", signature);
    }
    send(request).await
}

async fn stored(database: &TestDatabase) -> Vec<String> {
    strings(
        database.connection(),
        "SELECT delivery AS value FROM bosun.webhook_delivery ORDER BY id",
    )
    .await
}

pub async fn processed(database: &TestDatabase) {
    let deadline = Instant::now() + WAIT;
    loop {
        let pending = bosun_db::deliveries::pending(database.connection())
            .await
            .expect("the pending deliveries");
        if pending == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "{pending} deliveries were not processed within {WAIT:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let failed = strings(
        database.connection(),
        "SELECT delivery || ': ' || last_error AS value FROM bosun.webhook_delivery \
         WHERE last_error IS NOT NULL",
    )
    .await;
    assert!(failed.is_empty(), "deliveries failed: {failed:?}");
}

pub async fn delivered(api: &Api, forge: &FakeForge, event: &str, payload: &Value) {
    let status = forge.webhook(event, payload).send(&api.hooks()).await;
    assert_eq!(status, 202, "{event}");
}

fn fields(body: &Value) -> Vec<(String, String)> {
    body["error"]["fields"]
        .as_array()
        .unwrap_or_else(|| panic!("no fields in {body}"))
        .iter()
        .map(|field| {
            (
                text(&field["field"]).to_owned(),
                text(&field["message"]).to_owned(),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_delivery_without_the_signature_of_its_body_is_refused_and_a_valid_one_is_stored_once() {
    let database = TestDatabase::migrated().await;
    let forge = FakeForge::start().await;
    let api = Api::with_forge(&database, &forge).await;
    let webhook = forge.webhook(
        "ping",
        &json!({ "zen": "Keep it logically awesome.", "hook_id": 1 }),
    );
    let mut tampered = webhook.body().to_vec();
    tampered.extend_from_slice(b" ");
    let zeros = format!("sha256={}", "0".repeat(64));
    for (signature, body, message) in [
        (
            None,
            webhook.body(),
            "the delivery carries no X-Hub-Signature-256",
        ),
        (
            Some(zeros.as_str()),
            webhook.body(),
            "the X-Hub-Signature-256 of the delivery does not match its body under the webhook \
             secret",
        ),
        (
            Some(webhook.signature()),
            tampered.as_slice(),
            "the X-Hub-Signature-256 of the delivery does not match its body under the webhook \
             secret",
        ),
        (
            Some("sha256=not hexadecimal"),
            webhook.body(),
            "the X-Hub-Signature-256 of the delivery does not match its body under the webhook \
             secret",
        ),
        (
            Some(webhook.signature().trim_start_matches("sha256=")),
            webhook.body(),
            "the X-Hub-Signature-256 of the delivery does not match its body under the webhook \
             secret",
        ),
    ] {
        let (status, refused) = deliver(&api, &webhook, signature, body).await;
        assert_eq!(status, 401, "{signature:?}: {refused}");
        assert_eq!(refused["error"]["code"], "unauthenticated");
        assert_eq!(refused["error"]["message"], message);
    }
    assert_eq!(stored(&database).await, Vec::<String>::new());

    let listed = forge.webhook("ping", &json!(["not", "an", "object"]));
    let (status, refused) = deliver(&api, &listed, Some(listed.signature()), listed.body()).await;
    assert_eq!(status, 422, "{refused}");
    assert_eq!(
        refused["error"]["message"],
        "the payload of the delivery is not a JSON object"
    );
    assert_eq!(stored(&database).await, Vec::<String>::new());

    let (status, first) = deliver(&api, &webhook, Some(webhook.signature()), webhook.body()).await;
    assert_eq!(status, 202, "{first}");
    assert_eq!(
        first,
        json!({ "delivery": webhook.delivery(), "stored": true })
    );
    assert_eq!(webhook.send(&api.hooks()).await, 200);
    assert_eq!(stored(&database).await, [webhook.delivery()]);
    processed(&database).await;
}

#[tokio::test]
async fn a_server_without_a_github_app_takes_no_deliveries() {
    let database = TestDatabase::migrated().await;
    let forge = FakeForge::start().await;
    let api = Api::start(&database).await;
    let webhook = forge.webhook("ping", &json!({ "zen": "Design for failure." }));
    let (status, refused) =
        deliver(&api, &webhook, Some(webhook.signature()), webhook.body()).await;
    assert_eq!(status, 404, "{refused}");
    assert_eq!(
        refused["error"]["message"],
        "this server has no GitHub App, so it takes no deliveries at /hooks/github"
    );
    assert_eq!(stored(&database).await, Vec::<String>::new());
}

#[tokio::test]
async fn a_slow_forge_delays_the_processing_and_not_the_acknowledgement() {
    let database = TestDatabase::migrated().await;
    let forge = FakeForge::start().await;
    let installation = forge.install(ACCOUNT, PERMISSIONS);
    forge.create_repository(installation, "GlobalArtInc/ops");
    let api = Api::with_forge(&database, &forge).await;
    let delay = Duration::from_millis(1500);
    forge.delay(delay);

    let webhook = forge.webhook(
        "installation",
        &forge.installation_event(installation, "created"),
    );
    let sent = Instant::now();
    assert_eq!(webhook.send(&api.hooks()).await, 202);
    let acknowledged = sent.elapsed();
    assert!(
        acknowledged < delay,
        "the acknowledgement took {acknowledged:?}"
    );
    processed(&database).await;
    let done = sent.elapsed();
    assert!(
        done >= delay * 2,
        "the processing took {done:?}, so it did not wait for the forge"
    );
    let (status, organisation) = api.get("/api/v1/organisations/globalartinc").await;
    assert_eq!(status, 200, "{organisation}");
}

#[tokio::test]
async fn deliveries_stored_through_two_replicas_are_each_processed_exactly_once() {
    let database = TestDatabase::migrated().await;
    let forge = FakeForge::start().await;
    let installation = forge.install(ACCOUNT, PERMISSIONS);
    forge.create_repository(installation, "GlobalArtInc/ops");
    forge.delay(Duration::from_millis(50));
    let left = Api::with_forge(&database, &forge).await;
    let right = left.replica(&database, &forge).await;

    let mut webhooks = vec![forge.webhook(
        "installation",
        &forge.installation_event(installation, "created"),
    )];
    for _ in 0..15 {
        webhooks.push(forge.webhook(
            "repository",
            &forge.repository_event("GlobalArtInc/ops", "edited"),
        ));
    }
    for (index, webhook) in webhooks.iter().enumerate() {
        let (first, second) = if index % 2 == 0 {
            (&left, &right)
        } else {
            (&right, &left)
        };
        assert_eq!(webhook.send(&first.hooks()).await, 202);
        assert_eq!(webhook.send(&second.hooks()).await, 200);
    }
    let mut expected: Vec<String> = webhooks
        .iter()
        .map(|webhook| webhook.delivery().to_owned())
        .collect();
    expected.sort();
    let mut kept = stored(&database).await;
    kept.sort();
    assert_eq!(kept, expected);
    processed(&database).await;

    let mut counted: BTreeMap<String, usize> = BTreeMap::new();
    let mut replicas = Vec::new();
    for replica in [left, right] {
        let mut processed_here = 0;
        for line in replica.stop().await {
            if message(&line) == "processed a webhook delivery" {
                processed_here += 1;
                *counted
                    .entry(text(&line["delivery"]).to_owned())
                    .or_default() += 1;
            }
        }
        replicas.push(processed_here);
    }
    assert_eq!(
        counted.keys().cloned().collect::<Vec<_>>(),
        expected,
        "the deliveries processed are not the deliveries stored"
    );
    assert!(
        counted.values().all(|times| *times == 1),
        "a delivery was processed more than once: {counted:?}"
    );
    assert!(
        replicas.iter().all(|processed| *processed > 0),
        "a replica processed nothing: {replicas:?}"
    );
}

#[tokio::test]
async fn an_installation_maps_its_account_to_an_organisation_whose_projects_bind_what_it_sees() {
    let database = TestDatabase::migrated().await;
    let forge = FakeForge::start().await;
    let installation = forge.install(ACCOUNT, PERMISSIONS);
    let ops = forge.create_repository(installation, "GlobalArtInc/ops");
    let elsewhere = forge.install("Elsewhere", PERMISSIONS);
    forge.create_repository(elsewhere, "Elsewhere/hidden");
    let api = Api::with_forge(&database, &forge).await;
    let (status, taken) = api
        .post(
            "/api/v1/organisations",
            &json!({ "slug": "elsewhere", "name": "Elsewhere by hand" }),
        )
        .await;
    assert_eq!(status, 201, "{taken}");

    delivered(
        &api,
        &forge,
        "installation",
        &forge.installation_event(installation, "created"),
    )
    .await;
    delivered(
        &api,
        &forge,
        "installation",
        &forge.installation_event(elsewhere, "created"),
    )
    .await;
    processed(&database).await;

    let (status, organisation) = api.get("/api/v1/organisations/globalartinc").await;
    assert_eq!(status, 200, "{organisation}");
    assert_eq!(organisation["name"], ACCOUNT);
    assert_eq!(
        organisation["installation"],
        json!({ "id": installation, "account": ACCOUNT })
    );
    let (status, unmapped) = api.get("/api/v1/organisations/elsewhere").await;
    assert_eq!(status, 200, "{unmapped}");
    assert_eq!(unmapped["installation"], Value::Null);

    let (status, project) = api
        .post(
            "/api/v1/projects",
            &json!({
                "organisation": "globalartinc",
                "slug": "ops",
                "name": "Operations",
                "repository": "globalartinc/OPS"
            }),
        )
        .await;
    assert_eq!(status, 201, "{project}");
    assert_eq!(
        project["repository"],
        json!({ "id": ops, "owner": ACCOUNT, "name": "ops", "default_branch": "main" })
    );
    let (status, shown) = api.get("/api/v1/projects/globalartinc/ops").await;
    assert_eq!((status, &shown), (200, &project));

    let added = forge.create_repository(installation, "GlobalArtInc/added");
    let bind_added = json!({
        "organisation": "globalartinc",
        "slug": "added",
        "name": "Added",
        "repository": "GlobalArtInc/added"
    });
    let (status, refused) = api.post("/api/v1/projects", &bind_added).await;
    assert_eq!(status, 422, "{refused}");
    delivered(
        &api,
        &forge,
        "installation_repositories",
        &forge.installation_repositories_event(
            installation,
            "added",
            &[(added, "GlobalArtInc/added")],
        ),
    )
    .await;
    processed(&database).await;
    let (status, bound) = api.post("/api/v1/projects", &bind_added).await;
    assert_eq!(status, 201, "{bound}");
    assert_eq!(bound["repository"]["id"], json!(added));

    for (repository, message) in [
        (
            "Elsewhere/hidden",
            "the installation of the GitHub App on `GlobalArtInc` cannot see the repository \
             `Elsewhere/hidden`",
        ),
        (
            "GlobalArtInc/missing",
            "the installation of the GitHub App on `GlobalArtInc` cannot see the repository \
             `GlobalArtInc/missing`",
        ),
        (
            "ops",
            "`ops` is not the full name of a repository, `<owner>/<name>`",
        ),
    ] {
        let (status, refused) = api
            .post(
                "/api/v1/projects",
                &json!({
                    "organisation": "globalartinc",
                    "slug": "other",
                    "name": "Other",
                    "repository": repository
                }),
            )
            .await;
        assert_eq!(status, 422, "{repository}: {refused}");
        assert_eq!(refused["error"]["message"], "the project is not valid");
        assert_eq!(
            fields(&refused),
            [("repository".to_owned(), message.to_owned())]
        );
    }
    let (status, conflict) = api
        .post(
            "/api/v1/projects",
            &json!({
                "organisation": "globalartinc",
                "slug": "other",
                "name": "Other",
                "repository": "GlobalArtInc/ops"
            }),
        )
        .await;
    assert_eq!(status, 409, "{conflict}");
    assert_eq!(
        conflict["error"]["message"],
        "the repository `GlobalArtInc/ops` is bound to another project"
    );
    let (status, bare) = api
        .post(
            "/api/v1/projects",
            &json!({ "organisation": "elsewhere", "slug": "bare", "name": "Bare" }),
        )
        .await;
    assert_eq!(status, 201, "{bare}");
    assert_eq!(bare["repository"], Value::Null);
    let (status, refused) = api
        .patch(
            "/api/v1/projects/elsewhere/bare",
            &json!({ "repository": "Elsewhere/hidden" }),
        )
        .await;
    assert_eq!(status, 422, "{refused}");
    assert_eq!(
        fields(&refused),
        [(
            "repository".to_owned(),
            "the organisation `elsewhere` has no installation of the GitHub App, so its projects \
             bind no repository"
                .to_owned()
        )]
    );

    let (status, refused) = api
        .patch(
            "/api/v1/organisations/elsewhere",
            &json!({ "installation": 7 }),
        )
        .await;
    assert_eq!(status, 422, "{refused}");
    assert_eq!(fields(&refused)[0].0, "installation");
    let (status, conflict) = api
        .patch(
            "/api/v1/organisations/elsewhere",
            &json!({ "installation": installation }),
        )
        .await;
    assert_eq!(status, 409, "{conflict}");
    assert_eq!(
        conflict["error"]["message"],
        format!(
            "the installation `{installation}` of the GitHub App belongs to the organisation \
             `globalartinc`"
        )
    );
    let (status, bound) = api
        .patch(
            "/api/v1/organisations/elsewhere",
            &json!({ "installation": elsewhere }),
        )
        .await;
    assert_eq!(status, 200, "{bound}");
    assert_eq!(
        bound["installation"],
        json!({ "id": elsewhere, "account": "Elsewhere" })
    );
    let (status, kept) = api
        .patch("/api/v1/organisations/elsewhere", &json!({}))
        .await;
    assert_eq!((status, &kept), (200, &bound));
    let (status, project) = api
        .patch(
            "/api/v1/projects/elsewhere/bare",
            &json!({ "repository": "Elsewhere/hidden" }),
        )
        .await;
    assert_eq!(status, 200, "{project}");
    assert_eq!(project["repository"]["name"], "hidden");
}

#[tokio::test]
async fn a_repository_renamed_on_the_forge_stays_bound_to_its_project() {
    let database = TestDatabase::migrated().await;
    let forge = FakeForge::start().await;
    let installation = forge.install(ACCOUNT, PERMISSIONS);
    let ops = forge.create_repository(installation, "GlobalArtInc/ops");
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
                "repository": "GlobalArtInc/ops"
            }),
        )
        .await;
    assert_eq!(status, 201, "{project}");

    forge.rename_repository("GlobalArtInc/ops", "operations");
    delivered(
        &api,
        &forge,
        "repository",
        &forge.repository_event("GlobalArtInc/operations", "renamed"),
    )
    .await;
    processed(&database).await;
    let renamed = json!({
        "id": ops,
        "owner": ACCOUNT,
        "name": "operations",
        "default_branch": "main"
    });
    for path in [
        "/api/v1/projects/globalartinc/ops".to_owned(),
        format!("/api/v1/projects/{}", text(&project["id"])),
    ] {
        let (status, shown) = api.get(&path).await;
        assert_eq!(status, 200, "{shown}");
        assert_eq!(shown["repository"], renamed, "{path}");
    }

    let (status, unbound) = api
        .patch(
            "/api/v1/projects/globalartinc/ops",
            &json!({ "repository": null }),
        )
        .await;
    assert_eq!(status, 200, "{unbound}");
    assert_eq!(unbound["repository"], Value::Null);
    let (status, refused) = api
        .patch(
            "/api/v1/projects/globalartinc/ops",
            &json!({ "repository": "GlobalArtInc/ops" }),
        )
        .await;
    assert_eq!(status, 422, "{refused}");
    let (status, bound) = api
        .patch(
            "/api/v1/projects/globalartinc/ops",
            &json!({ "repository": "GlobalArtInc/operations" }),
        )
        .await;
    assert_eq!(status, 200, "{bound}");
    assert_eq!(bound["repository"], renamed);

    delivered(
        &api,
        &forge,
        "installation",
        &forge.installation_event(installation, "deleted"),
    )
    .await;
    processed(&database).await;
    let (status, organisation) = api.get("/api/v1/organisations/globalartinc").await;
    assert_eq!(status, 200, "{organisation}");
    assert_eq!(organisation["installation"], Value::Null);
    let (status, shown) = api.get("/api/v1/projects/globalartinc/ops").await;
    assert_eq!(status, 200, "{shown}");
    assert_eq!(shown["repository"], renamed);
}

#[tokio::test]
async fn a_server_refuses_to_start_with_a_github_app_it_cannot_use() {
    let forge = FakeForge::start().await;
    let launch = || Launch::serve("postgres://bosun@127.0.0.1:1/bosun").github(&forge);
    let cases = [
        (
            launch().env(
                "BOSUN_GITHUB_PRIVATE_KEY_FILE",
                "/nonexistent/bosun/github-app.pem",
            ),
            "could not read the private key of the GitHub App from \
             /nonexistent/bosun/github-app.pem",
        ),
        (
            {
                let launch = launch();
                let key = launch.file("not-a-key.pem", "not a key\n");
                launch.env("BOSUN_GITHUB_PRIVATE_KEY_FILE", &key.to_string_lossy())
            },
            "the GitHub App cannot be used: the forge is not configured correctly",
        ),
        (
            {
                let launch = launch();
                let secret = launch.file("empty-secret", " \n");
                launch.env(
                    "BOSUN_GITHUB_WEBHOOK_SECRET_FILE",
                    &secret.to_string_lossy(),
                )
            },
            "the webhook secret of the GitHub App in",
        ),
    ];
    for (launch, expected) in cases {
        let (status, lines) = launch.spawn().exit().await;
        assert!(!status.success(), "{lines:#?}");
        assert!(
            errors(&lines).iter().any(|error| error.contains(expected)),
            "expected an error containing `{expected}`: {lines:#?}"
        );
    }
}
