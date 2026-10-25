use std::collections::BTreeMap;

use axum::extract::State;
use bosun_db::runs::NewRun;
use bosun_types::api::{DispatchRequest, Run};
use bosun_types::error::{ErrorBody, ErrorCode};
use bosun_workflow::github::{self, Dispatch};
use sea_orm::TransactionTrait;

use crate::access::{Caller, OnProject, RunDispatch};
use crate::envelope::ApiError;
use crate::reply::Created;
use crate::request::{Faults, Idempotent, Segment, database, failed};
use crate::{AppState, idempotency, planner, workflows};

pub(crate) async fn dispatch(
    State(state): State<AppState>,
    caller: Caller<OnProject<RunDispatch>>,
    Segment(reference): Segment,
    body: Result<Idempotent<DispatchRequest>, ApiError>,
) -> Result<Created<Run>, ApiError> {
    let workflow = workflows::find(&state.db, &caller, &reference).await?;
    let body = body?;
    let request = &body.value;
    let mut faults = Faults::default();
    if let Some(given) = &request.r#ref {
        faults.add(
            "ref",
            format!(
                "`{given}` is a ref, and a run of a project without a repository has none to \
                 take its code from"
            ),
        );
    }
    for name in request.inputs.keys() {
        faults.add(
            format!("inputs.{name}"),
            format!(
                "the workflow declares no input `{name}`; inputs of workflow_dispatch arrive in M1"
            ),
        );
    }
    faults.finish("the dispatch is not valid")?;

    let transaction = state.db.begin().await.map_err(database)?;
    if let Some(replayed) = idempotency::replay(&transaction, body.idempotency.as_ref()).await? {
        return Ok(replayed);
    }
    if !workflow.enabled {
        return Err(ApiError::new(
            ErrorCode::InvalidState,
            format!(
                "workflow `{}` is disabled, so it cannot be dispatched",
                workflow.name
            ),
        ));
    }
    let revision_id = workflow.revision_id.ok_or_else(|| {
        ApiError::new(
            ErrorCode::InvalidState,
            format!("workflow `{}` has no revision to run", workflow.name),
        )
    })?;
    let revision = bosun_db::workflows::revision(&transaction, revision_id)
        .await
        .map_err(database)?
        .ok_or_else(|| failed(format!("revision {revision_id} of a workflow is missing")))?;
    let parsed =
        bosun_workflow::parse(&workflow.name, &revision.source).map_err(|diagnostics| {
            let mut body = ErrorBody::new(
                ErrorCode::InvalidState,
                format!(
                    "the current revision of workflow `{}` is not valid for this server, so it \
                 cannot be dispatched until a valid source is saved",
                    workflow.name
                ),
            );
            body.diagnostics = diagnostics;
            ApiError(body)
        })?;
    let ir = parsed.workflow;
    let payload = github::dispatch_payload(&Dispatch {
        inputs: BTreeMap::new(),
        git_ref: None,
        workflow: workflow.name.clone(),
        repository: None,
        organization: None,
        sender: None,
    });
    if let Some(unapplied) = planner::unapplied(&ir) {
        return Err(ApiError::new(
            ErrorCode::InvalidState,
            format!(
                "the current revision of workflow `{}` uses {unapplied}, so it cannot be \
                 dispatched yet",
                workflow.name
            ),
        ));
    }
    let run = planner::dispatch(
        &transaction,
        NewRun {
            workflow: &workflow,
            revision_id,
            ir: &ir,
            payload: &payload,
            actor: &caller.principal.actor(),
        },
        &parsed.warnings,
        &state.site,
    )
    .await
    .map_err(database)?;
    idempotency::commit(&state.db, transaction, body.idempotency.as_ref(), run).await
}
