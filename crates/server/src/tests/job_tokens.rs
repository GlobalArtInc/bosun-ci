use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::header::AUTHORIZATION;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::{Extension, Router, middleware};
use bosun_db::entity::signing_key;
use bosun_db::queue::ClaimedJob;
use bosun_services::JobToken;
use bosun_types::status::Conclusion;
use chrono::{DateTime, TimeDelta, Utc};
use sea_orm::prelude::Uuid;
use sea_orm::{DatabaseConnection, EntityTrait};
use tokio::sync::Barrier;
use tower::ServiceExt;

use super::queue::{LABELS, LEASE, assigned, claim, connected};
use super::{KEK, World, finished, id, site, state, template, workflow};
use crate::AppState;
use crate::agents::progress::{self, Reporter};
use crate::auth::Refusal;
use crate::clock::Clock;
use crate::job_token::{self, MAX_LIFETIME, authentication, lifetime};
use crate::keys::{JOB_TOKEN_KEY, Kek, Keyring, Unreadable};
use crate::signing_key::{self as keys_of_jobs, Failure};
use crate::site::Site;

const OTHER_KEK: &str = "f0e1d2c3b4a5968778695a4b3c2d1e0f00112233445566778899aabbccddeeff";

const DAY: Duration = Duration::from_secs(24 * 3600);

fn start() -> DateTime<Utc> {
    DateTime::from_timestamp(1_790_000_000, 0).expect("a time")
}

fn other_keys() -> Keyring {
    Keyring::new(
        Kek::parse(OTHER_KEK, "the tests").expect("a KEK"),
        Vec::new(),
    )
}

struct Fixture {
    world: World,
    state: AppState,
    clock: Clock,
    agent: Reporter,
    replica: Uuid,
    _directory: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Fixture {
        let world = World::new().await;
        let directory = tempfile::tempdir().expect("a directory");
        let clock = Clock::manual(start());
        let state = state(
            &world.database,
            directory.path(),
            clock.clone(),
            crate::sign_in::Settings::new(Vec::new(), 1000, directory.path().join("data")),
        )
        .await;
        let replica = Uuid::now_v7();
        let agent = connected(&world, "runner1-1", &LABELS, replica).await;
        Fixture {
            world,
            state,
            clock,
            agent,
            replica,
            _directory: directory,
        }
    }

    fn db(&self) -> &DatabaseConnection {
        self.world.db()
    }

    async fn claim(&self) -> ClaimedJob {
        claim(self.db(), self.agent.id, self.replica, LEASE, &[])
            .await
            .expect("a job for the agent")
    }

    async fn token(&self, claimed: &ClaimedJob) -> String {
        job_token::runtime(
            self.db(),
            &self.state.keys,
            &site(),
            claimed,
            self.clock.now(),
        )
        .await
        .expect("a runtime")
        .expect("a runtime for a server with a public address")
        .token
    }

    async fn finish(&self, claimed: &ClaimedJob) {
        progress::accepted(
            self.db(),
            &self.agent,
            claimed.execution_id,
            &Site::default(),
        )
        .await
        .expect("an acceptance");
        progress::job_finished(
            self.db(),
            &self.agent,
            claimed.execution_id,
            finished(Conclusion::Success),
            &Site::default(),
        )
        .await
        .expect("a finished job");
    }

    fn probe(&self) -> Router {
        Router::new()
            .route(
                "/probe",
                get(|Extension(token): Extension<JobToken>| async move {
                    token.identity().execution.to_string()
                }),
            )
            .route_layer(middleware::from_fn_with_state(
                self.state.clone(),
                authentication::authenticated,
            ))
            .with_state(self.state.clone())
    }

    async fn call(&self, authorization: &[&str]) -> (StatusCode, String) {
        let mut request = Request::builder().uri("/probe");
        for value in authorization {
            request = request.header(AUTHORIZATION, *value);
        }
        let response = self
            .probe()
            .oneshot(request.body(Body::empty()).expect("a request"))
            .await
            .expect("a response");
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .expect("a body");
        (status, String::from_utf8(body.to_vec()).expect("UTF-8"))
    }

    async fn status_with(&self, token: &str) -> StatusCode {
        self.call(&[&format!("Bearer {token}")]).await.0
    }

    async fn dispatch_two(&self) {
        self.world
            .dispatch(&workflow(vec![
                template("one", &[], 1),
                template("two", &[], 1),
            ]))
            .await;
    }
}

#[tokio::test]
async fn a_job_without_actions_or_a_server_without_a_public_address_assigns_no_job_token_and_makes_no_key()
 {
    let world = World::new().await;
    let replica = Uuid::now_v7();
    world
        .dispatch(&workflow(vec![template("one", &[], 1)]))
        .await;
    let agent = connected(&world, "runner1-1", &LABELS, replica).await;
    let claimed = claim(world.db(), agent.id, replica, LEASE, &[])
        .await
        .expect("a claim");
    let without_an_address = job_token::runtime(
        world.db(),
        &Keyring::new(Kek::parse(KEK, "the tests").expect("a KEK"), Vec::new()),
        &Site::default(),
        &claimed,
        Utc::now(),
    )
    .await
    .expect("no failure");
    assert!(without_an_address.is_none());
    let (assignment, documents) = assigned(&world, claimed).await;
    assert!(documents.actions.is_empty());
    assert_eq!(documents.evaluated.runtime, None);
    assert!(
        !assignment.evaluated.contains("runtime"),
        "{}",
        assignment.evaluated
    );
    assert!(
        bosun_db::signing_keys::public_keys(world.db())
            .await
            .expect("the public keys")
            .is_empty(),
        "an assignment that needs no job token made a key"
    );
}

#[tokio::test]
async fn a_request_with_a_malformed_token_is_refused_without_a_query() {
    let mut headers = axum::http::HeaderMap::new();
    for token in [
        "a.b",
        "a.b.c.d",
        "a..c",
        "a.b.c=",
        "a.b.c d",
        &format!("a.b.{}", "c".repeat(4096)),
    ] {
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {token}").parse().expect("a header"),
        );
        let refused =
            authentication::authenticate(&DatabaseConnection::Disconnected, Utc::now(), &headers)
                .await;
        assert!(
            matches!(
                refused,
                Err(Refusal::Unauthenticated("the job token is malformed"))
            ),
            "{token}"
        );
    }
}

#[test]
fn the_longest_job_token_lives_eight_days_and_three_hours_and_any_timeout_adds_twenty_seven_hours()
{
    assert_eq!(MAX_LIFETIME.num_seconds(), 702_000);
    assert_eq!(lifetime(0).num_seconds(), 702_000);
    assert_eq!(lifetime(u32::MAX).num_seconds(), 702_000);
    assert_eq!(lifetime(10_080).num_seconds(), 702_000);
    assert_eq!(lifetime(360).num_seconds(), 118_800);
    assert_eq!(lifetime(1).num_seconds(), 97_260);
}

#[tokio::test]
async fn a_retired_key_is_kept_until_eight_days_four_hours_after_its_retirement() {
    let world = World::new().await;
    let db = world.db();
    let keys = Keyring::new(Kek::parse(KEK, "the tests").expect("a KEK"), Vec::new());
    let first = keys_of_jobs::signer(db, &keys, start())
        .await
        .expect("a signing key");
    let retired = start() + TimeDelta::hours(25);
    let last = first
        .mint(
            bosun_services::JobIdentity {
                attempt: id(Uuid::now_v7()),
                execution: id(Uuid::now_v7()),
            },
            retired,
            retired + TimeDelta::seconds(702_000),
        )
        .expect("a token");
    assert!(
        keys_of_jobs::sweep_once(db, &keys, retired, DAY)
            .await
            .expect("a sweep")
            .expect("the lock")
            .rotated
    );
    let forever = Duration::from_secs(3650 * 24 * 3600);
    let before_expiry = retired + TimeDelta::seconds(702_000 - 1);
    let sweep = |at: DateTime<Utc>| keys_of_jobs::sweep_once(db, &keys, at, forever);

    let swept = sweep(retired + TimeDelta::seconds(702_000))
        .await
        .expect("a sweep")
        .expect("the lock");
    assert_eq!(swept.purged, 0);
    authentication::verifying_keys(db)
        .await
        .expect("the keys")
        .verify(&last, before_expiry)
        .expect("a token of a retired key that has not been purged");

    let swept = sweep(retired + TimeDelta::seconds(702_000 + 3601))
        .await
        .expect("a sweep")
        .expect("the lock");
    assert_eq!(swept.purged, 1);
    assert!(
        authentication::verifying_keys(db)
            .await
            .expect("the keys")
            .verify(&last, before_expiry)
            .is_err()
    );
}

#[tokio::test]
async fn a_job_token_is_refused_when_its_execution_has_completed_or_it_is_altered_expired_or_malformed()
 {
    let fixture = Fixture::new().await;
    fixture.dispatch_two().await;
    let first = fixture.claim().await;
    let token = fixture.token(&first).await;
    let (status, body) = fixture.call(&[&format!("Bearer {token}")]).await;
    assert_eq!(
        (status, body),
        (StatusCode::OK, first.execution_id.to_string())
    );
    let (header, claims, signature) = {
        let mut parts = token.split('.');
        (
            parts.next().expect("a header"),
            parts.next().expect("claims"),
            parts.next().expect("a signature"),
        )
    };
    let swapped = |text: &str| {
        let middle = text.len() / 2;
        let other = if &text[middle..=middle] == "A" {
            "B"
        } else {
            "A"
        };
        format!("{}{other}{}", &text[..middle], &text[middle + 1..])
    };
    for altered in [
        format!("{header}.{}.{signature}", swapped(claims)),
        format!("{header}.{claims}.{}", swapped(signature)),
        format!("{}.{claims}.{signature}", swapped(header)),
    ] {
        assert_eq!(
            fixture.status_with(&altered).await,
            StatusCode::UNAUTHORIZED,
            "{altered}"
        );
    }
    for headers in [
        vec![],
        vec!["Bearer"],
        vec!["Bearer "],
        vec!["Basic dXNlcjpwYXNz"],
        vec![token.as_str()],
        vec!["Bearer a.b"],
    ] {
        let (status, _) = fixture.call(&headers).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{headers:?}");
    }
    let two = format!("Bearer {token}");
    assert_eq!(
        fixture.call(&[&two, &two]).await.0,
        StatusCode::UNAUTHORIZED
    );

    fixture.finish(&first).await;
    let (status, body) = fixture.call(&[&format!("Bearer {token}")]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("has completed"), "{body}");

    let second = fixture.claim().await;
    let token = fixture.token(&second).await;
    assert_eq!(fixture.status_with(&token).await, StatusCode::OK);
    fixture.clock.advance(lifetime(360) + TimeDelta::minutes(1));
    let (status, body) = fixture.call(&[&format!("Bearer {token}")]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("has expired"), "{body}");
    assert!(!body.contains(&token), "{body}");
}

#[tokio::test]
async fn a_token_of_the_previous_key_holds_after_a_rotation_and_one_of_a_removed_key_is_refused() {
    let fixture = Fixture::new().await;
    fixture.dispatch_two().await;
    let claimed = fixture.claim().await;
    let keys = &fixture.state.keys;
    let before = fixture.token(&claimed).await;
    let long_lived = keys_of_jobs::signer(fixture.db(), keys, fixture.clock.now())
        .await
        .expect("the signing key")
        .mint(
            bosun_services::JobIdentity {
                attempt: claimed.attempt_id,
                execution: claimed.execution_id,
            },
            fixture.clock.now(),
            fixture.clock.now() + TimeDelta::days(365),
        )
        .expect("a token");
    assert_eq!(fixture.status_with(&before).await, StatusCode::OK);

    fixture.clock.advance(TimeDelta::hours(25));
    let swept = keys_of_jobs::sweep_once(fixture.db(), keys, fixture.clock.now(), DAY)
        .await
        .expect("a sweep")
        .expect("the lock");
    assert!(swept.rotated);
    let after = fixture.token(&claimed).await;
    assert_ne!(
        before.split('.').next(),
        after.split('.').next(),
        "the new token names the old key"
    );
    for token in [&before, &after, &long_lived] {
        assert_eq!(fixture.status_with(token).await, StatusCode::OK);
    }

    fixture.clock.advance(MAX_LIFETIME + TimeDelta::hours(2));
    let swept = keys_of_jobs::sweep_once(fixture.db(), keys, fixture.clock.now(), DAY)
        .await
        .expect("a sweep")
        .expect("the lock");
    assert_eq!(swept.purged, 1);
    assert_eq!(
        fixture.status_with(&long_lived).await,
        StatusCode::UNAUTHORIZED,
        "a token of a removed key was accepted although it had not expired"
    );
    let fresh = fixture.token(&claimed).await;
    assert_eq!(fixture.status_with(&fresh).await, StatusCode::OK);
}

#[tokio::test]
async fn replicas_that_start_together_end_up_with_one_key_that_only_the_kek_opens_and_rewrap_moves()
{
    let world = World::new().await;
    let db = world.db().clone();
    let replicas = {
        let url = world.database.url();
        let mut pools = vec![db.clone()];
        for _ in 0..3 {
            pools.push(bosun_db::connect(&url, 4).await.expect("a pool"));
        }
        pools
    };
    let keys = Arc::new(Keyring::new(
        Kek::parse(KEK, "the tests").expect("a KEK"),
        Vec::new(),
    ));
    let barrier = Arc::new(Barrier::new(12));
    let tasks: Vec<_> = (0..12)
        .map(|index| {
            let pool = replicas[index % replicas.len()].clone();
            let keys = Arc::clone(&keys);
            let barrier = Arc::clone(&barrier);
            tokio::spawn(async move {
                barrier.wait().await;
                keys_of_jobs::signer(&pool, &keys, start())
                    .await
                    .map(|key| key.key_id().to_owned())
            })
        })
        .collect();
    let mut named = Vec::new();
    for task in tasks {
        named.push(task.await.expect("a task").expect("a signing key"));
    }
    named.dedup();
    assert_eq!(named.len(), 1, "the replicas signed with {named:?}");
    let rows = signing_key::Entity::find()
        .all(&db)
        .await
        .expect("the rows");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert!(row.retired_at.is_none());

    let sealed = bosun_db::secrets::Sealed {
        ciphertext: row.ciphertext.clone().expect("a ciphertext"),
        data_key: row.data_key.clone().expect("a data key"),
        kek_id: row.kek_id.clone().expect("a KEK id"),
    };
    let binding = *row.id.as_bytes();
    assert!(keys.open_as(&JOB_TOKEN_KEY, &binding, &sealed).is_ok());
    let wrong = other_keys();
    assert!(matches!(
        wrong.open_as(&JOB_TOKEN_KEY, &binding, &sealed),
        Err(Unreadable::UnknownKek(_))
    ));
    assert!(matches!(
        keys_of_jobs::signer(&db, &wrong, start()).await,
        Err(Failure::Unreadable(Unreadable::UnknownKek(_)))
    ));
    assert!(
        keys.open_as(&JOB_TOKEN_KEY, Uuid::now_v7().as_bytes(), &sealed)
            .is_err(),
        "a sealed key opened for another row"
    );
    assert!(keys.open(&binding, &sealed).is_err());

    let rotating = Keyring::new(
        Kek::parse(OTHER_KEK, "the tests").expect("a KEK"),
        vec![Kek::parse(KEK, "the tests").expect("a KEK")],
    );
    let mut failures = Vec::new();
    keys_of_jobs::rewrap(&db, &rotating, &mut failures)
        .await
        .expect("a rewrap");
    assert!(failures.is_empty(), "{failures:?}");
    let renewed = keys_of_jobs::signer(&db, &wrong, start())
        .await
        .expect("the key opens under the new KEK alone");
    assert_eq!(renewed.key_id(), named[0]);
    assert!(matches!(
        keys_of_jobs::signer(&db, &keys, start()).await,
        Err(Failure::Unreadable(Unreadable::UnknownKek(_)))
    ));
}

#[tokio::test]
async fn the_key_rotates_once_per_period_however_many_replicas_try_and_a_replica_without_its_kek_leaves_it()
 {
    let world = World::new().await;
    let db = world.db();
    let keys = Keyring::new(Kek::parse(KEK, "the tests").expect("a KEK"), Vec::new());
    let first = keys_of_jobs::signer(db, &keys, start())
        .await
        .expect("a signing key");
    assert_eq!(together(db, &keys, start() + TimeDelta::hours(23)).await, 0);
    assert_eq!(together(db, &keys, start() + TimeDelta::hours(25)).await, 1);
    assert_eq!(together(db, &keys, start() + TimeDelta::hours(26)).await, 0);
    let rows = signing_key::Entity::find().all(db).await.expect("the rows");
    assert_eq!(rows.len(), 2);
    let (retired, live): (Vec<_>, Vec<_>) = rows.iter().partition(|row| row.retired_at.is_some());
    assert_eq!((retired.len(), live.len()), (1, 1));
    assert!(
        retired[0].ciphertext.is_none()
            && retired[0].data_key.is_none()
            && retired[0].kek_id.is_none(),
        "a retired key kept its private key"
    );
    assert_eq!(
        retired[0].public_key.as_slice(),
        first.public_key().as_slice()
    );
    assert_eq!(retired[0].retired_at, Some(start() + TimeDelta::hours(25)));

    let strange = other_keys();
    let swept = keys_of_jobs::sweep_once(db, &strange, start() + TimeDelta::days(3), DAY)
        .await
        .expect("a sweep")
        .expect("the lock");
    assert!(!swept.rotated);
    assert_eq!(
        signing_key::Entity::find()
            .all(db)
            .await
            .expect("the rows")
            .len(),
        2
    );
}

async fn together(db: &DatabaseConnection, keys: &Keyring, at: DateTime<Utc>) -> usize {
    let barrier = Barrier::new(6);
    let attempt = || async {
        barrier.wait().await;
        keys_of_jobs::sweep_once(db, keys, at, DAY)
            .await
            .expect("a sweep")
            .is_some_and(|swept| swept.rotated)
    };
    let (a, b, c, d, e, f) = tokio::join!(
        attempt(),
        attempt(),
        attempt(),
        attempt(),
        attempt(),
        attempt()
    );
    [a, b, c, d, e, f]
        .into_iter()
        .filter(|rotated| *rotated)
        .count()
}
