use std::fmt;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bosun_types::id::{AttemptId, ExecutionId};
use chrono::{DateTime, Utc};
use ring::digest::{SHA256, digest};
use ring::rand::SystemRandom;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde::{Deserialize, Serialize};

const ISSUER: &str = "bosun";
const AUDIENCE: &str = "bosun-runtime";
const ALGORITHM: &str = "EdDSA";
const MAX_TOKEN_BYTES: usize = 4096;
const SIGNATURE_BYTES: usize = 64;
const SCOPE_READ: &str = "Actions.GenericRead";
const SCOPE_UPLOAD: &str = "Actions.UploadArtifacts";
const SCOPE_RESULTS: &str = "Actions.Results";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JobIdentity {
    pub attempt: AttemptId,
    pub execution: ExecutionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobToken {
    identity: JobIdentity,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    key_id: String,
}

impl JobToken {
    pub fn identity(&self) -> JobIdentity {
        self.identity
    }

    pub fn issued_at(&self) -> DateTime<Utc> {
        self.issued_at
    }

    pub fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenError {
    Malformed(&'static str),
    UnsupportedAlgorithm,
    UnknownKey,
    BadSignature,
    Expired,
    Claims(&'static str),
    Key(String),
    Lifetime,
}

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(reason) => write!(f, "the job token is malformed: {reason}"),
            Self::UnsupportedAlgorithm => f.write_str("the job token is not signed with EdDSA"),
            Self::UnknownKey => f.write_str("the job token is signed with an unknown key"),
            Self::BadSignature => f.write_str("the signature of the job token does not verify"),
            Self::Expired => f.write_str("the job token has expired"),
            Self::Claims(reason) => write!(f, "the claims of the job token are invalid: {reason}"),
            Self::Key(problem) => write!(f, "the signing key cannot be used: {problem}"),
            Self::Lifetime => f.write_str("the job token must expire after it is issued"),
        }
    }
}

impl std::error::Error for TokenError {}

#[derive(Serialize, Deserialize)]
struct Header {
    alg: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    typ: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kid: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Claims {
    iss: String,
    aud: String,
    iat: i64,
    exp: i64,
    scp: String,
}

pub struct SigningKey {
    pair: Ed25519KeyPair,
    key_id: String,
}

impl fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SigningKey")
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

impl SigningKey {
    pub fn generate() -> Result<(Self, Vec<u8>), TokenError> {
        let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
            .map_err(|_| TokenError::Key("the system random source failed".to_owned()))?;
        let pkcs8 = document.as_ref().to_vec();
        Ok((Self::from_pkcs8(&pkcs8)?, pkcs8))
    }

    pub fn from_pkcs8(pkcs8: &[u8]) -> Result<Self, TokenError> {
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8)
            .map_err(|_| TokenError::Key("not an Ed25519 key in PKCS#8".to_owned()))?;
        let key_id = key_id(pair.public_key().as_ref());
        Ok(Self { pair, key_id })
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    pub fn public_key(&self) -> [u8; 32] {
        let mut public = [0u8; 32];
        public.copy_from_slice(self.pair.public_key().as_ref());
        public
    }

    pub fn mint(
        &self,
        identity: JobIdentity,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<String, TokenError> {
        if expires_at <= issued_at {
            return Err(TokenError::Lifetime);
        }
        let header = Header {
            alg: ALGORITHM.to_owned(),
            typ: Some("JWT".to_owned()),
            kid: Some(self.key_id.clone()),
        };
        let claims = Claims {
            iss: ISSUER.to_owned(),
            aud: AUDIENCE.to_owned(),
            iat: issued_at.timestamp(),
            exp: expires_at.timestamp(),
            scp: scopes(identity),
        };
        let header = serde_json::to_vec(&header)
            .map_err(|_| TokenError::Malformed("the header cannot be written"))?;
        let claims = serde_json::to_vec(&claims)
            .map_err(|_| TokenError::Malformed("the claims cannot be written"))?;
        let signed = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header),
            URL_SAFE_NO_PAD.encode(claims)
        );
        let signature = self.pair.sign(signed.as_bytes());
        Ok(format!(
            "{signed}.{}",
            URL_SAFE_NO_PAD.encode(signature.as_ref())
        ))
    }
}

#[derive(Debug, Clone, Default)]
pub struct KeySet {
    keys: Vec<(String, [u8; 32])>,
}

impl KeySet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, public_key: [u8; 32]) {
        let key_id = key_id(&public_key);
        if !self
            .keys
            .iter()
            .any(|(id, key)| *id == key_id && *key == public_key)
        {
            self.keys.push((key_id, public_key));
        }
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    pub fn verify(&self, token: &str, now: DateTime<Utc>) -> Result<JobToken, TokenError> {
        if token.len() > MAX_TOKEN_BYTES {
            return Err(TokenError::Malformed("it is too long"));
        }
        let Some((signed, signature)) = token.rsplit_once('.') else {
            return Err(TokenError::Malformed("it does not have three parts"));
        };
        let mut parts = signed.split('.');
        let (Some(header), Some(claims), None) = (parts.next(), parts.next(), parts.next()) else {
            return Err(TokenError::Malformed("it does not have three parts"));
        };
        let header: Header = decode(header, "the header")?;
        if header.alg != ALGORITHM {
            return Err(TokenError::UnsupportedAlgorithm);
        }
        let key_id = header.kid.ok_or(TokenError::Malformed("it names no key"))?;
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| TokenError::Malformed("the signature is not base64url"))?;
        if signature.len() != SIGNATURE_BYTES {
            return Err(TokenError::BadSignature);
        }
        let mut named = false;
        let mut verified = false;
        for (id, key) in &self.keys {
            if *id != key_id {
                continue;
            }
            named = true;
            if UnparsedPublicKey::new(&ED25519, key)
                .verify(signed.as_bytes(), &signature)
                .is_ok()
            {
                verified = true;
                break;
            }
        }
        if !named {
            return Err(TokenError::UnknownKey);
        }
        if !verified {
            return Err(TokenError::BadSignature);
        }
        let claims: Claims = decode(claims, "the claims")?;
        if claims.iss != ISSUER || claims.aud != AUDIENCE {
            return Err(TokenError::Claims(
                "the issuer or the audience is not Bosun's",
            ));
        }
        let issued_at = DateTime::from_timestamp(claims.iat, 0)
            .ok_or(TokenError::Claims("the issue time is out of range"))?;
        let expires_at = DateTime::from_timestamp(claims.exp, 0)
            .ok_or(TokenError::Claims("the expiry is out of range"))?;
        if expires_at <= issued_at {
            return Err(TokenError::Claims("it expires before it is issued"));
        }
        if now >= expires_at {
            return Err(TokenError::Expired);
        }
        Ok(JobToken {
            identity: identity(&claims.scp)?,
            issued_at,
            expires_at,
            key_id,
        })
    }
}

fn decode<T: for<'de> Deserialize<'de>>(part: &str, what: &'static str) -> Result<T, TokenError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(part)
        .map_err(|_| TokenError::Malformed(what))?;
    serde_json::from_slice(&bytes).map_err(|_| TokenError::Malformed(what))
}

fn key_id(public_key: &[u8]) -> String {
    let hash = digest(&SHA256, public_key);
    URL_SAFE_NO_PAD.encode(&hash.as_ref()[..12])
}

fn scopes(identity: JobIdentity) -> String {
    let JobIdentity { attempt, execution } = identity;
    format!(
        "{SCOPE_READ}:{attempt} {SCOPE_UPLOAD}:{attempt}:{execution} {SCOPE_RESULTS}:{attempt}:{execution}"
    )
}

fn identity(scp: &str) -> Result<JobIdentity, TokenError> {
    let mut read = None;
    let mut upload = None;
    let mut results = None;
    for scope in scp.split(' ') {
        let mut fields = scope.split(':');
        let name = fields.next().unwrap_or_default();
        let slot = match name {
            SCOPE_READ => &mut read,
            SCOPE_UPLOAD => &mut upload,
            SCOPE_RESULTS => &mut results,
            _ => continue,
        };
        let attempt = fields
            .next()
            .and_then(|text| text.parse::<AttemptId>().ok())
            .ok_or(TokenError::Claims("a scope names no valid run"))?;
        let execution = match name {
            SCOPE_READ => None,
            _ => Some(
                fields
                    .next()
                    .and_then(|text| text.parse::<ExecutionId>().ok())
                    .ok_or(TokenError::Claims("a scope names no valid job"))?,
            ),
        };
        if fields.next().is_some() || slot.replace((attempt, execution)).is_some() {
            return Err(TokenError::Claims("a scope is malformed or repeated"));
        }
    }
    match (read, upload, results) {
        (Some((read, _)), Some((upload, Some(upload_job))), Some((attempt, Some(execution))))
            if read == attempt && upload == attempt && upload_job == execution =>
        {
            Ok(JobIdentity { attempt, execution })
        }
        _ => Err(TokenError::Claims(
            "the scopes of the runtime services are missing or disagree",
        )),
    }
}
