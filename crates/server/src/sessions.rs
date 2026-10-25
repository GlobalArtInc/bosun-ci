use std::time::Duration;

use axum::http::header::COOKIE;
use axum::http::{HeaderMap, HeaderValue};
use bosun_db::sessions::{NewSession, Sweep};
use bosun_types::id::{SessionId, UserId};
use chrono::{DateTime, TimeDelta, Utc};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use sha2::{Digest, Sha256};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

use crate::clock::Clock;
use crate::envelope::ApiError;
use crate::request::{database, failed};
use crate::sign_in::FORGET;
use crate::token::{SESSION, Token, base62};

pub(crate) const COOKIE_NAME: &str = "__Host-bosun_session";

pub(crate) const IDLE: TimeDelta = TimeDelta::hours(12);

pub(crate) const LIFETIME: TimeDelta = TimeDelta::days(30);

pub(crate) const TOUCH: TimeDelta = TimeDelta::minutes(1);

const SWEEP: Duration = Duration::from_secs(60);

const ATTRIBUTES: &str = "Path=/; Secure; HttpOnly; SameSite=Lax";

const CLEARED: &str = "__Host-bosun_session=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Lax";

const CSRF_CONTEXT: &[u8] = b"bosun csrf\0";

const CSRF_DIGITS: usize = 43;

pub(crate) struct Started {
    pub(crate) id: SessionId,
    pub(crate) token: Token,
}

pub(crate) async fn start<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    at: DateTime<Utc>,
) -> Result<Started, ApiError> {
    let token = Token::generate(SESSION).map_err(failed)?;
    let id = bosun_db::sessions::create(
        db,
        NewSession {
            user_id: user,
            sha256: token.sha256(),
            at,
        },
    )
    .await
    .map_err(database)?;
    Ok(Started { id, token })
}

pub(crate) fn cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name == COOKIE_NAME).then_some(value)
        })
}

pub(crate) fn set_cookie(token: &Token) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(&format!(
        "{COOKIE_NAME}={}; Max-Age={}; {ATTRIBUTES}",
        token.as_str(),
        LIFETIME.num_seconds()
    ))
    .map_err(failed)
}

pub(crate) fn clear_cookie() -> HeaderValue {
    HeaderValue::from_static(CLEARED)
}

pub(crate) fn csrf(token: &Token) -> String {
    let mut digest = Sha256::new();
    digest.update(CSRF_CONTEXT);
    digest.update(token.as_str().as_bytes());
    base62(&digest.finalize(), CSRF_DIGITS)
}

pub(crate) fn same(given: &[u8], expected: &[u8]) -> bool {
    given.len() == expected.len()
        && given
            .iter()
            .zip(expected)
            .fold(0, |difference, (left, right)| difference | (left ^ right))
            == 0
}

pub(crate) async fn sweep(db: DatabaseConnection, clock: Clock) {
    let mut ticks = interval(SWEEP);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let now = clock.now();
        match bosun_db::sessions::sweep(&db, expiry(now)).await {
            Ok(Some(swept)) if swept.sessions > 0 || swept.failures > 0 => info!(
                sessions = swept.sessions,
                failures = swept.failures,
                "removed expired sessions and forgotten failed sign-ins"
            ),
            Ok(_) => {}
            Err(error) => warn!(%error, "the removal of expired sessions failed"),
        }
    }
}

pub(crate) fn expiry(now: DateTime<Utc>) -> Sweep {
    Sweep {
        idle_before: now - IDLE,
        created_before: now - LIFETIME,
        failed_before: now - FORGET,
        now,
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};

    use super::{COOKIE_NAME, clear_cookie, cookie, csrf, same, set_cookie};
    use crate::token::{SESSION, Token};

    fn headers(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(
                "cookie",
                HeaderValue::from_str(value).expect("a header value"),
            );
        }
        headers
    }

    #[test]
    fn the_session_cookie_is_found_among_the_others() {
        assert_eq!(
            cookie(&headers(&[
                "theme=dark; __Host-bosun_session=bss_a; lang=ru"
            ])),
            Some("bss_a")
        );
        assert_eq!(
            cookie(&headers(&["theme=dark", "__Host-bosun_session=bss_b"])),
            Some("bss_b")
        );
        assert_eq!(
            cookie(&headers(&[
                "bosun_session=bss_c; __Host-bosun_session2=bss_d"
            ])),
            None
        );
        assert_eq!(cookie(&headers(&[])), None);
    }

    #[test]
    fn the_cookie_carries_its_attributes_and_the_cleared_one_empties_it() {
        let token = Token::generate(SESSION).expect("random bytes");
        let Ok(set) = set_cookie(&token) else {
            panic!("the cookie of {} is not a header value", token.as_str());
        };
        assert_eq!(
            set.to_str().expect("text"),
            format!(
                "{COOKIE_NAME}={}; Max-Age=2592000; Path=/; Secure; HttpOnly; SameSite=Lax",
                token.as_str()
            )
        );
        assert_eq!(
            clear_cookie().to_str().expect("text"),
            format!("{COOKIE_NAME}=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Lax")
        );
    }

    #[test]
    fn the_csrf_token_is_bound_to_its_session() {
        let one = Token::generate(SESSION).expect("random bytes");
        let other = Token::generate(SESSION).expect("random bytes");
        let token = csrf(&one);
        assert_eq!(token.len(), 43);
        assert!(token.bytes().all(|digit| digit.is_ascii_alphanumeric()));
        assert_eq!(csrf(&one), token);
        assert_ne!(csrf(&other), token);
        assert!(!one.as_str().contains(&token));
    }

    #[test]
    fn same_compares_whole_values() {
        assert!(same(b"abc", b"abc"));
        assert!(!same(b"abc", b"abd"));
        assert!(!same(b"abc", b"abcd"));
        assert!(!same(b"", b"a"));
        assert!(same(b"", b""));
    }
}
