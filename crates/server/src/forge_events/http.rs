use axum::extract::State;
use bosun_db::projects::Binding;
use bosun_forge::Repository;
use bosun_types::api::{self, PlannedRuns, RepositoryDispatchRequest};
use bosun_types::error::ErrorCode;
use bosun_workflow::github::{self, RepositoryDispatch};
use sea_orm::TransactionTrait;
use serde_json::Value;
use tracing::warn;

use super::{Failed, Occurrence, repository_runs, server_runs};
use crate::access::{Caller, OnProject, RunDispatch};
use crate::envelope::ApiError;
use crate::forge::Forge;
use crate::projects::ProjectPath;
use crate::reply::Created;
use crate::request::{Faults, Idempotent, database, failed};
use crate::{AppState, idempotency};

const EVENT: &str = "repository_dispatch";

const LONGEST_EVENT_TYPE: usize = 100;

const MOST_PROPERTIES: usize = 10;

pub(crate) async fn receive(
    State(state): State<AppState>,
    caller: Caller<OnProject<RunDispatch>>,
    project: ProjectPath,
    body: Result<Idempotent<RepositoryDispatchRequest>, ApiError>,
) -> Result<Created<PlannedRuns>, ApiError> {
    let project = project.resolve(&state.db, &caller).await?;
    let body = body?;
    let request = &body.value;
    let mut faults = Faults::default();
    let length = request.event_type.chars().count();
    if length == 0 || length > LONGEST_EVENT_TYPE {
        faults.add(
            "event_type",
            format!(
                "an event type is 1 to {LONGEST_EVENT_TYPE} characters, and this one is {length}"
            ),
        );
    }
    if request.client_payload.len() > MOST_PROPERTIES {
        faults.add(
            "client_payload",
            format!(
                "a client payload has at most {MOST_PROPERTIES} properties at its top, and this \
                 one has {}",
                request.client_payload.len()
            ),
        );
    }
    faults.finish("the event is not valid")?;

    let transaction = state.db.begin().await.map_err(database)?;
    if let Some(replayed) = idempotency::replay(&transaction, body.idempotency.as_ref()).await? {
        return Ok(replayed);
    }
    let binding = bosun_db::projects::binding(&transaction, project.id)
        .await
        .map_err(database)?
        .ok_or_else(|| failed("a project vanished while an event came for it"))?;
    let payload = github::repository_dispatch_payload(&RepositoryDispatch {
        event_type: request.event_type.clone(),
        client_payload: Value::Object(request.client_payload.clone()),
        branch: binding
            .project
            .repository
            .as_ref()
            .map(|repository| repository.default_branch.clone()),
        repository: None,
        organization: None,
        sender: None,
        installation: None,
    });
    let actor = caller.principal.actor();
    let occurrence = Occurrence {
        event: EVENT,
        action: Some(&request.event_type),
        payload: &payload,
        actor: &actor,
        attempt: 0,
        at: state.clock.now(),
        starts_runs: true,
    };
    let runs = match &binding.project.repository {
        None => server_runs(&transaction, &state.site, &binding, &occurrence)
            .await
            .map_err(database)?,
        Some(repository) => {
            let (forge, readable) = readable(state.forge.as_deref(), &binding, repository)?;
            repository_runs(
                &transaction,
                forge,
                &state.parses,
                &state.site,
                &binding,
                readable.installation,
                &occurrence,
            )
            .await
            .map_err(|failure| refused(failure, &full_name(repository)))?
            .runs
        }
    };
    idempotency::commit(
        &state.db,
        transaction,
        body.idempotency.as_ref(),
        PlannedRuns { runs },
    )
    .await
}

pub(crate) fn readable<'a>(
    forge: Option<&'a Forge>,
    binding: &Binding,
    bound: &api::Repository,
) -> Result<(&'a Forge, Repository), ApiError> {
    let full_name = full_name(bound);
    let forge = forge.ok_or_else(|| {
        ApiError::new(
            ErrorCode::InvalidState,
            format!(
                "the project is bound to the repository `{full_name}`, and this server has no \
                 GitHub App to read its workflows with"
            ),
        )
    })?;
    let installation = binding.installation.ok_or_else(|| {
        ApiError::new(
            ErrorCode::InvalidState,
            format!(
                "no installation of the GitHub App sees the repository `{full_name}`, so its \
                 workflows cannot be read"
            ),
        )
    })?;
    Ok((
        forge,
        Repository {
            installation,
            owner: bound.owner.clone(),
            name: bound.name.clone(),
        },
    ))
}

pub(crate) fn full_name(bound: &api::Repository) -> String {
    format!("{}/{}", bound.owner, bound.name)
}

pub(crate) fn refused(failure: Failed, full_name: &str) -> ApiError {
    match failure {
        Failed::Database(error) => database(error),
        Failed::Payload(message) | Failed::Unmerged(message) => failed(message),
        Failed::Forge(bosun_forge::Error::RateLimited { reset }) => ApiError::new(
            ErrorCode::RateLimited,
            format!(
                "the forge refuses requests until {reset}, so the workflows of `{full_name}` \
                 cannot be read"
            ),
        ),
        Failed::Forge(error) => {
            warn!(%error, repository = full_name, "the forge failed while workflows of a repository were read");
            ApiError::new(
                ErrorCode::Internal,
                format!("the workflows of `{full_name}` could not be read from the forge: {error}"),
            )
        }
    }
}
