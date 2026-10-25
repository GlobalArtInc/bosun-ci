use axum::Json;
use axum::extract::State;
use bosun_db::aliases::NewAlias;
use bosun_types::api::{Capability, LabelAlias, Page, SetLabelAliasRequest};
use tracing::info;

use crate::access::{AgentManage, Caller, OnInstallation};
use crate::agents::{LABEL_RULE, valid_label};
use crate::envelope::ApiError;
use crate::openapi::Api;
use crate::reply::Saved;
use crate::request::{Body, Faults, Listing, database};
use crate::{AppState, organisations};

const IMAGE_LENGTH: usize = 255;

pub(crate) fn routes() -> Api {
    Api::new()
        .get("/api/v1/label-aliases", list)
        .post("/api/v1/label-aliases", set)
}

async fn list(
    State(state): State<AppState>,
    caller: Caller<OnInstallation<AgentManage>>,
    listing: Result<Listing, ApiError>,
) -> Result<Json<Page<LabelAlias>>, ApiError> {
    caller.installation()?;
    let listing = listing?;
    let aliases = bosun_db::aliases::list(&state.db, listing.cursor(), listing.fetch())
        .await
        .map_err(database)?;
    Ok(Json(listing.page(aliases, |alias| alias.id.as_uuid())))
}

async fn set(
    State(state): State<AppState>,
    caller: Caller<OnInstallation<AgentManage>>,
    body: Result<Body<SetLabelAliasRequest>, ApiError>,
) -> Result<Saved<LabelAlias>, ApiError> {
    caller.installation()?;
    let Body(request) = body?;
    let mut faults = Faults::default();
    if !valid_label(&request.label) {
        faults.add("label", LABEL_RULE);
    }
    let organisation = match &request.organisation {
        Some(reference) => match organisations::find(&state.db, reference).await? {
            Some(organisation) => Some(organisation.id),
            None => {
                faults.add("organisation", organisations::missing(reference));
                None
            }
        },
        None => None,
    };
    let pool = match &request.pool {
        Some(name) => match bosun_db::pools::named(&state.db, name)
            .await
            .map_err(database)?
        {
            Some(pool) => Some(pool.id),
            None => {
                faults.add("pool", format!("there is no pool `{name}`"));
                None
            }
        },
        None => None,
    };
    if let Some(image) = &request.image
        && !valid_image(image)
    {
        faults.add(
            "image",
            format!(
                "an image is 1 to {IMAGE_LENGTH} characters without white space or control \
                 characters"
            ),
        );
    }
    let capability = match (request.capability, &request.image) {
        (Some(Capability::Shell), Some(_)) => {
            faults.add(
                "capability",
                "is `shell`, and the jobs of an alias with an image run in a container, which \
                 needs `podman`",
            );
            None
        }
        (None, Some(_)) => Some(Capability::Podman),
        (capability, _) => capability,
    };
    faults.finish("the label alias is not valid")?;

    let label = request.label.to_lowercase();
    let set = bosun_db::aliases::set(
        &state.db,
        NewAlias {
            organisation,
            label: &label,
            pool,
            capability,
            image: request.image.as_deref(),
        },
    )
    .await
    .map_err(database)?;
    info!(
        alias_id = %set.alias.id,
        label,
        organisation_id = ?organisation,
        created = set.created,
        "set a label alias"
    );
    Ok(if set.created {
        Saved::New(set.alias)
    } else {
        Saved::Again(set.alias)
    })
}

fn valid_image(image: &str) -> bool {
    (1..=IMAGE_LENGTH).contains(&image.chars().count())
        && image
            .chars()
            .all(|character| !character.is_whitespace() && !character.is_control())
}
