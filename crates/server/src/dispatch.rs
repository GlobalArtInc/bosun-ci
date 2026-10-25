mod inputs;

use axum::extract::State;
use bosun_db::projects::Binding;
use bosun_db::runs::{NewRun, Trust};
use bosun_forge::{Forge as _, Repository};
use bosun_types::api::{DispatchRequest, Run, Workflow, WorkflowSource};
use bosun_types::diagnostic::Diagnostic;
use bosun_types::error::{ErrorBody, ErrorCode};
use bosun_types::id::RevisionId;
use bosun_types::ir::{self, Trigger};
use bosun_workflow::github::{self, Dispatch};
use sea_orm::{DatabaseTransaction, TransactionTrait};

use crate::access::{Caller, OnProject, RunDispatch};
use crate::envelope::ApiError;
use crate::forge::Forge;
use crate::forge_events;
use crate::reply::Created;
use crate::request::{Faults, Idempotent, Segment, database, failed};
use crate::{AppState, idempotency, planner, workflows};

const DISPATCH: &str = "workflow_dispatch";

const BRANCHES: &str = "refs/heads/";

const TAGS: &str = "refs/tags/";

const REFUSED: &str = "the dispatch is not valid";

struct Checkout<'a> {
    forge: &'a Forge,
    repository: Repository,
    full_name: String,
    git_ref: String,
    sha: String,
    default_branch: bool,
}

struct Source {
    described: String,
    revision_id: RevisionId,
    ir: ir::Workflow,
    warnings: Vec<Diagnostic>,
}

pub(crate) async fn dispatch(
    State(state): State<AppState>,
    caller: Caller<OnProject<RunDispatch>>,
    Segment(reference): Segment,
    body: Result<Idempotent<DispatchRequest>, ApiError>,
) -> Result<Created<Run>, ApiError> {
    let workflow = workflows::find(&state.db, &caller, &reference).await?;
    let body = body?;
    let request = &body.value;

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
    let binding = bosun_db::projects::binding(&transaction, workflow.project_id)
        .await
        .map_err(database)?
        .ok_or_else(|| failed("a project vanished while one of its workflows was dispatched"))?;
    let mut faults = Faults::default();
    let checkout = match &binding.project.repository {
        None => {
            if let Some(given) = &request.r#ref {
                faults.add(
                    "ref",
                    format!(
                        "`{given}` is a ref, and a run of a project without a repository has none \
                         to take its code from"
                    ),
                );
            }
            None
        }
        Some(bound) => {
            let (forge, repository) =
                forge_events::readable(state.forge.as_deref(), &binding, bound)?;
            let full_name = forge_events::full_name(bound);
            let resolved = resolve(
                forge,
                &repository,
                &full_name,
                &bound.default_branch,
                request.r#ref.as_deref(),
            )
            .await?;
            match resolved {
                Ok((git_ref, sha)) => Some(Checkout {
                    default_branch: git_ref == format!("{BRANCHES}{}", bound.default_branch),
                    forge,
                    repository,
                    full_name,
                    git_ref,
                    sha,
                }),
                Err(message) => {
                    faults.add("ref", message);
                    None
                }
            }
        }
    };
    let source = match workflow.source {
        WorkflowSource::Server => server_side(&transaction, &workflow).await?,
        WorkflowSource::Repository => {
            let Some(checkout) = &checkout else {
                if binding.project.repository.is_none() {
                    return Err(ApiError::new(
                        ErrorCode::InvalidState,
                        format!(
                            "workflow `{}` is a file of a repository the project is no longer \
                             bound to, so it cannot be dispatched",
                            workflow.name
                        ),
                    ));
                }
                return Err(faults.refuse(REFUSED));
            };
            repository_file(&state, &transaction, &binding, checkout, &workflow).await?
        }
    };
    let declared = source
        .ir
        .triggers
        .iter()
        .find_map(|trigger| match trigger {
            Trigger::WorkflowDispatch(dispatch) => Some(dispatch.inputs.as_slice()),
            _ => None,
        })
        .unwrap_or_default();
    let inputs = inputs::check(declared, &request.inputs, &mut faults);
    faults.finish(REFUSED)?;
    if let Some(unapplied) = planner::unapplied(&source.ir) {
        return Err(ApiError::new(
            ErrorCode::InvalidState,
            format!(
                "{} uses {unapplied}, so it cannot be dispatched yet",
                source.described
            ),
        ));
    }
    let payload = github::dispatch_payload(&Dispatch {
        inputs: inputs.texts,
        git_ref: checkout.as_ref().map(|checkout| checkout.git_ref.clone()),
        workflow: workflow.name.clone(),
        repository: None,
        organization: None,
        sender: None,
    });
    let run = planner::dispatch(
        &transaction,
        NewRun {
            workflow: &workflow,
            revision_id: source.revision_id,
            ir: &source.ir,
            event: DISPATCH,
            git_ref: checkout.as_ref().map(|checkout| checkout.git_ref.as_str()),
            sha: checkout.as_ref().map(|checkout| checkout.sha.as_str()),
            payload: &payload,
            inputs: &inputs.typed,
            actor: &caller.principal.actor(),
            trust: Trust::Trusted,
        },
        &source.warnings,
        &state.site,
    )
    .await
    .map_err(database)?;
    if workflow.source == WorkflowSource::Repository
        && checkout.is_some_and(|checkout| checkout.default_branch)
    {
        bosun_db::workflows::make_current(&transaction, workflow.id, source.revision_id)
            .await
            .map_err(database)?;
    }
    idempotency::commit(&state.db, transaction, body.idempotency.as_ref(), run).await
}

async fn resolve(
    forge: &Forge,
    repository: &Repository,
    full_name: &str,
    default_branch: &str,
    given: Option<&str>,
) -> Result<Result<(String, String), String>, ApiError> {
    let candidates = match given {
        None => vec![format!("{BRANCHES}{default_branch}")],
        Some(given) if given.starts_with(BRANCHES) || given.starts_with(TAGS) => {
            vec![given.to_owned()]
        }
        Some(given) if given.is_empty() || given.starts_with("refs/") => {
            return Ok(Err(format!(
                "`{given}` is neither a branch nor a tag, and a dispatch runs the code of one"
            )));
        }
        Some(given) => vec![format!("{BRANCHES}{given}"), format!("{TAGS}{given}")],
    };
    for candidate in candidates {
        let resolved = forge
            .app
            .resolve(repository, &candidate)
            .await
            .map_err(|error| forge_events::refused(error.into(), full_name))?;
        if let Some(sha) = resolved {
            return Ok(Ok((candidate, sha)));
        }
    }
    match given {
        Some(given) => Ok(Err(format!(
            "the repository `{full_name}` has no branch or tag `{given}`"
        ))),
        None => Err(ApiError::new(
            ErrorCode::InvalidState,
            format!(
                "the default branch `{default_branch}` of the repository `{full_name}` has no \
                 commit to run"
            ),
        )),
    }
}

async fn server_side(
    transaction: &DatabaseTransaction,
    workflow: &Workflow,
) -> Result<Source, ApiError> {
    let revision_id = workflow.revision_id.ok_or_else(|| {
        ApiError::new(
            ErrorCode::InvalidState,
            format!("workflow `{}` has no revision to run", workflow.name),
        )
    })?;
    let revision = bosun_db::workflows::revision(transaction, revision_id)
        .await
        .map_err(database)?
        .ok_or_else(|| failed(format!("revision {revision_id} of a workflow is missing")))?;
    let parsed =
        bosun_workflow::parse(&workflow.name, &revision.source).map_err(|diagnostics| {
            invalid(
                format!(
                    "the current revision of workflow `{}` is not valid for this server, so it \
                     cannot be dispatched until a valid source is saved",
                    workflow.name
                ),
                diagnostics,
            )
        })?;
    Ok(Source {
        described: format!("the current revision of workflow `{}`", workflow.name),
        revision_id,
        ir: parsed.workflow,
        warnings: parsed.warnings,
    })
}

async fn repository_file(
    state: &AppState,
    transaction: &DatabaseTransaction,
    binding: &Binding,
    checkout: &Checkout<'_>,
    workflow: &Workflow,
) -> Result<Source, ApiError> {
    let found = forge_events::file_at(
        transaction,
        checkout.forge,
        &state.parses,
        binding,
        &checkout.repository,
        &checkout.sha,
        workflow,
    )
    .await
    .map_err(|failure| forge_events::refused(failure, &checkout.full_name))?;
    let Some((revision_id, lowered)) = found else {
        let mut faults = Faults::default();
        faults.add(
            "ref",
            format!(
                "`{}` is not a workflow of the project at `{}`",
                workflow.name, checkout.git_ref
            ),
        );
        return Err(faults.refuse(REFUSED));
    };
    let described = format!("workflow `{}` at `{}`", workflow.name, checkout.git_ref);
    let Some(ir) = lowered.workflow.clone() else {
        return Err(invalid(
            format!("{described} is not valid for this server, so it cannot be dispatched"),
            lowered.diagnostics.clone(),
        ));
    };
    Ok(Source {
        described,
        revision_id,
        ir,
        warnings: lowered.warnings(),
    })
}

fn invalid(message: String, diagnostics: Vec<Diagnostic>) -> ApiError {
    let mut body = ErrorBody::new(ErrorCode::InvalidState, message);
    body.diagnostics = diagnostics;
    ApiError(body)
}
