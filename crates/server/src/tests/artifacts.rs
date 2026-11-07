use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use bosun_blob::Store;
use bosun_services::{Index, IndexError, NewArtifact};
use bosun_types::id::{AttemptId, ExecutionId, RunId};
use chrono::{DateTime, TimeDelta, Utc};
use sea_orm::prelude::Uuid;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

use super::access::Site;
use super::queue::{LABELS, LEASE, claim, connected};
use super::{template, workflow};
use crate::clock::Clock;
use crate::job_token;
use crate::retention::{Removed, round};
use crate::services::{self, Postgres};
use crate::site::{Retention, Site as Public};

const ORIGIN: &str = "https://ci.example.org";

const BLOCK: &str = "YmxvY2stMDAwMQ==";

fn start() -> DateTime<Utc> {
    DateTime::from_timestamp(1_790_000_000, 0).expect("a time")
}

struct Job {
    attempt: AttemptId,
    execution: ExecutionId,
    token: String,
}

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "{error}: {} {}",
                self.status,
                String::from_utf8_lossy(&self.body)
            )
        })
    }
}

struct Runtime {
    site: Site,
    clock: Clock,
    router: Router,
    replica: Uuid,
    agents: u32,
}

impl Runtime {
    async fn new() -> Runtime {
        let clock = Clock::manual(start());
        let site = Site::configured(clock.clone(), Public::new(Some(ORIGIN.to_owned()))).await;
        let router = services::routes(&site.state).expect("the runtime routes");
        Runtime {
            site,
            clock,
            router,
            replica: Uuid::now_v7(),
            agents: 0,
        }
    }

    async fn job(&mut self) -> Job {
        self.agents += 1;
        let agent = connected(
            &self.site.world,
            &format!("runner1-{}", self.agents),
            &LABELS,
            self.replica,
        )
        .await;
        let claimed = claim(self.site.db(), agent.id, self.replica, LEASE, &[])
            .await
            .expect("a job for the agent");
        let token = job_token::runtime(
            self.site.db(),
            &self.site.state.keys,
            &self.site.state.site,
            &claimed,
            self.clock.now(),
        )
        .await
        .expect("a runtime")
        .expect("a runtime for a server with a public address")
        .token;
        Job {
            attempt: claimed.attempt_id,
            execution: claimed.execution_id,
            token,
        }
    }

    async fn send(
        &self,
        method: Method,
        target: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
    ) -> Answer {
        let mut request = Request::builder().method(method).uri(target);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = self
            .router
            .clone()
            .oneshot(request.body(Body::from(body)).expect("a request"))
            .await
            .expect("an answer");
        Answer {
            status: response.status(),
            headers: response.headers().clone(),
            body: to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("a body")
                .to_vec(),
        }
    }

    async fn twirp(&self, token: Option<&str>, method: &str, body: Value) -> Answer {
        let target = format!("/twirp/github.actions.results.api.v1.ArtifactService/{method}");
        let bearer = token.map(|token| format!("Bearer {token}"));
        let mut headers = vec![("content-type", "application/json")];
        if let Some(bearer) = &bearer {
            headers.push(("authorization", bearer));
        }
        self.send(
            Method::POST,
            &target,
            &headers,
            body.to_string().into_bytes(),
        )
        .await
    }

    async fn call(&self, job: &Job, method: &str, extra: Value) -> Answer {
        let mut body = json!({
            "workflow_run_backend_id": job.attempt.to_string(),
            "workflow_job_run_backend_id": job.execution.to_string(),
        });
        body.as_object_mut()
            .expect("an object")
            .extend(extra.as_object().expect("an object").clone());
        self.twirp(Some(&job.token), method, body).await
    }

    async fn upload(&self, job: &Job, name: &str, content: &[u8]) -> String {
        self.upload_until(job, name, content, json!({})).await
    }

    async fn upload_until(&self, job: &Job, name: &str, content: &[u8], asked: Value) -> String {
        let mut request = json!({ "name": name });
        request
            .as_object_mut()
            .expect("an object")
            .extend(asked.as_object().expect("an object").clone());
        let created = self.call(job, "CreateArtifact", request).await;
        assert_eq!(created.status, StatusCode::OK, "{:?}", created.json());
        let address = target(
            created.json()["signed_upload_url"]
                .as_str()
                .expect("an address"),
        );
        let block = self
            .send(
                Method::PUT,
                &format!("{address}&comp=block&blockid={BLOCK}"),
                &[],
                content.to_vec(),
            )
            .await;
        assert_eq!(block.status, StatusCode::CREATED);
        let list = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList><Latest>{BLOCK}</Latest></BlockList>"
        );
        let committed = self
            .send(
                Method::PUT,
                &format!("{address}&comp=blocklist"),
                &[],
                list.into_bytes(),
            )
            .await;
        assert_eq!(committed.status, StatusCode::CREATED);
        let hash = format!("sha256:{:x}", Sha256::digest(content));
        let finalized = self
            .call(
                job,
                "FinalizeArtifact",
                json!({ "name": name, "size": content.len().to_string(), "hash": hash }),
            )
            .await;
        assert_eq!(finalized.status, StatusCode::OK, "{:?}", finalized.json());
        finalized.json()["artifact_id"]
            .as_str()
            .expect("an id")
            .to_owned()
    }

    async fn signed(&self, asker: &Job, owner: &Job, name: &str) -> Answer {
        let body = json!({
            "workflow_run_backend_id": owner.attempt.to_string(),
            "workflow_job_run_backend_id": owner.execution.to_string(),
            "name": name,
        });
        self.twirp(Some(&asker.token), "GetSignedArtifactURL", body)
            .await
    }

    async fn download(&self, asker: &Job, owner: &Job, name: &str) -> Answer {
        let signed = self.signed(asker, owner, name).await;
        assert_eq!(signed.status, StatusCode::OK, "{:?}", signed.json());
        let address = target(signed.json()["signed_url"].as_str().expect("an address"));
        self.send(Method::GET, &address, &[], Vec::new()).await
    }

    async fn stored(&self, prefix: &str) -> Vec<String> {
        self.site
            .state
            .store
            .list(prefix)
            .await
            .expect("a listing")
            .into_iter()
            .map(|key| key.to_string())
            .collect()
    }
}

fn target(address: &str) -> String {
    address
        .strip_prefix(ORIGIN)
        .unwrap_or_else(|| panic!("{address} is not under {ORIGIN}"))
        .to_owned()
}

fn two_jobs() -> crate::tests::ir::Workflow {
    workflow(vec![template("one", &[], 1), template("two", &[], 1)])
}

#[tokio::test]
async fn the_toolkit_protocol_uploads_in_blocks_shares_with_the_attempt_and_overwrites_by_name() {
    let mut runtime = Runtime::new().await;
    runtime.site.world.dispatch(&two_jobs()).await;
    let one = runtime.job().await;
    let two = runtime.job().await;
    assert_eq!(one.attempt, two.attempt);

    let first = runtime.upload(&one, "greeting", b"hello from one").await;
    let listed = runtime.call(&two, "ListArtifacts", json!({})).await.json();
    assert_eq!(listed["artifacts"][0]["database_id"], first);
    assert_eq!(listed["artifacts"][0]["size"], "14");
    let downloaded = runtime.download(&two, &one, "greeting").await;
    assert_eq!(downloaded.status, StatusCode::OK);
    assert_eq!(downloaded.body, b"hello from one");
    assert_eq!(
        downloaded.headers["content-disposition"],
        "attachment; filename=\"greeting\"; filename*=UTF-8''greeting"
    );

    let refused = runtime
        .call(&one, "CreateArtifact", json!({ "name": "greeting" }))
        .await;
    assert_eq!(refused.status, StatusCode::CONFLICT);
    let deleted = runtime
        .twirp(
            Some(&two.token),
            "DeleteArtifact",
            json!({
                "workflow_run_backend_id": one.attempt.to_string(),
                "workflow_job_run_backend_id": one.execution.to_string(),
                "name": "greeting",
            }),
        )
        .await;
    assert_eq!(deleted.status, StatusCode::OK, "{:?}", deleted.json());
    assert!(runtime.stored("artifacts/").await.is_empty());
    let second = runtime.upload(&one, "greeting", b"hello again").await;
    assert_ne!(first, second);
    let replaced = runtime.download(&two, &one, "greeting").await;
    assert_eq!(replaced.body, b"hello again");
    assert_eq!(runtime.stored("artifacts/").await.len(), 1);
    assert!(runtime.stored("blocks/").await.is_empty());

    let first_try = runtime
        .call(&one, "CreateArtifact", json!({ "name": "retried" }))
        .await
        .json();
    let first_try = target(first_try["signed_upload_url"].as_str().expect("an address"));
    let second_try = runtime
        .call(&one, "CreateArtifact", json!({ "name": "retried" }))
        .await;
    assert_eq!(second_try.status, StatusCode::OK, "{:?}", second_try.json());
    let stale = runtime
        .send(
            Method::PUT,
            &format!("{first_try}&comp=block&blockid={BLOCK}"),
            &[],
            b"late".to_vec(),
        )
        .await;
    assert_eq!(stale.status, StatusCode::NOT_FOUND);
    let other_job = runtime
        .call(&two, "CreateArtifact", json!({ "name": "retried" }))
        .await;
    assert_eq!(other_job.status, StatusCode::CONFLICT);

    let viewer = runtime.site.user("viewer").await;
    let organisation = runtime.site.project().await.organisation_id;
    runtime
        .site
        .bind(
            "viewer",
            bosun_db::roles::Subject::User(viewer.0),
            organisation,
            None,
        )
        .await;
    let run = RunId::try_from(
        sea_orm::ConnectionTrait::query_one(
            runtime.site.db(),
            sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "SELECT run_id FROM attempt WHERE id = $1",
                [Uuid::from(one.attempt).into()],
            ),
        )
        .await
        .expect("a query")
        .expect("the attempt")
        .try_get::<Uuid>("", "run_id")
        .expect("a run"),
    )
    .expect("a run id");
    let rest = runtime
        .site
        .get(&format!("/api/v1/runs/{run}/artifacts"), &viewer.1)
        .await;
    assert_eq!(rest.status, StatusCode::OK, "{rest:?}");
    assert_eq!(rest.body["items"][0]["id"].to_string(), second);
    assert_eq!(rest.body["items"][0]["name"], "greeting");
    assert_eq!(rest.body["items"][0]["size"], 11);

    runtime.upload(&one, "second", b"2").await;
    runtime.upload(&one, "third", b"3").await;
    let list = format!("/api/v1/runs/{run}/artifacts");
    let mut names = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let path = match &cursor {
            Some(next) => format!("{list}?limit=1&cursor={next}"),
            None => format!("{list}?limit=1"),
        };
        let page = runtime.site.get(&path, &viewer.1).await;
        assert_eq!(page.status, StatusCode::OK, "{page:?}");
        let items = page.body["items"].as_array().expect("items");
        assert_eq!(items.len(), 1, "{page:?}");
        names.push(items[0]["name"].as_str().expect("a name").to_owned());
        pages += 1;
        assert!(pages <= 3, "{names:?}");
        cursor = page.body["next"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(names, ["greeting", "second", "third"]);
    for foreign in [
        "not-a-cursor".to_owned(),
        Uuid::from_u128(u128::MAX).to_string(),
    ] {
        let refused = runtime
            .site
            .get(&format!("{list}?limit=1&cursor={foreign}"), &viewer.1)
            .await;
        assert_eq!(
            refused.status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "{refused:?}"
        );
    }
}

#[tokio::test]
async fn a_signed_address_that_expired_or_names_another_artifact_is_403_and_another_runs_token_reaches_nothing()
 {
    let mut runtime = Runtime::new().await;
    runtime
        .site
        .world
        .dispatch(&workflow(vec![template("one", &[], 1)]))
        .await;
    let one = runtime.job().await;
    runtime.upload(&one, "first", b"first content").await;
    runtime.upload(&one, "second", b"second content").await;

    let signed = runtime.signed(&one, &one, "first").await.json();
    let address = target(signed["signed_url"].as_str().expect("an address"));
    let fresh = runtime.send(Method::GET, &address, &[], Vec::new()).await;
    assert_eq!(fresh.status, StatusCode::OK);
    let other = runtime.signed(&one, &one, "second").await.json();
    let other = target(other["signed_url"].as_str().expect("an address"));
    let (path, query) = other.split_once('?').expect("a query");
    let (own_path, own_query) = address.split_once('?').expect("a query");
    let refused = vec![
        format!("{path}?{own_query}"),
        format!("{own_path}?{query}"),
        format!("{own_path}?{}x", own_query),
    ];
    let mut bodies = Vec::new();
    for uri in &refused {
        let answer = runtime.send(Method::GET, uri, &[], Vec::new()).await;
        assert_eq!(answer.status, StatusCode::FORBIDDEN, "{uri}");
        bodies.push(answer.body);
    }
    let upload_on_download = runtime.send(Method::PUT, &address, &[], Vec::new()).await;
    assert_eq!(upload_on_download.status, StatusCode::FORBIDDEN);
    runtime.clock.advance(TimeDelta::minutes(61));
    let expired = runtime.send(Method::GET, &address, &[], Vec::new()).await;
    assert_eq!(expired.status, StatusCode::FORBIDDEN);
    bodies.push(expired.body);
    assert!(
        bodies.windows(2).all(|pair| pair[0] == pair[1]),
        "the refusals differ"
    );

    runtime
        .site
        .world
        .dispatch(&workflow(vec![template("elsewhere", &[], 1)]))
        .await;
    let outsider = runtime.job().await;
    assert_ne!(outsider.attempt, one.attempt);
    let foreign = json!({
        "workflow_run_backend_id": one.attempt.to_string(),
        "workflow_job_run_backend_id": one.execution.to_string(),
        "name": "first",
    });
    for method in ["ListArtifacts", "CreateArtifact", "FinalizeArtifact"] {
        let answer = runtime
            .twirp(Some(&outsider.token), method, foreign.clone())
            .await;
        assert_eq!(answer.status, StatusCode::FORBIDDEN, "{method}");
    }
    for method in ["GetSignedArtifactURL", "DeleteArtifact"] {
        let answer = runtime
            .twirp(Some(&outsider.token), method, foreign.clone())
            .await;
        assert_eq!(answer.status, StatusCode::NOT_FOUND, "{method}");
    }
    let own = runtime
        .call(&outsider, "ListArtifacts", json!({}))
        .await
        .json();
    assert_eq!(own["artifacts"], json!([]));
    assert_eq!(runtime.stored("artifacts/").await.len(), 2);
    let anonymous = runtime.twirp(None, "ListArtifacts", foreign.clone()).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_rest_download_serves_a_range_a_viewer_cannot_delete_and_an_override_removes_the_object()
 {
    let site = Site::new().await;
    let (run, job) = site.run().await;
    let execution = site.world.assign(run, job).await;
    let attempt = AttemptId::try_from(
        sea_orm::ConnectionTrait::query_one(
            site.db(),
            sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Postgres,
                "SELECT j.attempt_id FROM execution e JOIN job j ON j.id = e.job_id WHERE e.id = $1",
                [Uuid::from(execution).into()],
            ),
        )
        .await
        .expect("a query")
        .expect("the execution")
        .try_get::<Uuid>("", "attempt_id")
        .expect("an attempt"),
    )
    .expect("an attempt id");
    let index = Postgres::new(site.db().clone());
    let artifact = index
        .create_artifact(NewArtifact {
            attempt,
            execution,
            name: "dist/report.txt".to_owned(),
            content_type: "text/plain".to_owned(),
            created_at: Utc::now(),
            expires_at: None,
        })
        .await
        .expect("an artifact");
    site.state
        .store
        .put(&artifact.key, &b"0123456789"[..])
        .await
        .expect("an object");
    index
        .finalize_artifact(
            artifact.id,
            bosun_services::Finalized {
                size: 10,
                sha256: None,
                at: Utc::now(),
            },
        )
        .await
        .expect("a finalisation");
    let organisation = site.project().await.organisation_id;
    let (viewer, viewing) = site.user("viewer").await;
    site.bind(
        "viewer",
        bosun_db::roles::Subject::User(viewer),
        organisation,
        None,
    )
    .await;
    let (maintainer, maintaining) = site.user("maintainer").await;
    site.bind(
        "maintainer",
        bosun_db::roles::Subject::User(maintainer),
        organisation,
        None,
    )
    .await;
    let (_, strange) = site.user("stranger").await;
    let path = format!("/api/v1/artifacts/{}", artifact.id);

    let whole = site
        .send_with(Method::GET, &path, &viewing, &[], None)
        .await;
    assert_eq!(whole.status, StatusCode::OK, "{whole:?}");
    assert_eq!(whole.headers["content-length"], "10");
    assert_eq!(whole.headers["accept-ranges"], "bytes");
    assert_eq!(
        whole.headers["content-disposition"],
        "attachment; filename=\"report.txt\"; filename*=UTF-8''report.txt"
    );
    assert_eq!(
        whole.headers["content-security-policy"],
        "default-src 'none'; sandbox"
    );
    let ranged = site
        .send_with(
            Method::GET,
            &path,
            &viewing,
            &[("range", "bytes=2-5".to_owned())],
            None,
        )
        .await;
    assert_eq!(ranged.status, StatusCode::PARTIAL_CONTENT, "{ranged:?}");
    assert_eq!(ranged.headers["content-range"], "bytes 2-5/10");
    assert_eq!(ranged.body, "2345");
    let outside = site
        .send_with(
            Method::GET,
            &path,
            &viewing,
            &[("range", "bytes=10-".to_owned())],
            None,
        )
        .await;
    assert_eq!(outside.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(outside.headers["content-range"], "bytes */10");
    let tail = site
        .send_with(
            Method::GET,
            &path,
            &viewing,
            &[("range", "bytes=-4".to_owned())],
            None,
        )
        .await;
    assert_eq!(tail.status, StatusCode::PARTIAL_CONTENT, "{tail:?}");
    assert_eq!(tail.headers["content-range"], "bytes 6-9/10");
    assert_eq!(tail.body, "6789");
    let nothing = site
        .send_with(
            Method::GET,
            &path,
            &viewing,
            &[("range", "bytes=-0".to_owned())],
            None,
        )
        .await;
    assert_eq!(
        nothing.status,
        StatusCode::RANGE_NOT_SATISFIABLE,
        "{nothing:?}"
    );
    assert_eq!(nothing.headers["content-range"], "bytes */10");
    let hidden = site
        .send_with(Method::GET, &path, &strange, &[], None)
        .await;
    assert_eq!(hidden.status, StatusCode::NOT_FOUND);

    let refused = site
        .send_with(Method::DELETE, &path, &viewing, &[], None)
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{refused:?}");
    assert_eq!(
        site.state
            .store
            .list("artifacts/")
            .await
            .expect("a listing")
            .len(),
        1
    );
    let removed = site
        .send_with(Method::DELETE, &path, &maintaining, &[], None)
        .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT, "{removed:?}");
    assert!(
        site.state
            .store
            .list("artifacts/")
            .await
            .expect("a listing")
            .is_empty()
    );
    let gone = site
        .send_with(Method::GET, &path, &viewing, &[], None)
        .await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn retention_and_the_clean_up_of_abandoned_uploads_remove_rows_blocks_and_objects() {
    let mut runtime = Runtime::new().await;
    runtime.site.world.dispatch(&two_jobs()).await;
    let one = runtime.job().await;
    let kept = runtime
        .call(
            &one,
            "CreateArtifact",
            json!({ "name": "kept", "expires_at": (start() + TimeDelta::days(30)).to_rfc3339() }),
        )
        .await;
    assert_eq!(kept.status, StatusCode::OK);
    runtime
        .upload_until(
            &one,
            "short",
            b"short lived",
            json!({ "expires_at": (start() + TimeDelta::days(2)).to_rfc3339() }),
        )
        .await;
    let created = runtime
        .call(
            &one,
            "CreateArtifact",
            json!({ "name": "half", "expires_at": (start() + TimeDelta::days(1)).to_rfc3339() }),
        )
        .await;
    let address = target(
        created.json()["signed_upload_url"]
            .as_str()
            .expect("an address"),
    );
    let staged = runtime
        .send(
            Method::PUT,
            &format!("{address}&comp=block&blockid={BLOCK}"),
            &[],
            b"half".to_vec(),
        )
        .await;
    assert_eq!(staged.status, StatusCode::CREATED);
    assert_eq!(runtime.stored("blocks/").await.len(), 1);
    assert_eq!(runtime.stored("artifacts/").await.len(), 1);

    let swept = |clock: &Clock| {
        let site = &runtime.site;
        let now = clock.now();
        async move {
            round(
                site.db(),
                site.state.store.as_ref(),
                now,
                Retention::default(),
            )
            .await
            .expect("a round")
            .expect("the lock of retention")
        }
    };
    let early = swept(&runtime.clock).await;
    assert_eq!(early, Removed::default());
    runtime.clock.advance(TimeDelta::hours(25));
    let abandoned = swept(&runtime.clock).await;
    assert_eq!(abandoned.uploads, 2, "{abandoned:?}");
    assert_eq!(abandoned.artifacts, 0, "{abandoned:?}");
    assert!(runtime.stored("blocks/").await.is_empty());
    let after_clean_up = runtime.stored("artifacts/").await;
    assert_eq!(after_clean_up.len(), 1, "{after_clean_up:?}");
    runtime.clock.advance(TimeDelta::days(2));
    let expired = swept(&runtime.clock).await;
    assert_eq!(expired.artifacts, 1, "{expired:?}");
    assert!(runtime.stored("artifacts/").await.is_empty());
    let left: Vec<String> =
        bosun_db::artifacts::of_attempt(runtime.site.db(), one.attempt, None, None)
            .await
            .expect("a query")
            .into_iter()
            .map(|artifact| artifact.name)
            .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[tokio::test]
async fn an_artifact_never_outlives_ninety_days_and_a_request_for_less_is_kept() {
    let mut runtime = Runtime::new().await;
    runtime.site.world.dispatch(&two_jobs()).await;
    let one = runtime.job().await;
    let index = Postgres::new(runtime.site.db().clone());
    let mut expiries = Vec::new();
    for (name, asked) in [
        ("none", None),
        ("long", Some(start() + TimeDelta::days(400))),
        ("short", Some(start() + TimeDelta::days(5))),
    ] {
        let artifact = index
            .create_artifact(NewArtifact {
                attempt: one.attempt,
                execution: one.execution,
                name: name.to_owned(),
                content_type: "application/zip".to_owned(),
                created_at: start(),
                expires_at: asked,
            })
            .await
            .expect("an artifact");
        expiries.push(artifact.expires_at.expect("an expiry") - start());
    }
    assert_eq!(
        expiries,
        [TimeDelta::days(90), TimeDelta::days(90), TimeDelta::days(5)]
    );
    let again = index
        .create_artifact(NewArtifact {
            attempt: one.attempt,
            execution: one.execution,
            name: "none".to_owned(),
            content_type: "application/zip".to_owned(),
            created_at: start(),
            expires_at: None,
        })
        .await;
    assert_eq!(again, Err(IndexError::AlreadyExists));
}

#[tokio::test]
async fn a_server_without_a_public_address_serves_no_runtime_route_and_one_with_a_path_refuses_to_start()
 {
    let site = Site::new().await;
    let router = services::routes(&site.state).expect("no routes");
    for (method, uri) in [
        (
            Method::POST,
            "/twirp/github.actions.results.api.v1.ArtifactService/ListArtifacts",
        ),
        (Method::GET, "/_runtime/blob/artifacts/1"),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .expect("a request"),
            )
            .await
            .expect("an answer");
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
    }

    let mut prefixed = Site::new().await;
    prefixed.state.site = Public::new(Some("https://ci.example.org/bosun".to_owned()));
    let refused = services::routes(&prefixed.state);
    assert!(
        matches!(refused, Err(crate::Error::RuntimeServices(_))),
        "{:?}",
        refused.map(|_| ())
    );
}
