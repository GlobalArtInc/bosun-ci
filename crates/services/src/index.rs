use std::fmt;
use std::future::Future;
use std::str::FromStr;

use bosun_blob::Key;
use bosun_types::id::{AttemptId, ExecutionId};
use chrono::{DateTime, Utc};

pub const MAX_ARTIFACT_ID: i64 = (1 << 53) - 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ArtifactId(i64);

impl ArtifactId {
    pub fn new(value: i64) -> Option<Self> {
        (1..=MAX_ARTIFACT_ID)
            .contains(&value)
            .then_some(Self(value))
    }

    pub fn get(self) -> i64 {
        self.0
    }
}

impl fmt::Display for ArtifactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for ArtifactId {
    type Err = InvalidArtifactId;

    fn from_str(value: &str) -> Result<Self, InvalidArtifactId> {
        if value.is_empty()
            || value.len() > 16
            || !value.bytes().all(|byte| byte.is_ascii_digit())
            || (value.len() > 1 && value.starts_with('0'))
        {
            return Err(InvalidArtifactId);
        }
        value
            .parse::<i64>()
            .ok()
            .and_then(Self::new)
            .ok_or(InvalidArtifactId)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidArtifactId;

impl fmt::Display for InvalidArtifactId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("not an artifact id")
    }
}

impl std::error::Error for InvalidArtifactId {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sha256([u8; 32]);

impl Sha256 {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn parse(value: &str) -> Option<Self> {
        let hex = value.strip_prefix("sha256:")?;
        if hex.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        let (pairs, _) = hex.as_bytes().as_chunks::<2>();
        for (byte, pair) in bytes.iter_mut().zip(pairs) {
            *byte = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Some(Self(bytes))
    }
}

fn nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

impl fmt::Display for Sha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("sha256:")?;
        self.0.iter().try_for_each(|byte| write!(f, "{byte:02x}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finalized {
    pub size: u64,
    pub sha256: Option<Sha256>,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Artifact {
    pub id: ArtifactId,
    pub attempt: AttemptId,
    pub execution: ExecutionId,
    pub name: String,
    pub key: Key,
    pub content_type: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub finalized: Option<Finalized>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewArtifact {
    pub attempt: AttemptId,
    pub execution: ExecutionId,
    pub name: String,
    pub content_type: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtifactFilter {
    pub name: Option<String>,
    pub id: Option<ArtifactId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexError {
    AlreadyExists,
    NotFound,
    Finalized,
    Backend(String),
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyExists => f.write_str("an artifact of this name exists"),
            Self::NotFound => f.write_str("the artifact does not exist"),
            Self::Finalized => f.write_str("the artifact was finalised with other values"),
            Self::Backend(problem) => write!(f, "the artifact index failed: {problem}"),
        }
    }
}

impl std::error::Error for IndexError {}

pub trait Index: Send + Sync {
    fn create_artifact(
        &self,
        artifact: NewArtifact,
    ) -> impl Future<Output = Result<Artifact, IndexError>> + Send;

    fn artifact(
        &self,
        id: ArtifactId,
    ) -> impl Future<Output = Result<Option<Artifact>, IndexError>> + Send;

    fn artifacts(
        &self,
        visible_to: AttemptId,
        filter: &ArtifactFilter,
    ) -> impl Future<Output = Result<Vec<Artifact>, IndexError>> + Send;

    fn finalize_artifact(
        &self,
        id: ArtifactId,
        finalized: Finalized,
    ) -> impl Future<Output = Result<Artifact, IndexError>> + Send;

    fn delete_artifact(
        &self,
        visible_to: AttemptId,
        id: ArtifactId,
    ) -> impl Future<Output = Result<bool, IndexError>> + Send;

    fn register_block(
        &self,
        id: ArtifactId,
        block: &str,
        limit: usize,
    ) -> impl Future<Output = Result<bool, IndexError>> + Send;
}
