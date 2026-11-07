use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Duration, Utc};
use ring::hmac;

pub const MIN_SECRET_BYTES: usize = 32;
const LABEL: &[u8] = b"bosun signed address v1";
const SIGNATURE_BYTES: usize = 32;

pub const LIFETIME: Duration = Duration::hours(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    Upload,
    Download,
}

impl Purpose {
    fn label(self) -> &'static [u8] {
        match self {
            Self::Upload => b"upload",
            Self::Download => b"download",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressError {
    ShortSecret,
    Missing,
    Malformed,
    Mismatch,
    Expired,
}

impl fmt::Display for AddressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ShortSecret => write!(
                f,
                "the secret of signed addresses is shorter than {MIN_SECRET_BYTES} bytes"
            ),
            Self::Missing => f.write_str("the address carries no expiry or signature"),
            Self::Malformed => {
                f.write_str("the expiry or the signature of the address is malformed")
            }
            Self::Mismatch => f.write_str("the signature does not match the address"),
            Self::Expired => f.write_str("the address has expired"),
        }
    }
}

impl std::error::Error for AddressError {}

pub struct AddressSigner {
    key: hmac::Key,
}

impl fmt::Debug for AddressSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AddressSigner").finish_non_exhaustive()
    }
}

impl AddressSigner {
    pub fn new(secret: &[u8]) -> Result<Self, AddressError> {
        if secret.len() < MIN_SECRET_BYTES {
            return Err(AddressError::ShortSecret);
        }
        Ok(Self {
            key: hmac::Key::new(hmac::HMAC_SHA256, secret),
        })
    }

    pub fn query(&self, purpose: Purpose, object: &str, expires_at: DateTime<Utc>) -> String {
        let expires = expires_at.timestamp();
        let tag = hmac::sign(&self.key, &message(purpose, object, expires));
        format!(
            "expires={expires}&sig={}",
            URL_SAFE_NO_PAD.encode(tag.as_ref())
        )
    }

    pub fn verify(
        &self,
        purpose: Purpose,
        object: &str,
        expires: Option<&str>,
        signature: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<(), AddressError> {
        let (Some(expires), Some(signature)) = (expires, signature) else {
            return Err(AddressError::Missing);
        };
        let expires = canonical_seconds(expires).ok_or(AddressError::Malformed)?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| AddressError::Malformed)?;
        if signature.len() != SIGNATURE_BYTES {
            return Err(AddressError::Malformed);
        }
        hmac::verify(&self.key, &message(purpose, object, expires), &signature)
            .map_err(|_| AddressError::Mismatch)?;
        if now.timestamp() >= expires {
            return Err(AddressError::Expired);
        }
        Ok(())
    }
}

fn canonical_seconds(text: &str) -> Option<i64> {
    if text.is_empty()
        || text.len() > 18
        || !text.bytes().all(|byte| byte.is_ascii_digit())
        || (text.len() > 1 && text.starts_with('0'))
    {
        return None;
    }
    text.parse().ok()
}

fn message(purpose: Purpose, object: &str, expires: i64) -> Vec<u8> {
    let mut message = Vec::with_capacity(LABEL.len() + object.len() + 40);
    message.extend_from_slice(LABEL);
    message.push(0);
    message.extend_from_slice(purpose.label());
    message.push(0);
    message.extend_from_slice(expires.to_string().as_bytes());
    message.push(0);
    message.extend_from_slice(object.as_bytes());
    message
}
