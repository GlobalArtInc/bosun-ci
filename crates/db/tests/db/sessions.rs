use bosun_db::entity::{session, sign_in_failure};
use bosun_db::sessions::{self, NewSession, Sweep, Swept};
use bosun_db::sign_in::{self, Kind, Lockout, Reservation, Subject};
use bosun_db::tokens::NewToken;
use bosun_db::users;
use bosun_testkit::TestDatabase;
use bosun_types::id::UserId;
use chrono::TimeDelta;
use sea_orm::prelude::DateTimeUtc;
use sea_orm::{ConnectionTrait, EntityTrait, PaginatorTrait, TransactionTrait};
use tokio::task::JoinSet;

use crate::rows;

const SWEEP_LOCK: i64 = i64::from_be_bytes(*b"bosunses");

const NOW: &str = "2026-10-25T12:00:00Z";

fn token(byte: u8) -> NewToken {
    NewToken {
        name: "bootstrap".to_owned(),
        sha256: vec![byte; 32],
        expires_at: rows::at("2027-10-25T12:00:00Z"),
    }
}

fn lockout() -> Lockout {
    Lockout {
        failures: 3,
        first: TimeDelta::minutes(1),
        longest: TimeDelta::days(1),
        forget: TimeDelta::days(1),
    }
}

fn account(login: &str) -> Subject {
    Subject {
        kind: Kind::Account,
        key: login.to_owned(),
    }
}

async fn owner<C: TransactionTrait>(db: &C, login: &str, byte: u8) -> UserId {
    users::create_owner(db, login, token(byte))
        .await
        .expect("an owner")
        .id
}

async fn session_at<C: ConnectionTrait>(db: &C, user: UserId, byte: u8, at: DateTimeUtc) {
    sessions::create(
        db,
        NewSession {
            user_id: user,
            sha256: vec![byte; 32],
            at,
        },
    )
    .await
    .expect("a session");
}

#[tokio::test]
async fn the_first_owner_has_a_password_to_change_and_exists_only_in_a_database_without_users() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let first = users::first_owner(db, "admin", token(1), "$argon2id$first".to_owned())
        .await
        .expect("the first owner")
        .expect("a database without users");
    assert_eq!(first.login, "admin");
    assert!(first.owner);
    assert!(
        users::first_owner(db, "other", token(2), "$argon2id$other".to_owned())
            .await
            .expect("a second attempt")
            .is_none()
    );

    let credentials = users::credentials_by_login(db, "admin")
        .await
        .expect("a lookup")
        .expect("the first owner");
    assert_eq!(credentials.user, first);
    assert_eq!(
        credentials.password_hash.as_deref(),
        Some("$argon2id$first")
    );
    assert!(credentials.password_change_required);
    assert!(
        users::credentials_by_login(db, "other")
            .await
            .expect("a lookup")
            .is_none()
    );

    users::set_password(db, first.id, "$argon2id$changed".to_owned())
        .await
        .expect("a new password");
    let credentials = users::credentials(db, first.id)
        .await
        .expect("a lookup")
        .expect("the first owner");
    assert_eq!(
        credentials.password_hash.as_deref(),
        Some("$argon2id$changed")
    );
    assert!(!credentials.password_change_required);

    let created = users::create_owner(db, "roman", token(3))
        .await
        .expect("an owner");
    let credentials = users::credentials(db, created.id)
        .await
        .expect("a lookup")
        .expect("the owner");
    assert_eq!(credentials.password_hash, None);
    assert!(!credentials.password_change_required);
}

#[tokio::test]
async fn a_session_is_found_by_its_hash_touched_forward_and_ended() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let roman = owner(db, "roman", 1).await;
    let other = owner(db, "other", 2).await;
    let at = rows::at(NOW);
    session_at(db, roman, 10, at).await;
    session_at(db, roman, 11, at).await;
    session_at(db, other, 12, at).await;

    let found = sessions::find(db, &[10; 32])
        .await
        .expect("a lookup")
        .expect("the session");
    assert_eq!(found.user.login, "roman");
    assert!(!found.password_change_required);
    assert_eq!((found.created_at, found.last_seen_at), (at, at));
    assert!(
        sessions::find(db, &[13; 32])
            .await
            .expect("a lookup")
            .is_none()
    );

    let later = at + TimeDelta::minutes(5);
    sessions::touch(db, found.id, later).await.expect("a touch");
    sessions::touch(db, found.id, at).await.expect("a touch");
    let touched = sessions::find(db, &[10; 32])
        .await
        .expect("a lookup")
        .expect("the session");
    assert_eq!((touched.created_at, touched.last_seen_at), (at, later));

    assert_eq!(
        sessions::end_others(db, roman, Some(found.id))
            .await
            .expect("the other sessions end"),
        1
    );
    assert!(
        sessions::find(db, &[11; 32])
            .await
            .expect("a lookup")
            .is_none()
    );
    assert!(
        sessions::find(db, &[12; 32])
            .await
            .expect("a lookup")
            .is_some()
    );
    sessions::end(db, found.id).await.expect("the end");
    assert!(
        sessions::find(db, &[10; 32])
            .await
            .expect("a lookup")
            .is_none()
    );
    assert_eq!(
        sessions::end_others(db, other, None)
            .await
            .expect("every session ends"),
        1
    );
}

#[tokio::test]
async fn the_sweep_holds_its_lock_and_removes_expired_sessions_and_forgotten_failures() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let roman = owner(db, "roman", 1).await;
    let now = rows::at(NOW);
    session_at(db, roman, 10, now - TimeDelta::hours(13)).await;
    session_at(db, roman, 11, now - TimeDelta::minutes(5)).await;
    let old = rows::at("2026-09-20T12:00:00Z");
    session_at(db, roman, 12, old).await;
    let still_used = sessions::find(db, &[12; 32])
        .await
        .expect("a lookup")
        .expect("the session");
    sessions::touch(db, still_used.id, now - TimeDelta::minutes(1))
        .await
        .expect("a touch");
    for (key, last_failed_at, locked_until) in [
        ("forgotten", now - TimeDelta::hours(25), None),
        (
            "locked",
            now - TimeDelta::hours(25),
            Some(now + TimeDelta::hours(1)),
        ),
        ("recent", now - TimeDelta::hours(1), None),
    ] {
        sign_in_failure::Entity::insert(sign_in_failure::ActiveModel {
            kind: sea_orm::ActiveValue::Set("address".to_owned()),
            key: sea_orm::ActiveValue::Set(key.to_owned()),
            failures: sea_orm::ActiveValue::Set(2),
            last_failed_at: sea_orm::ActiveValue::Set(last_failed_at),
            locked_until: sea_orm::ActiveValue::Set(locked_until),
        })
        .exec(db)
        .await
        .expect("a failed sign-in");
    }
    let sweep = || Sweep {
        idle_before: now - TimeDelta::hours(12),
        created_before: now - TimeDelta::days(30),
        failed_before: now - TimeDelta::days(1),
        now,
    };

    let holder = db.begin().await.expect("a transaction");
    holder
        .execute_unprepared(&format!("SELECT pg_advisory_xact_lock({SWEEP_LOCK})"))
        .await
        .expect("the lock");
    assert!(
        sessions::sweep(db, sweep())
            .await
            .expect("a sweep")
            .is_none(),
        "a second sweep ran"
    );
    holder.rollback().await.expect("the lock is released");

    assert_eq!(
        sessions::sweep(db, sweep()).await.expect("a sweep"),
        Some(Swept {
            sessions: 2,
            failures: 1
        })
    );
    assert_eq!(session::Entity::find().count(db).await.expect("a count"), 1);
    assert!(
        sessions::find(db, &[11; 32])
            .await
            .expect("a lookup")
            .is_some()
    );
    let mut kept: Vec<String> = sign_in_failure::Entity::find()
        .all(db)
        .await
        .expect("the failed sign-ins")
        .into_iter()
        .map(|failure| failure.key)
        .collect();
    kept.sort();
    assert_eq!(kept, ["locked", "recent"]);
}

#[tokio::test]
async fn concurrent_attempts_at_one_account_get_no_more_tries_than_the_lockout_allows() {
    let database = TestDatabase::migrated().await;
    let pool = bosun_db::connect(&database.url(), 12)
        .await
        .expect("a pool");
    let now = rows::at(NOW);
    let mut attempts = JoinSet::new();
    for _ in 0..12 {
        let pool = pool.clone();
        attempts.spawn(async move {
            sign_in::reserve(&pool, vec![account("roman")], now, &lockout())
                .await
                .expect("a reservation")
        });
    }
    let mut granted = 0;
    let mut locked = 0;
    while let Some(reservation) = attempts.join_next().await {
        match reservation.expect("an attempt") {
            Reservation::Granted(_) => granted += 1,
            Reservation::Locked { subject, until } => {
                assert_eq!(subject, account("roman"));
                assert_eq!(until, now + TimeDelta::minutes(1));
                locked += 1;
            }
        }
    }
    assert_eq!((granted, locked), (3, 9));
}

#[tokio::test]
async fn a_released_attempt_of_an_address_leaves_its_count_and_lock_as_they_were() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let now = rows::at(NOW);
    let address = Subject {
        kind: Kind::Address,
        key: "192.0.2.10".to_owned(),
    };
    for _ in 0..2 {
        assert!(matches!(
            sign_in::reserve(db, vec![address.clone()], now, &lockout())
                .await
                .expect("a reservation"),
            Reservation::Granted(_)
        ));
    }
    let stored = || async {
        let row =
            sign_in_failure::Entity::find_by_id(("address".to_owned(), "192.0.2.10".to_owned()))
                .one(db)
                .await
                .expect("a query")
                .expect("the failed sign-ins of the address");
        (row.failures, row.last_failed_at, row.locked_until)
    };
    let before = stored().await;
    assert_eq!(before, (2, now, None));

    let later = now + TimeDelta::minutes(10);
    let Reservation::Granted(granted) = sign_in::reserve(
        db,
        vec![account("roman"), address.clone()],
        later,
        &lockout(),
    )
    .await
    .expect("a reservation") else {
        panic!("the third attempt was refused");
    };
    assert_eq!(
        stored().await,
        (3, later, Some(later + TimeDelta::minutes(1)))
    );
    sign_in::release(db, granted).await.expect("a release");
    assert_eq!(stored().await, before);
    assert_eq!(
        sign_in_failure::Entity::find()
            .count(db)
            .await
            .expect("a count"),
        1,
        "the account's count is gone"
    );
}
