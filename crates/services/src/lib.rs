mod artifact;
mod blob;
mod index;
mod memory;
mod signed;
mod token;
mod twirp;

use std::fmt;
use std::sync::Arc;

use axum::Router;
use bosun_blob::Store;
use chrono::{DateTime, Utc};

pub use blob::{MAX_BLOCK_ID_BYTES, MAX_BLOCKS};
pub use index::{
    Artifact, ArtifactFilter, ArtifactId, Finalized, Index, IndexError, InvalidArtifactId,
    MAX_ARTIFACT_ID, NewArtifact, Sha256,
};
pub use memory::MemoryIndex;
pub use signed::{AddressError, AddressSigner, LIFETIME, MIN_SECRET_BYTES, Purpose};
pub use token::{JobIdentity, JobToken, KeySet, SigningKey, TokenError};

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsError(String);

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the runtime services cannot be set up: {}", self.0)
    }
}

impl std::error::Error for SettingsError {}

pub struct Services<S, I> {
    store: S,
    index: I,
    signer: AddressSigner,
    origin: String,
    clock: Clock,
    max_blocks: usize,
}

impl<S, I> Services<S, I> {
    pub fn new(
        store: S,
        index: I,
        signer: AddressSigner,
        public_url: &str,
    ) -> Result<Self, SettingsError> {
        let url = url::Url::parse(public_url)
            .map_err(|error| SettingsError(format!("`{public_url}` is not an address: {error}")))?;
        let plain = matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && matches!(url.path(), "" | "/")
            && url.query().is_none()
            && url.fragment().is_none()
            && url.username().is_empty()
            && url.password().is_none();
        if !plain {
            return Err(SettingsError(format!(
                "`{public_url}` is not an http or https origin without a path"
            )));
        }
        Ok(Self {
            store,
            index,
            signer,
            origin: url.origin().ascii_serialization(),
            clock: Arc::new(Utc::now),
            max_blocks: MAX_BLOCKS,
        })
    }

    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    pub fn with_block_limit(mut self, limit: usize) -> Self {
        self.max_blocks = limit.min(MAX_BLOCKS);
        self
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn index(&self) -> &I {
        &self.index
    }

    fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }
}

impl<S, I> Services<S, I>
where
    S: Store + 'static,
    I: Index + 'static,
{
    pub fn twirp_router(self: &Arc<Self>) -> Router {
        artifact::router(Arc::clone(self))
    }

    pub fn blob_router(self: &Arc<Self>) -> Router {
        blob::router(Arc::clone(self))
    }
}
