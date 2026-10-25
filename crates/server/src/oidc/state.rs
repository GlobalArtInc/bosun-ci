use axum::http::{HeaderMap, HeaderValue};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::keys::Keyring;
use crate::sessions;

pub(super) const COOKIE: &str = "__Host-bosun_oidc";

pub(super) const LIFETIME: TimeDelta = TimeDelta::minutes(10);

const LABEL: &[u8] = b"bosun oidc state\0";

const ATTRIBUTES: &str = "Path=/; Secure; HttpOnly; SameSite=Lax";

const RANDOM_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Started {
    pub(super) provider: String,
    pub(super) state: String,
    pub(super) nonce: String,
    pub(super) verifier: String,
    pub(super) expires_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Unread {
    Missing,
    Forged,
    Elsewhere,
    Expired,
}

impl Started {
    pub(super) fn new(provider: &str, now: DateTime<Utc>) -> Result<Started, getrandom::Error> {
        Ok(Started {
            provider: provider.to_owned(),
            state: random()?,
            nonce: random()?,
            verifier: random()?,
            expires_at: (now + LIFETIME).timestamp(),
        })
    }

    pub(super) fn challenge(&self) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(self.verifier.as_bytes()))
    }

    pub(super) fn expires_at(&self) -> DateTime<Utc> {
        DateTime::from_timestamp(self.expires_at, 0).unwrap_or(DateTime::<Utc>::MIN_UTC)
    }

    pub(super) fn cookie(&self, keys: &Keyring) -> Option<HeaderValue> {
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(self).ok()?);
        let tag = URL_SAFE_NO_PAD.encode(keys.sign(LABEL, payload.as_bytes()));
        HeaderValue::from_str(&format!(
            "{COOKIE}={payload}.{tag}; Max-Age={}; {ATTRIBUTES}",
            LIFETIME.num_seconds()
        ))
        .ok()
    }

    pub(super) fn read(
        headers: &HeaderMap,
        keys: &Keyring,
        provider: &str,
        now: DateTime<Utc>,
    ) -> Result<Started, Unread> {
        let value = sessions::named(headers, COOKIE).ok_or(Unread::Missing)?;
        let (payload, tag) = value.split_once('.').ok_or(Unread::Forged)?;
        let tag = URL_SAFE_NO_PAD.decode(tag).map_err(|_| Unread::Forged)?;
        if !keys.verify(LABEL, payload.as_bytes(), &tag) {
            return Err(Unread::Forged);
        }
        let started: Started = URL_SAFE_NO_PAD
            .decode(payload)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .ok_or(Unread::Forged)?;
        if started.provider != provider {
            return Err(Unread::Elsewhere);
        }
        if started.expires_at() <= now {
            return Err(Unread::Expired);
        }
        Ok(started)
    }
}

pub(super) fn cleared() -> HeaderValue {
    HeaderValue::from_static(
        "__Host-bosun_oidc=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Lax",
    )
}

fn random() -> Result<String, getrandom::Error> {
    let mut bytes = [0; RANDOM_BYTES];
    getrandom::fill(&mut bytes)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue};
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use chrono::{TimeDelta, Utc};
    use sha2::{Digest, Sha256};

    use super::{COOKIE, Started, Unread, cleared};
    use crate::keys::{Kek, Keyring};

    const KEK: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    const OTHER: &str = "f0e1d2c3b4a5968778695a4b3c2d1e0f00112233445566778899aabbccddeeff";

    fn keys(text: &str) -> Keyring {
        Keyring::new(Kek::parse(text, "the test").expect("a KEK"), Vec::new())
    }

    fn sent(cookie: &HeaderValue) -> HeaderMap {
        let pair = cookie
            .to_str()
            .expect("text")
            .split(';')
            .next()
            .expect("a pair")
            .to_owned();
        let mut headers = HeaderMap::new();
        headers.insert("cookie", HeaderValue::from_static("theme=dark"));
        headers.append("cookie", HeaderValue::from_str(&pair).expect("a value"));
        headers
    }

    #[test]
    fn the_state_comes_back_from_its_cookie_for_its_provider_until_it_expires() {
        let now = Utc::now();
        let keys = keys(KEK);
        let started = Started::new("keycloak", now).expect("random bytes");
        assert_eq!(started.state.len(), 43);
        assert_ne!(started.state, started.nonce);
        assert_ne!(started.nonce, started.verifier);
        assert_eq!(
            started.challenge(),
            URL_SAFE_NO_PAD.encode(Sha256::digest(started.verifier.as_bytes()))
        );
        let cookie = started.cookie(&keys).expect("a cookie");
        let text = cookie.to_str().expect("text");
        assert!(text.starts_with(&format!("{COOKIE}=")), "{text}");
        assert!(
            text.ends_with("; Max-Age=600; Path=/; Secure; HttpOnly; SameSite=Lax"),
            "{text}"
        );
        let headers = sent(&cookie);
        assert_eq!(
            Started::read(&headers, &keys, "keycloak", now),
            Ok(started.clone())
        );
        assert_eq!(
            Started::read(&headers, &keys, "other", now),
            Err(Unread::Elsewhere)
        );
        assert_eq!(
            Started::read(&headers, &keys, "keycloak", now + TimeDelta::minutes(10)),
            Err(Unread::Expired)
        );
        assert_eq!(
            Started::read(&headers, &self::keys(OTHER), "keycloak", now),
            Err(Unread::Forged)
        );
        assert_eq!(
            Started::read(&HeaderMap::new(), &keys, "keycloak", now),
            Err(Unread::Missing)
        );
        assert!(cleared().to_str().expect("text").contains("Max-Age=0"));
    }

    #[test]
    fn a_changed_cookie_is_refused() {
        let now = Utc::now();
        let keys = keys(KEK);
        let started = Started::new("keycloak", now).expect("random bytes");
        let cookie = started.cookie(&keys).expect("a cookie");
        let text = cookie.to_str().expect("text").to_owned();
        let value = text
            .split(';')
            .next()
            .and_then(|pair| pair.split_once('='))
            .map(|(_, value)| value.to_owned())
            .expect("a value");
        let (payload, tag) = value.split_once('.').expect("two parts");
        let mut changed = started.clone();
        changed.provider = "elsewhere".to_owned();
        let forged = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&changed).expect("JSON"));
        for value in [
            format!("{forged}.{tag}"),
            payload.to_owned(),
            format!("{payload}.!!"),
            format!("{payload}.{}", URL_SAFE_NO_PAD.encode(b"short")),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(
                "cookie",
                HeaderValue::from_str(&format!("{COOKIE}={value}")).expect("a value"),
            );
            assert_eq!(
                Started::read(&headers, &keys, "keycloak", now),
                Err(Unread::Forged),
                "{value}"
            );
        }
    }
}
