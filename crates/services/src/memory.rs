use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Mutex, MutexGuard, PoisonError};

use bosun_blob::Key;
use bosun_types::id::AttemptId;

use crate::index::{
    Artifact, ArtifactFilter, ArtifactId, Finalized, Index, IndexError, NewArtifact,
};

#[derive(Debug, Default)]
pub struct MemoryIndex {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    last: i64,
    artifacts: BTreeMap<ArtifactId, Artifact>,
    blocks: BTreeMap<ArtifactId, BTreeSet<String>>,
}

impl MemoryIndex {
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Index for MemoryIndex {
    async fn create_artifact(&self, new: NewArtifact) -> Result<Artifact, IndexError> {
        let mut state = self.state();
        if state
            .artifacts
            .values()
            .any(|artifact| artifact.attempt == new.attempt && artifact.name == new.name)
        {
            return Err(IndexError::AlreadyExists);
        }
        let id = state
            .last
            .checked_add(1)
            .and_then(ArtifactId::new)
            .ok_or_else(|| IndexError::Backend("the artifact ids are used up".to_owned()))?;
        let key = Key::new(format!("artifacts/local/{id}"))
            .map_err(|error| IndexError::Backend(error.to_string()))?;
        let artifact = Artifact {
            id,
            attempt: new.attempt,
            execution: new.execution,
            name: new.name,
            key,
            content_type: new.content_type,
            created_at: new.created_at,
            expires_at: new.expires_at,
            finalized: None,
        };
        state.last = id.get();
        state.artifacts.insert(id, artifact.clone());
        Ok(artifact)
    }

    async fn artifact(&self, id: ArtifactId) -> Result<Option<Artifact>, IndexError> {
        Ok(self.state().artifacts.get(&id).cloned())
    }

    async fn artifacts(
        &self,
        visible_to: AttemptId,
        filter: &ArtifactFilter,
    ) -> Result<Vec<Artifact>, IndexError> {
        Ok(self
            .state()
            .artifacts
            .values()
            .filter(|artifact| {
                artifact.attempt == visible_to
                    && filter
                        .name
                        .as_ref()
                        .is_none_or(|name| *name == artifact.name)
                    && filter.id.is_none_or(|id| id == artifact.id)
            })
            .cloned()
            .collect())
    }

    async fn finalize_artifact(
        &self,
        id: ArtifactId,
        finalized: Finalized,
    ) -> Result<Artifact, IndexError> {
        let mut state = self.state();
        let artifact = state.artifacts.get_mut(&id).ok_or(IndexError::NotFound)?;
        match &artifact.finalized {
            Some(earlier)
                if earlier.size == finalized.size && earlier.sha256 == finalized.sha256 => {}
            Some(_) => return Err(IndexError::Finalized),
            None => artifact.finalized = Some(finalized),
        }
        Ok(artifact.clone())
    }

    async fn delete_artifact(
        &self,
        visible_to: AttemptId,
        id: ArtifactId,
    ) -> Result<bool, IndexError> {
        let mut state = self.state();
        match state.artifacts.get(&id) {
            Some(artifact) if artifact.attempt == visible_to => {
                state.artifacts.remove(&id);
                state.blocks.remove(&id);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    async fn register_block(
        &self,
        id: ArtifactId,
        block: &str,
        limit: usize,
    ) -> Result<bool, IndexError> {
        let mut state = self.state();
        if !state.artifacts.contains_key(&id) {
            return Err(IndexError::NotFound);
        }
        let blocks = state.blocks.entry(id).or_default();
        if blocks.contains(block) {
            return Ok(true);
        }
        if blocks.len() >= limit {
            return Ok(false);
        }
        blocks.insert(block.to_owned());
        Ok(true)
    }
}
