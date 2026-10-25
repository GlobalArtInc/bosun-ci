use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use bosun_db::projects::{Holder, Reach};
use bosun_db::roles::Grants;
use bosun_db::tokens::Carried;
use bosun_types::api::{Organisation, Permissions, Project, User};
use bosun_types::command::Command;
use bosun_types::error::ErrorCode;
use bosun_types::permission::Permission;
use sea_orm::ConnectionTrait;
use serde_json::{Value, json};

use crate::auth::Principal;
use crate::commands;
use crate::envelope::ApiError;
use crate::openapi::{Input, Spec};
use crate::request::{database, failed};

pub(crate) trait Key {
    const KEY: Permission;
}

macro_rules! keys {
    ($($name:ident),* $(,)?) => {
        $(
            pub(crate) struct $name;

            impl Key for $name {
                const KEY: Permission = Permission::$name;
            }
        )*
    };
}

keys!(
    ProjectView,
    RunDispatch,
    WorkflowEdit,
    WorkflowManage,
    ProjectManage,
    OrganisationManage,
    AgentManage,
);

pub(crate) trait Rule {
    fn declaration() -> Value;
}

pub(crate) struct Authenticated;

pub(crate) struct OnInstallation<K>(PhantomData<fn() -> K>);

pub(crate) struct OnOrganisation<K>(PhantomData<fn() -> K>);

pub(crate) struct OnProject<K>(PhantomData<fn() -> K>);

pub(crate) struct Listed<K>(PhantomData<fn() -> K>);

pub(crate) struct ByCommand;

pub(crate) struct Subscribed;

impl Rule for Authenticated {
    fn declaration() -> Value {
        json!({ "on": "authenticated" })
    }
}

impl<K: Key> Rule for OnInstallation<K> {
    fn declaration() -> Value {
        json!({ "on": "installation", "key": K::KEY })
    }
}

impl<K: Key> Rule for OnOrganisation<K> {
    fn declaration() -> Value {
        json!({ "on": "organisation", "key": K::KEY })
    }
}

impl<K: Key> Rule for OnProject<K> {
    fn declaration() -> Value {
        json!({ "on": "project", "key": K::KEY })
    }
}

impl<K: Key> Rule for Listed<K> {
    fn declaration() -> Value {
        json!({ "on": "items", "key": K::KEY })
    }
}

impl Rule for Subscribed {
    fn declaration() -> Value {
        json!({
            "on": "items",
            "key": Permission::ProjectView,
            "installation": Permission::AgentManage
        })
    }
}

impl Rule for ByCommand {
    fn declaration() -> Value {
        let by_command: serde_json::Map<String, Value> = commands::COMMANDS
            .iter()
            .map(|command| {
                (
                    commands::name(command).to_owned(),
                    json!(command.permission()),
                )
            })
            .collect();
        json!({ "on": "project", "by_command": by_command })
    }
}

pub(crate) struct Caller<R> {
    pub(crate) principal: Principal,
    rule: PhantomData<fn() -> R>,
}

impl<S: Send + Sync, R> FromRequestParts<S> for Caller<R> {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Caller<R>, ApiError> {
        let principal = parts
            .extensions
            .get::<Principal>()
            .cloned()
            .ok_or_else(|| failed("a route behind credentials was reached without a principal"))?;
        Ok(Caller {
            principal,
            rule: PhantomData,
        })
    }
}

impl<R: Rule> Input for Caller<R> {
    fn input(spec: &mut Spec<'_>) {
        spec.permission(R::declaration());
    }
}

impl<R> Caller<R> {
    fn user(&self) -> &User {
        &self.principal.user
    }

    fn permissions(&self) -> &Permissions {
        &self.principal.permissions
    }

    fn check(
        &self,
        held: BTreeSet<Permission>,
        needed: Permission,
        place: impl FnOnce() -> String,
        missing: impl FnOnce() -> ApiError,
    ) -> Result<(), ApiError> {
        if held.is_empty() {
            return Err(missing());
        }
        if held.contains(&needed) {
            return Ok(());
        }
        Err(ApiError::new(
            ErrorCode::Forbidden,
            format!(
                "`{}` does not hold `{needed}` on {}",
                self.user().login,
                place()
            ),
        ))
    }

    async fn project_of<C: ConnectionTrait>(
        &self,
        db: &C,
        holder: Holder,
        missing: impl Fn() -> ApiError,
    ) -> Result<Project, ApiError> {
        let project = bosun_db::projects::holding(db, holder)
            .await
            .map_err(database)?
            .ok_or_else(&missing)?;
        if self
            .permissions()
            .on_project(project.organisation_id, project.id)
            .is_empty()
        {
            return Err(missing());
        }
        Ok(project)
    }

    fn on_project(
        &self,
        needed: Permission,
        project: &Project,
        missing: impl FnOnce() -> ApiError,
    ) -> Result<(), ApiError> {
        self.check(
            self.permissions()
                .on_project(project.organisation_id, project.id),
            needed,
            || format!("project `{}`", project.slug),
            missing,
        )
    }
}

impl<K: Key> Caller<OnInstallation<K>> {
    pub(crate) fn installation(&self) -> Result<(), ApiError> {
        if self.permissions().installation.contains(&K::KEY) {
            return Ok(());
        }
        Err(ApiError::new(
            ErrorCode::Forbidden,
            format!(
                "`{}` does not hold `{}` on this installation, which only its owners do",
                self.user().login,
                K::KEY
            ),
        ))
    }
}

impl<K: Key> Caller<OnOrganisation<K>> {
    pub(crate) fn organisation(
        &self,
        organisation: &Organisation,
        missing: impl FnOnce() -> ApiError,
    ) -> Result<(), ApiError> {
        self.check(
            self.permissions().on_organisation(organisation.id),
            K::KEY,
            || format!("organisation `{}`", organisation.slug),
            missing,
        )
    }
}

impl<K: Key> Caller<OnProject<K>> {
    pub(crate) fn project(
        &self,
        project: &Project,
        missing: impl FnOnce() -> ApiError,
    ) -> Result<(), ApiError> {
        self.on_project(K::KEY, project, missing)
    }

    pub(crate) async fn holding<C: ConnectionTrait>(
        &self,
        db: &C,
        holder: Holder,
        missing: impl Fn() -> ApiError,
    ) -> Result<Project, ApiError> {
        let project = self.project_of(db, holder, &missing).await?;
        self.project(&project, missing)?;
        Ok(project)
    }
}

impl<K: Key> Caller<Listed<K>> {
    pub(crate) fn reach(&self) -> Reach {
        reach(self.permissions(), K::KEY)
    }
}

impl Caller<Subscribed> {
    pub(crate) fn reach(&self) -> Reach {
        reach(self.permissions(), Permission::ProjectView)
    }

    pub(crate) fn installation(&self) -> bool {
        self.permissions()
            .installation
            .contains(&Permission::AgentManage)
    }
}

fn reach(permissions: &Permissions, key: Permission) -> Reach {
    Reach {
        everywhere: permissions.installation.contains(&key),
        organisations: permissions
            .organisations
            .iter()
            .filter(|(_, held)| held.contains(&key))
            .map(|(organisation, _)| *organisation)
            .collect(),
        projects: permissions
            .projects
            .iter()
            .filter(|(_, held)| held.contains(&key))
            .map(|(project, _)| *project)
            .collect(),
    }
}

impl Caller<ByCommand> {
    pub(crate) async fn holding<C: ConnectionTrait>(
        &self,
        db: &C,
        holder: Holder,
        missing: impl Fn() -> ApiError,
    ) -> Result<Project, ApiError> {
        self.project_of(db, holder, missing).await
    }

    pub(crate) fn command(&self, command: &Command, project: &Project) -> Result<(), ApiError> {
        self.on_project(command.permission(), project, || {
            failed("a command reached a project its caller holds nothing on")
        })
    }
}

pub(crate) async fn load<C: ConnectionTrait>(
    db: &C,
    user: &User,
    carried: Option<&Carried>,
) -> Result<Permissions, bosun_db::Error> {
    let (installation, grants) = if user.owner {
        (Permission::ALL.into_iter().collect(), Grants::default())
    } else {
        (BTreeSet::new(), bosun_db::roles::grants(db, user.id).await?)
    };
    Ok(effective(installation, grants, carried))
}

fn effective(
    installation: BTreeSet<Permission>,
    grants: Grants,
    carried: Option<&Carried>,
) -> Permissions {
    let carries = carried.and_then(|carried| carried.permissions.as_ref());
    let keep = |held: BTreeSet<Permission>| -> BTreeSet<Permission> {
        match carries {
            Some(keys) => held.intersection(keys).copied().collect(),
            None => held,
        }
    };
    let Some(scopes) = carried.and_then(|carried| carried.scopes.as_ref()) else {
        return Permissions {
            installation: keep(installation),
            organisations: present(
                grants
                    .organisations
                    .into_iter()
                    .map(|(organisation, held)| (organisation, keep(held))),
            ),
            projects: present(
                grants
                    .projects
                    .into_iter()
                    .map(|(project, grant)| (project, keep(grant.permissions))),
            ),
        };
    };
    let on_organisation = |organisation| {
        let mut held = installation.clone();
        if let Some(granted) = grants.organisations.get(&organisation) {
            held.extend(granted);
        }
        held
    };
    let mut organisations = BTreeMap::new();
    let mut projects: BTreeMap<_, BTreeSet<Permission>> = BTreeMap::new();
    for scope in scopes.iter().filter(|scope| scope.project_id.is_none()) {
        organisations.insert(
            scope.organisation_id,
            keep(on_organisation(scope.organisation_id)),
        );
        for (project, grant) in &grants.projects {
            if grant.organisation_id == scope.organisation_id {
                projects
                    .entry(*project)
                    .or_default()
                    .extend(keep(grant.permissions.clone()));
            }
        }
    }
    for scope in scopes {
        let Some(project) = scope.project_id else {
            continue;
        };
        let mut held = on_organisation(scope.organisation_id);
        if let Some(grant) = grants.projects.get(&project) {
            held.extend(&grant.permissions);
        }
        projects.entry(project).or_default().extend(keep(held));
    }
    Permissions {
        installation: BTreeSet::new(),
        organisations: present(organisations),
        projects: present(projects),
    }
}

fn present<K: Ord>(
    entries: impl IntoIterator<Item = (K, BTreeSet<Permission>)>,
) -> BTreeMap<K, BTreeSet<Permission>> {
    entries
        .into_iter()
        .filter(|(_, held)| !held.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use bosun_db::roles::{Grants, ProjectGrant};
    use bosun_db::tokens::{Carried, Scope};
    use bosun_types::api::Permissions;
    use bosun_types::id::{OrganisationId, ProjectId};
    use bosun_types::permission::Permission;
    use sea_orm::prelude::Uuid;

    use super::effective;

    const VIEW: Permission = Permission::ProjectView;

    const DISPATCH: Permission = Permission::RunDispatch;

    const EDIT: Permission = Permission::WorkflowEdit;

    fn uuid(number: u8) -> Uuid {
        Uuid::from_bytes([
            0x01, 0x92, 0xf3, 0xa4, 0x5b, 0x6c, 0x7d, 0x8e, 0x9f, 0x01, 0, 0, 0, 0, 0, number,
        ])
    }

    fn organisation(number: u8) -> OrganisationId {
        OrganisationId::try_from(uuid(number)).expect("an id")
    }

    fn project(number: u8) -> ProjectId {
        ProjectId::try_from(uuid(number)).expect("an id")
    }

    fn keys(permissions: &[Permission]) -> BTreeSet<Permission> {
        permissions.iter().copied().collect()
    }

    fn grants() -> Grants {
        Grants {
            organisations: BTreeMap::from([
                (organisation(1), keys(&[VIEW])),
                (organisation(2), keys(&[VIEW, DISPATCH])),
            ]),
            projects: BTreeMap::from([(
                project(11),
                ProjectGrant {
                    organisation_id: organisation(1),
                    permissions: keys(&[VIEW, EDIT]),
                },
            )]),
        }
    }

    fn carried(permissions: &[Permission], scopes: Option<Vec<Scope>>) -> Carried {
        Carried {
            permissions: Some(keys(permissions)),
            scopes,
        }
    }

    fn within(organisation_id: OrganisationId, project_id: Option<ProjectId>) -> Scope {
        Scope {
            organisation_id,
            project_id,
        }
    }

    #[test]
    fn a_session_and_a_token_without_keys_hold_what_the_bindings_give() {
        let expected = Permissions {
            installation: BTreeSet::new(),
            organisations: BTreeMap::from([
                (organisation(1), keys(&[VIEW])),
                (organisation(2), keys(&[VIEW, DISPATCH])),
            ]),
            projects: BTreeMap::from([(project(11), keys(&[VIEW, EDIT]))]),
        };
        assert_eq!(effective(BTreeSet::new(), grants(), None), expected);
        let everything = Carried {
            permissions: None,
            scopes: None,
        };
        assert_eq!(
            effective(BTreeSet::new(), grants(), Some(&everything)),
            expected
        );
    }

    #[test]
    fn a_token_holds_the_intersection_of_its_keys_and_the_bindings() {
        assert_eq!(
            effective(BTreeSet::new(), grants(), Some(&carried(&[EDIT], None))),
            Permissions {
                installation: BTreeSet::new(),
                organisations: BTreeMap::new(),
                projects: BTreeMap::from([(project(11), keys(&[EDIT]))]),
            }
        );
    }

    #[test]
    fn a_token_of_an_organisation_reaches_its_projects_and_nothing_else() {
        let token = carried(
            &[VIEW, EDIT, DISPATCH],
            Some(vec![within(organisation(1), None)]),
        );
        assert_eq!(
            effective(BTreeSet::new(), grants(), Some(&token)),
            Permissions {
                installation: BTreeSet::new(),
                organisations: BTreeMap::from([(organisation(1), keys(&[VIEW]))]),
                projects: BTreeMap::from([(project(11), keys(&[VIEW, EDIT]))]),
            }
        );
    }

    #[test]
    fn a_token_of_a_project_holds_there_what_its_organisation_gives() {
        let token = carried(
            &[VIEW, DISPATCH],
            Some(vec![within(organisation(2), Some(project(21)))]),
        );
        assert_eq!(
            effective(BTreeSet::new(), grants(), Some(&token)),
            Permissions {
                installation: BTreeSet::new(),
                organisations: BTreeMap::new(),
                projects: BTreeMap::from([(project(21), keys(&[VIEW, DISPATCH]))]),
            }
        );
    }

    #[test]
    fn a_restricted_token_of_an_owner_holds_its_keys_only_where_it_reaches() {
        let owner: BTreeSet<Permission> = Permission::ALL.into_iter().collect();
        let token = carried(
            &[Permission::RunCancel],
            Some(vec![within(organisation(2), Some(project(21)))]),
        );
        assert_eq!(
            effective(owner.clone(), Grants::default(), Some(&token)),
            Permissions {
                installation: BTreeSet::new(),
                organisations: BTreeMap::new(),
                projects: BTreeMap::from([(project(21), keys(&[Permission::RunCancel]))]),
            }
        );
        assert_eq!(
            effective(
                owner,
                Grants::default(),
                Some(&carried(&[Permission::AgentManage], None))
            )
            .installation,
            keys(&[Permission::AgentManage])
        );
        let nowhere = carried(&[VIEW], Some(Vec::new()));
        assert_eq!(
            effective(BTreeSet::new(), grants(), Some(&nowhere)),
            Permissions {
                installation: BTreeSet::new(),
                organisations: BTreeMap::new(),
                projects: BTreeMap::new(),
            }
        );
    }
}
