use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use rustls_pki_types::PrivateKeyDer;
use rustls_pki_types::pem::PemObject;
use serde_json::json;

use crate::Error;

const HEADER: &[u8] = br#"{"alg":"RS256","typ":"JWT"}"#;

const BACKDATE: i64 = 60;

const LIFETIME: i64 = 540;

pub(super) struct AppKey {
    pair: RsaKeyPair,
    random: SystemRandom,
}

impl AppKey {
    pub(super) fn parse(pem: &[u8]) -> Result<AppKey, Error> {
        let der = PrivateKeyDer::from_pem_slice(pem).map_err(|error| {
            Error::Config(format!(
                "the App's private key is not a PEM private key: {error}"
            ))
        })?;
        let pair = match &der {
            PrivateKeyDer::Pkcs1(key) => RsaKeyPair::from_der(key.secret_pkcs1_der()),
            PrivateKeyDer::Pkcs8(key) => RsaKeyPair::from_pkcs8(key.secret_pkcs8_der()),
            _ => {
                return Err(Error::Config(
                    "the App's private key is not an RSA key".to_owned(),
                ));
            }
        }
        .map_err(|error| Error::Config(format!("the App's private key is not usable: {error}")))?;
        Ok(AppKey {
            pair,
            random: SystemRandom::new(),
        })
    }

    pub(super) fn sign(&self, app: u64, now: DateTime<Utc>) -> Result<String, Error> {
        let now = now.timestamp();
        let claims = json!({
            "iat": now - BACKDATE,
            "exp": now + LIFETIME,
            "iss": app,
        });
        let mut token = URL_SAFE_NO_PAD.encode(HEADER);
        token.push('.');
        token.push_str(&URL_SAFE_NO_PAD.encode(claims.to_string()));
        let mut signature = vec![0; self.pair.public().modulus_len()];
        self.pair
            .sign(
                &RSA_PKCS1_SHA256,
                &self.random,
                token.as_bytes(),
                &mut signature,
            )
            .map_err(|_| Error::Config("the App's private key cannot sign".to_owned()))?;
        token.push('.');
        token.push_str(&URL_SAFE_NO_PAD.encode(signature));
        Ok(token)
    }
}
