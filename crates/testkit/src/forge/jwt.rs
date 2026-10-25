use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::APP_ID;

const DRIFT: i64 = 60;

const LONGEST: i64 = 600;

#[derive(Deserialize)]
struct Header {
    alg: String,
}

#[derive(Deserialize)]
struct Claims {
    iat: i64,
    exp: i64,
    iss: Value,
}

pub(super) fn verify(public_key: &[u8], token: &str, now: DateTime<Utc>) -> Result<(), String> {
    let parts: Vec<&str> = token.split('.').collect();
    let [header, claims, signature] = parts.as_slice() else {
        return Err("A JSON web token could not be decoded".to_owned());
    };
    let header: Header = decode(header)?;
    if header.alg != "RS256" {
        return Err(format!("The JWT uses the algorithm {}", header.alg));
    }
    let signature = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| "A JSON web token could not be decoded".to_owned())?;
    UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, public_key)
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .map_err(|_| {
            "A JSON web token could not be decoded: its signature does not match the App's \
             public key"
                .to_owned()
        })?;
    let claims: Claims = decode(claims)?;
    let issuer = match &claims.iss {
        Value::Number(number) => number.as_u64(),
        Value::String(text) => text.parse().ok(),
        _ => None,
    };
    if issuer != Some(APP_ID) {
        return Err(format!("The JWT names the issuer {}", claims.iss));
    }
    let now = now.timestamp();
    if claims.exp <= now {
        return Err("The JWT has expired".to_owned());
    }
    if claims.iat > now + DRIFT {
        return Err(
            "'Issued at' claim ('iat') must be an Integer representing a time in the past"
                .to_owned(),
        );
    }
    if claims.exp - claims.iat > LONGEST || claims.exp - now > LONGEST {
        return Err("'Expiration time' claim ('exp') is too far in the future".to_owned());
    }
    Ok(())
}

fn decode<T: DeserializeOwned>(part: &str) -> Result<T, String> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| "A JSON web token could not be decoded".to_owned())?;
    serde_json::from_slice(&bytes).map_err(|_| "A JSON web token could not be decoded".to_owned())
}
