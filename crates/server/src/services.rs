use std::ops::RangeBounds;
use std::sync::Arc;

use axum::Router;
use axum::middleware;
use bosun_blob::{Key, Object, Store};
use bosun_db::artifacts::{self, Done, Finalizing, Registration, Stored};
use bosun_services::{
    AddressSigner, Artifact, ArtifactFilter, ArtifactId, Finalized, Index, IndexError, NewArtifact,
    Services, Sha256,
};
use chrono::TimeDelta;
use sea_orm::DatabaseConnection;
use tokio::io::AsyncRead;

use crate::job_token::authentication;
use crate::store::Backend;
use crate::{AppState, Error};

pub(crate) const ARTIFACT_RETENTION: TimeDelta = TimeDelta::days(90);

pub(crate) struct Served(pub(crate) Arc<Backend>);

impl Store for Served {
    async fn put<R>(&self, key: &Key, body: R) -> Result<(), bosun_blob::Error>
    where
        R: AsyncRead + Send + Unpin,
    {
        self.0.put(key, body).await
    }

    async fn get<B>(&self, key: &Key, range: B) -> Result<Object, bosun_blob::Error>
    where
        B: RangeBounds<u64> + Send,
    {
        self.0.get(key, range).await
    }

    async fn delete(&self, key: &Key) -> Result<(), bosun_blob::Error> {
        self.0.delete(key).await
    }

    async fn list(&self, prefix: &str) -> Result<Vec<Key>, bosun_blob::Error> {
        self.0.list(prefix).await
    }
}

pub(crate) struct Postgres {
    db: DatabaseConnection,
}

impl Postgres {
    pub(crate) fn new(db: DatabaseConnection) -> Postgres {
        Postgres { db }
    }
}

fn backend(error: bosun_db::Error) -> IndexError {
    IndexError::Backend(error.to_string())
}

fn artifact(stored: Stored) -> Result<Artifact, IndexError> {
    let id = ArtifactId::new(stored.id)
        .ok_or_else(|| IndexError::Backend(format!("{} is not an artifact id", stored.id)))?;
    let key = Key::new(stored.key).map_err(|error| IndexError::Backend(error.to_string()))?;
    Ok(Artifact {
        id,
        attempt: stored.attempt_id,
        execution: stored.execution_id,
        name: stored.name,
        key,
        content_type: stored.content_type,
        created_at: stored.created_at,
        expires_at: Some(stored.expires_at),
        finalized: stored.finalized.map(|done| Finalized {
            size: done.size,
            sha256: done.sha256.map(Sha256::new),
            at: done.at,
        }),
    })
}

impl Index for Postgres {
    async fn create_artifact(&self, new: NewArtifact) -> Result<Artifact, IndexError> {
        let ceiling = new.created_at + ARTIFACT_RETENTION;
        let expires_at = new.expires_at.map_or(ceiling, |asked| asked.min(ceiling));
        let created = artifacts::create(
            &self.db,
            &artifacts::New {
                attempt: new.attempt,
                execution: new.execution,
                name: &new.name,
                content_type: &new.content_type,
                created_at: new.created_at,
                expires_at,
            },
        )
        .await;
        match created {
            Ok(Some(stored)) => artifact(stored),
            Ok(None) => Err(IndexError::AlreadyExists),
            Err(bosun_db::Error::NoExecution(_)) => Err(IndexError::NotFound),
            Err(error) => Err(backend(error)),
        }
    }

    async fn artifact(&self, id: ArtifactId) -> Result<Option<Artifact>, IndexError> {
        artifacts::find(&self.db, id.get())
            .await
            .map_err(backend)?
            .map(artifact)
            .transpose()
    }

    async fn artifacts(
        &self,
        visible_to: bosun_types::id::AttemptId,
        filter: &ArtifactFilter,
    ) -> Result<Vec<Artifact>, IndexError> {
        artifacts::of_attempt(
            &self.db,
            visible_to,
            filter.name.as_deref(),
            filter.id.map(ArtifactId::get),
        )
        .await
        .map_err(backend)?
        .into_iter()
        .map(artifact)
        .collect()
    }

    async fn finalize_artifact(
        &self,
        id: ArtifactId,
        finalized: Finalized,
    ) -> Result<Artifact, IndexError> {
        let done = Done {
            size: finalized.size,
            sha256: finalized.sha256.map(|digest| *digest.bytes()),
            at: finalized.at,
        };
        match artifacts::finalize(&self.db, id.get(), &done)
            .await
            .map_err(backend)?
        {
            Finalizing::Done(stored) => artifact(*stored),
            Finalizing::Differs => Err(IndexError::Finalized),
            Finalizing::Missing => Err(IndexError::NotFound),
        }
    }

    async fn delete_artifact(
        &self,
        visible_to: bosun_types::id::AttemptId,
        id: ArtifactId,
    ) -> Result<bool, IndexError> {
        artifacts::delete_in_attempt(&self.db, visible_to, id.get())
            .await
            .map_err(backend)
    }

    async fn register_block(
        &self,
        id: ArtifactId,
        block: &str,
        limit: usize,
    ) -> Result<bool, IndexError> {
        match artifacts::register_block(&self.db, id.get(), block, limit)
            .await
            .map_err(backend)?
        {
            Registration::Accepted => Ok(true),
            Registration::Full => Ok(false),
            Registration::Missing => Err(IndexError::NotFound),
        }
    }
}

pub(crate) fn routes(state: &AppState) -> Result<Router, Error> {
    let origin = state.site.url();
    if origin.is_empty() {
        return Ok(Router::new());
    }
    let signer = AddressSigner::new(&state.keys.address_key()).map_err(Error::AddressKey)?;
    let clock = state.clock.clone();
    let services = Arc::new(
        Services::new(
            Served(Arc::clone(&state.store)),
            Postgres::new(state.db.clone()),
            signer,
            origin,
        )
        .map_err(Error::RuntimeServices)?
        .with_clock(Arc::new(move || clock.now())),
    );
    let twirp = services
        .twirp_router()
        .layer(middleware::from_fn_with_state(
            state.clone(),
            authentication::authenticated,
        ));
    Ok(twirp.merge(services.blob_router()))
}
