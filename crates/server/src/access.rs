use std::collections::BTreeSet;
use std::marker::PhantomData;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use bosun_db::projects::{Holder, Reach};
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
        let permissions = self.permissions();
        Reach {
            everywhere: permissions.installation.contains(&K::KEY),
            organisations: permissions
                .organisations
                .iter()
                .filter(|(_, held)| held.contains(&K::KEY))
                .map(|(organisation, _)| *organisation)
                .collect(),
            projects: permissions
                .projects
                .iter()
                .filter(|(_, held)| held.contains(&K::KEY))
                .map(|(project, _)| *project)
                .collect(),
        }
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
) -> Result<Permissions, bosun_db::Error> {
    if user.owner {
        return Ok(Permissions::everything());
    }
    bosun_db::roles::permissions(db, user.id).await
}
