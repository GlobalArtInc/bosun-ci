use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bosun_services::{KeySet, SigningKey, TokenError};
use chrono::Duration;
use ring::signature::Ed25519KeyPair;
use serde_json::{Value, json};

use crate::{identity, run, start};

fn keys(signing: &SigningKey) -> KeySet {
    let mut keys = KeySet::new();
    keys.insert(signing.public_key());
    keys
}

fn payload(token: &str) -> Value {
    let part = token.split('.').nth(1).expect("a payload");
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).expect("base64url")).expect("JSON")
}

#[test]
fn a_token_verifies_against_a_set_that_holds_its_key_and_fails_against_one_that_does_not() {
    let (signing, _) = SigningKey::generate().expect("a key");
    let (other, _) = SigningKey::generate().expect("a key");
    let who = identity(1);
    let token = signing
        .mint(who, start(), start() + Duration::hours(3))
        .expect("a token");

    let verified = keys(&signing).verify(&token, start()).expect("verifies");
    assert_eq!(verified.identity(), who);
    assert_eq!(verified.key_id(), signing.key_id());
    assert_eq!(
        payload(&token)["scp"],
        format!(
            "Actions.GenericRead:{a} Actions.UploadArtifacts:{a}:{e} Actions.Results:{a}:{e}",
            a = who.attempt,
            e = who.execution
        )
    );

    assert_eq!(
        keys(&other).verify(&token, start()),
        Err(TokenError::UnknownKey)
    );
    assert_eq!(
        KeySet::new().verify(&token, start()),
        Err(TokenError::UnknownKey)
    );
    let mut both = keys(&other);
    both.insert(signing.public_key());
    assert!(both.verify(&token, start()).is_ok());
}

#[test]
fn an_expired_altered_or_malformed_token_is_refused_without_a_panic() {
    let (signing, _) = SigningKey::generate().expect("a key");
    let set = keys(&signing);
    let expires = start() + Duration::hours(1);
    let token = signing
        .mint(identity(1), start(), expires)
        .expect("a token");

    assert_eq!(set.verify(&token, expires), Err(TokenError::Expired));
    assert!(set.verify(&token, expires - Duration::seconds(1)).is_ok());
    assert_eq!(
        signing.mint(identity(1), expires, start()),
        Err(TokenError::Lifetime)
    );

    let mut parts: Vec<String> = token.split('.').map(str::to_owned).collect();
    let mut claims = payload(&token);
    claims["scp"] = json!(format!(
        "Actions.GenericRead:{a} Actions.UploadArtifacts:{a}:{e} Actions.Results:{a}:{e}",
        a = run(2),
        e = identity(2).execution
    ));
    parts[1] = URL_SAFE_NO_PAD.encode(claims.to_string());
    assert_eq!(
        set.verify(&parts.join("."), start()),
        Err(TokenError::BadSignature)
    );

    let none = format!(
        "{}.{}.",
        URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#),
        parts[1]
    );
    assert_eq!(
        set.verify(&none, start()),
        Err(TokenError::UnsupportedAlgorithm)
    );

    let long = "a".repeat(100_000);
    let nested = format!("{}.{}.{}", "W".repeat(40), "e30", "c2ln");
    for garbage in [
        "",
        ".",
        "..",
        "...",
        "a.b.c",
        "a.b",
        "é.é.é",
        "e30.e30.e30",
        nested.as_str(),
        long.as_str(),
    ] {
        assert!(set.verify(garbage, start()).is_err(), "{garbage:.20}");
    }
}

#[test]
fn a_signed_token_whose_scopes_disagree_is_refused() {
    let (signing, pkcs8) = SigningKey::generate().expect("a key");
    let pair = Ed25519KeyPair::from_pkcs8(&pkcs8).expect("a pair");
    let set = keys(&signing);
    let exp = (start() + Duration::hours(1)).timestamp();
    let (a, e) = (run(1), identity(1).execution);
    let scopes = [
        format!("Actions.GenericRead:{a} Actions.UploadArtifacts:{a}:{e}"),
        format!(
            "Actions.GenericRead:{} Actions.UploadArtifacts:{a}:{e} Actions.Results:{a}:{e}",
            run(2)
        ),
        format!(
            "Actions.GenericRead:{a} Actions.UploadArtifacts:{a}:{} Actions.Results:{a}:{e}",
            identity(2).execution
        ),
        format!(
            "Actions.GenericRead:{a} Actions.UploadArtifacts:{a}:{e} Actions.Results:{a}:{e} Actions.Results:{a}:{e}"
        ),
        format!(
            "Actions.GenericRead:{a} Actions.UploadArtifacts:{a}:{e} Actions.Results:{a}:{e}:extra"
        ),
        format!(
            "Actions.GenericRead:{a} Actions.UploadArtifacts:{a}:{e} Actions.Results:not-a-job"
        ),
        String::new(),
    ];
    for scp in scopes {
        let header = URL_SAFE_NO_PAD
            .encode(json!({"alg": "EdDSA", "typ": "JWT", "kid": signing.key_id()}).to_string());
        let claims = URL_SAFE_NO_PAD.encode(
            json!({
                "iss": "bosun", "aud": "bosun-runtime",
                "iat": start().timestamp(), "exp": exp, "scp": scp
            })
            .to_string(),
        );
        let signed = format!("{header}.{claims}");
        let signature = URL_SAFE_NO_PAD.encode(pair.sign(signed.as_bytes()));
        let token = format!("{signed}.{signature}");
        assert!(
            matches!(set.verify(&token, start()), Err(TokenError::Claims(_))),
            "{scp}"
        );
    }
}
