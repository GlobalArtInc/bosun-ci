use axum::extract::{FromRequestParts, Query};
use axum::http::request::Parts;
use bosun_db::secrets::Scope;
use sea_orm::ConnectionTrait;
use serde_json::json;

use crate::access::{Caller, Key, OnScope};
use crate::envelope::ApiError;
use crate::openapi::{Input, Spec};
use crate::organisations::{self, ORGANISATION};
use crate::projects;
use crate::request::{Faults, Filters, database, invalid};

pub(crate) const PROJECT: &str = "project";

const REFUSAL: &str = "the query is not valid";

pub(crate) struct InScope;

impl Filters for InScope {
    const NAMES: &'static [&'static str] = &[ORGANISATION, PROJECT];

    fn description(name: &str) -> String {
        described(name)
    }
}

fn described(name: &str) -> String {
    let what = if name == ORGANISATION {
        "the organisation's id or slug"
    } else {
        "the project's id, or the slugs of its organisation and of itself joined by `/`"
    };
    format!(
        "{what}, whose own secrets or variables the request names; the request gives exactly one \
         of `{ORGANISATION}` and `{PROJECT}`"
    )
}

pub(crate) struct Scoped {
    organisation: Option<String>,
    project: Option<String>,
}

impl<S: Send + Sync> FromRequestParts<S> for Scoped {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Scoped, ApiError> {
        let Query(pairs) = Query::<Vec<(String, String)>>::from_request_parts(parts, state)
            .await
            .map_err(|rejection| {
                invalid(format!(
                    "the query string could not be read: {}",
                    rejection.body_text()
                ))
            })?;
        let mut scoped = Scoped {
            organisation: None,
            project: None,
        };
        for (name, value) in pairs {
            match name.as_str() {
                ORGANISATION => scoped.organisation = Some(value),
                PROJECT => scoped.project = Some(value),
                _ => {
                    return Err(invalid(format!(
                        "this request has no query parameter `{name}`; it takes \
                         `{ORGANISATION}` or `{PROJECT}`"
                    )));
                }
            }
        }
        Ok(scoped)
    }
}

impl Input for Scoped {
    fn input(spec: &mut Spec<'_>) {
        for name in [ORGANISATION, PROJECT] {
            spec.parameter(json!({
                "name": name,
                "in": "query",
                "description": described(name),
                "schema": { "type": "string" }
            }));
        }
    }
}

impl Scoped {
    pub(crate) fn organisation(&self) -> Option<&str> {
        self.organisation.as_deref()
    }

    pub(crate) fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }
}

pub(crate) struct Place {
    pub(crate) scope: Scope,
    pub(crate) name: String,
}

pub(crate) async fn resolve<C: ConnectionTrait, K: Key>(
    db: &C,
    caller: &Caller<OnScope<K>>,
    organisation: Option<&str>,
    project: Option<&str>,
) -> Result<Place, ApiError> {
    let refused = |field: &str, message: String| {
        let mut faults = Faults::default();
        faults.add(field, message);
        faults.refuse(REFUSAL)
    };
    match (organisation, project) {
        (Some(reference), None) => {
            let absent = || refused(ORGANISATION, organisations::missing(reference));
            let organisation = organisations::find(db, reference)
                .await?
                .ok_or_else(absent)?;
            caller.organisation(&organisation, absent)?;
            Ok(Place {
                scope: Scope {
                    organisation_id: organisation.id,
                    project_id: None,
                },
                name: format!("organisation `{}`", organisation.slug),
            })
        }
        (None, Some(reference)) => {
            let absent = || refused(PROJECT, projects::missing(reference));
            let project = projects::find(db, reference).await?.ok_or_else(absent)?;
            caller.project(&project, absent)?;
            let organisation = bosun_db::organisations::find(db, project.organisation_id)
                .await
                .map_err(database)?
                .ok_or_else(absent)?;
            Ok(Place {
                scope: Scope {
                    organisation_id: project.organisation_id,
                    project_id: Some(project.id),
                },
                name: format!("project `{}/{}`", organisation.slug, project.slug),
            })
        }
        (given, _) => {
            let problem = if given.is_some() {
                "is given together with the other; the request names its scope with exactly one \
                 of `organisation` and `project`"
            } else {
                "is missing; the request names its scope with exactly one of `organisation` and \
                 `project`"
            };
            let mut faults = Faults::default();
            faults.add(ORGANISATION, problem);
            faults.add(PROJECT, problem);
            Err(faults.refuse(REFUSAL))
        }
    }
}
