use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::sessions;

const ALGORITHM: &str = "RS256";

const RSA: &str = "RSA";

const SIGNATURE: &str = "sig";

#[derive(Deserialize)]
pub(super) struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kty: String,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default, rename = "use")]
    usage: Option<String>,
    #[serde(default)]
    alg: Option<String>,
    #[serde(default)]
    n: Option<String>,
    #[serde(default)]
    e: Option<String>,
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
}

pub(super) struct Expected<'a> {
    pub(super) issuer: &'a str,
    pub(super) client_id: &'a str,
    pub(super) nonce: &'a str,
    pub(super) now: DateTime<Utc>,
}

pub(super) struct IdToken<'a> {
    kid: Option<String>,
    signed: &'a str,
    signature: Vec<u8>,
    claims: Map<String, Value>,
}

impl Jwks {
    pub(super) fn offers(&self, kid: Option<&str>) -> bool {
        self.select(kid).is_ok()
    }

    fn select(&self, kid: Option<&str>) -> Result<&Jwk, String> {
        let usable: Vec<&Jwk> = self
            .keys
            .iter()
            .filter(|key| {
                key.kty == RSA
                    && key.usage.as_deref().is_none_or(|usage| usage == SIGNATURE)
                    && key.alg.as_deref().is_none_or(|alg| alg == ALGORITHM)
            })
            .collect();
        match kid {
            Some(kid) => usable
                .into_iter()
                .find(|key| key.kid.as_deref() == Some(kid))
                .ok_or_else(|| {
                    format!("is signed with the key `{kid}`, which the provider does not publish")
                }),
            None => match usable.as_slice() {
                [only] => Ok(only),
                [] => {
                    Err("is signed, and the provider publishes no RSA key to check it".to_owned())
                }
                _ => Err(
                    "names no key, and the provider publishes several, so none can be chosen"
                        .to_owned(),
                ),
            },
        }
    }
}

impl<'a> IdToken<'a> {
    pub(super) fn parse(token: &'a str) -> Result<IdToken<'a>, String> {
        let mut parts = token.split('.');
        let (Some(header), Some(claims), Some(signature), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err("is not a signed JSON web token of three parts".to_owned());
        };
        let header: Header = decode(header, "header")?;
        if header.alg != ALGORITHM {
            return Err(format!(
                "is signed with `{}`, and an ID token is signed with {ALGORITHM}",
                header.alg
            ));
        }
        let signed = &token[..token.len() - signature.len() - 1];
        let claims: Map<String, Value> = decode(claims, "claims")?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| "has a signature that is not base64url".to_owned())?;
        Ok(IdToken {
            kid: header.kid,
            signed,
            signature,
            claims,
        })
    }

    pub(super) fn kid(&self) -> Option<&str> {
        self.kid.as_deref()
    }

    pub(super) fn verify(
        self,
        keys: &Jwks,
        expected: &Expected<'_>,
    ) -> Result<Map<String, Value>, String> {
        let key = keys.select(self.kid())?;
        let component = |value: &Option<String>, name: &str| {
            let bytes = value
                .as_deref()
                .and_then(|text| URL_SAFE_NO_PAD.decode(text).ok())
                .ok_or_else(|| format!("is signed with a key whose `{name}` is not base64url"))?;
            let start = bytes
                .iter()
                .position(|byte| *byte != 0)
                .unwrap_or(bytes.len());
            Ok::<Vec<u8>, String>(bytes[start..].to_vec())
        };
        let public = RsaPublicKeyComponents {
            n: component(&key.n, "n")?,
            e: component(&key.e, "e")?,
        };
        public
            .verify(
                &RSA_PKCS1_2048_8192_SHA256,
                self.signed.as_bytes(),
                &self.signature,
            )
            .map_err(|_| "carries a signature that the provider's key does not make".to_owned())?;
        let claims = self.claims;
        let text = |name: &str| claims.get(name).and_then(Value::as_str);
        match text("iss") {
            Some(issuer) if issuer == expected.issuer => {}
            Some(issuer) => {
                return Err(format!(
                    "names the issuer `{issuer}`, and the provider is `{}`",
                    expected.issuer
                ));
            }
            None => return Err("names no issuer".to_owned()),
        }
        let audiences: Vec<&str> = match claims.get("aud") {
            Some(Value::String(audience)) => vec![audience.as_str()],
            Some(Value::Array(audiences)) => audiences.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        if !audiences.contains(&expected.client_id) {
            return Err(format!(
                "is meant for {}, and the client of Bosun is `{}`",
                match audiences.as_slice() {
                    [] => "no audience".to_owned(),
                    audiences => audiences
                        .iter()
                        .map(|audience| format!("`{audience}`"))
                        .collect::<Vec<_>>()
                        .join(", "),
                },
                expected.client_id
            ));
        }
        match text("azp") {
            Some(party) if party != expected.client_id => {
                return Err(format!(
                    "was issued to `{party}`, and the client of Bosun is `{}`",
                    expected.client_id
                ));
            }
            None if audiences.len() > 1 => {
                return Err(
                    "is meant for several audiences without naming the party it was issued to"
                        .to_owned(),
                );
            }
            _ => {}
        }
        let expires = claims
            .get("exp")
            .and_then(|exp| exp.as_i64().or_else(|| exp.as_f64().map(|exp| exp as i64)))
            .ok_or_else(|| "has no expiry".to_owned())?;
        if expires <= expected.now.timestamp() {
            let at = DateTime::<Utc>::from_timestamp(expires, 0)
                .map_or_else(|| expires.to_string(), |at| at.to_rfc3339());
            return Err(format!("expired at {at}"));
        }
        match text("nonce") {
            Some(nonce) if sessions::same(nonce.as_bytes(), expected.nonce.as_bytes()) => {}
            _ => {
                return Err(
                    "carries another nonce than the sign-in this browser started".to_owned(),
                );
            }
        }
        if text("sub").is_none_or(str::is_empty) {
            return Err("names no subject".to_owned());
        }
        Ok(claims)
    }
}

fn decode<T: DeserializeOwned>(part: &str, what: &str) -> Result<T, String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| format!("has a {what} that is not base64url"))?;
    serde_json::from_slice(&bytes).map_err(|_| format!("has a {what} that is not a JSON object"))
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use chrono::Utc;
    use serde_json::json;

    use super::{Expected, IdToken, Jwks};

    fn part(value: &serde_json::Value) -> String {
        URL_SAFE_NO_PAD.encode(value.to_string())
    }

    #[test]
    fn a_token_that_is_not_signed_with_rs256_or_not_three_parts_is_refused() {
        let claims = part(&json!({ "iss": "https://sso.example.org" }));
        for (token, problem) in [
            (
                format!("{}.{claims}.", part(&json!({ "alg": "none" }))),
                "is signed with `none`",
            ),
            (
                format!("{}.{claims}.c2ln", part(&json!({ "alg": "HS256" }))),
                "is signed with `HS256`",
            ),
            (
                format!("{claims}.{claims}"),
                "is not a signed JSON web token",
            ),
            ("a.b.c.d".to_owned(), "is not a signed JSON web token"),
            (
                format!("{}.e30.!!", part(&json!({ "alg": "RS256" }))),
                "has a signature that is not base64url",
            ),
        ] {
            let Err(refused) = IdToken::parse(&token) else {
                panic!("{token} was taken");
            };
            assert!(refused.starts_with(problem), "{token}: {refused}");
        }
    }

    #[test]
    fn a_key_is_chosen_by_its_id_or_as_the_only_signing_key() {
        let keys: Jwks = serde_json::from_value(json!({
            "keys": [
                { "kty": "RSA", "kid": "enc", "use": "enc", "n": "AQAB", "e": "AQAB" },
                { "kty": "EC", "kid": "ec", "crv": "P-256", "x": "AA", "y": "AA" },
                { "kty": "RSA", "kid": "one", "use": "sig", "alg": "RS256", "n": "AQAB", "e": "AQAB" }
            ]
        }))
        .expect("a key set");
        assert!(keys.offers(Some("one")));
        assert!(keys.offers(None));
        assert!(!keys.offers(Some("enc")));
        assert!(!keys.offers(Some("ec")));
        assert!(!keys.offers(Some("two")));
        let several: Jwks = serde_json::from_value(json!({
            "keys": [
                { "kty": "RSA", "kid": "one", "n": "AQAB", "e": "AQAB" },
                { "kty": "RSA", "kid": "two", "n": "AQAB", "e": "AQAB" }
            ]
        }))
        .expect("a key set");
        assert!(!several.offers(None));
        assert!(several.offers(Some("two")));
    }

    #[test]
    fn a_signature_that_the_key_did_not_make_is_refused_before_the_claims() {
        let keys: Jwks = serde_json::from_value(json!({
            "keys": [{ "kty": "RSA", "n": "AQAB", "e": "AQAB" }]
        }))
        .expect("a key set");
        let token = format!(
            "{}.{}.c2lnbmF0dXJl",
            part(&json!({ "alg": "RS256" })),
            part(&json!({ "iss": "https://sso.example.org" }))
        );
        let parsed = IdToken::parse(&token).expect("a token");
        let refused = parsed
            .verify(
                &keys,
                &Expected {
                    issuer: "https://sso.example.org",
                    client_id: "bosun",
                    nonce: "nonce",
                    now: Utc::now(),
                },
            )
            .expect_err("a refusal");
        assert!(refused.contains("signature"), "{refused}");
    }
}
