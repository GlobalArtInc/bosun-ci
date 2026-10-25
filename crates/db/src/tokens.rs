use std::collections::{BTreeMap, BTreeSet};

use bosun_types::api::{OrganisationRef, PersonalToken, ProjectRef, User};
use bosun_types::id::{OrganisationId, ProjectId, TokenId, UserId};
use bosun_types::permission::Permission;
use sea_orm::prelude::DateTimeUtc;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, TransactionTrait,
};
use uuid::Uuid;

use crate::entity::{api_token, api_token_scope, user};
use crate::row::statement;
use crate::{Error, roles, row, users};

const TABLE: &str = "api_token";

const SCOPE: &str = "api_token_scope";

pub struct NewToken {
    pub name: String,
    pub sha256: Vec<u8>,
    pub expires_at: DateTimeUtc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub organisation_id: OrganisationId,
    pub project_id: Option<ProjectId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Carried {
    pub permissions: Option<BTreeSet<Permission>>,
    pub scopes: Option<Vec<Scope>>,
}

pub struct Bearer {
    pub id: TokenId,
    pub user: User,
    pub password_change_required: bool,
    pub expires_at: DateTimeUtc,
    pub revoked_at: Option<DateTimeUtc>,
    pub carried: Carried,
}

pub async fn find_personal<C: ConnectionTrait>(
    db: &C,
    sha256: &[u8],
) -> Result<Option<Bearer>, Error> {
    let found = api_token::Entity::find()
        .filter(api_token::Column::Sha256.eq(sha256.to_vec()))
        .find_also_related(user::Entity)
        .one(db)
        .await?;
    let Some((token, Some(user))) = found else {
        return Ok(None);
    };
    let scopes = if token.restricted {
        let mut scopes = Vec::new();
        for scope in api_token_scope::Entity::find()
            .filter(api_token_scope::Column::TokenId.eq(token.id))
            .all(db)
            .await?
        {
            scopes.push(Scope {
                organisation_id: row::id(SCOPE, scope.organisation_id)?,
                project_id: row::optional_id(SCOPE, scope.project_id)?,
            });
        }
        Some(scopes)
    } else {
        None
    };
    Ok(Some(Bearer {
        id: row::id(TABLE, token.id)?,
        password_change_required: user.password_change_required,
        user: users::decode(user)?,
        expires_at: token.expires_at,
        revoked_at: token.revoked_at,
        carried: Carried {
            permissions: token.permissions.as_deref().map(roles::known),
            scopes,
        },
    }))
}

pub async fn create<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    token: NewToken,
) -> Result<TokenId, Error> {
    let id = insert(db, user.into(), token).await?;
    row::id(TABLE, id)
}

pub async fn create_personal<C: TransactionTrait>(
    db: &C,
    user: UserId,
    token: NewToken,
    permissions: &BTreeSet<Permission>,
    scopes: Option<&[Scope]>,
) -> Result<PersonalToken, Error> {
    let transaction = db.begin().await?;
    let id = Uuid::now_v7();
    let created = api_token::ActiveModel {
        id: ActiveValue::Set(id),
        user_id: ActiveValue::Set(user.into()),
        name: ActiveValue::Set(token.name),
        sha256: ActiveValue::Set(token.sha256),
        created_at: ActiveValue::NotSet,
        expires_at: ActiveValue::Set(token.expires_at),
        revoked_at: ActiveValue::Set(None),
        permissions: ActiveValue::Set(Some(roles::keys(permissions))),
        restricted: ActiveValue::Set(scopes.is_some()),
    }
    .insert(&transaction)
    .await?;
    for scope in scopes.unwrap_or_default() {
        transaction
            .execute(statement(
                "INSERT INTO api_token_scope (id, token_id, organisation_id, project_id) \
                 VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING",
                [
                    Uuid::now_v7().into(),
                    id.into(),
                    Uuid::from(scope.organisation_id).into(),
                    scope.project_id.map(Uuid::from).into(),
                ],
            ))
            .await?;
    }
    let personal = decode(&transaction, vec![created])
        .await?
        .pop()
        .ok_or_else(|| Error::Vanished(format!("the token {id}")))?;
    transaction.commit().await?;
    Ok(personal)
}

pub async fn list<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    after: Option<Uuid>,
    limit: u64,
) -> Result<Vec<PersonalToken>, Error> {
    let mut query = api_token::Entity::find()
        .filter(api_token::Column::UserId.eq(Uuid::from(user)))
        .order_by_asc(api_token::Column::Id)
        .limit(limit);
    if let Some(after) = after {
        query = query.filter(api_token::Column::Id.gt(after));
    }
    let tokens = query.all(db).await?;
    decode(db, tokens).await
}

pub async fn revoke<C: ConnectionTrait>(
    db: &C,
    user: UserId,
    id: TokenId,
    at: DateTimeUtc,
) -> Result<bool, Error> {
    let revoked = api_token::Entity::update_many()
        .col_expr(
            api_token::Column::RevokedAt,
            Expr::col(api_token::Column::RevokedAt).if_null(at),
        )
        .filter(api_token::Column::Id.eq(Uuid::from(id)))
        .filter(api_token::Column::UserId.eq(Uuid::from(user)))
        .exec(db)
        .await?;
    Ok(revoked.rows_affected > 0)
}

pub(crate) async fn insert<C: ConnectionTrait>(
    db: &C,
    user_id: Uuid,
    token: NewToken,
) -> Result<Uuid, Error> {
    let id = Uuid::now_v7();
    api_token::ActiveModel {
        id: ActiveValue::Set(id),
        user_id: ActiveValue::Set(user_id),
        name: ActiveValue::Set(token.name),
        sha256: ActiveValue::Set(token.sha256),
        created_at: ActiveValue::NotSet,
        expires_at: ActiveValue::Set(token.expires_at),
        revoked_at: ActiveValue::Set(None),
        permissions: ActiveValue::Set(None),
        restricted: ActiveValue::Set(false),
    }
    .insert(db)
    .await?;
    Ok(id)
}

async fn decode<C: ConnectionTrait>(
    db: &C,
    tokens: Vec<api_token::Model>,
) -> Result<Vec<PersonalToken>, Error> {
    let ids: Vec<Uuid> = tokens.iter().map(|token| token.id).collect();
    let mut reach: BTreeMap<Uuid, (Vec<OrganisationRef>, Vec<ProjectRef>)> = BTreeMap::new();
    if !ids.is_empty() {
        let entries = db
            .query_all(statement(
                "SELECT s.token_id, s.organisation_id, s.project_id, o.slug AS organisation, \
                 p.slug AS project FROM api_token_scope s \
                 JOIN organisation o ON o.id = s.organisation_id \
                 LEFT JOIN project p ON p.id = s.project_id \
                 WHERE s.token_id = ANY ($1) ORDER BY o.slug, p.slug",
                [ids.into()],
            ))
            .await?;
        for entry in entries {
            let token: Uuid = entry.try_get("", "token_id")?;
            let (organisations, projects) = reach.entry(token).or_default();
            let organisation: Uuid = entry.try_get("", "organisation_id")?;
            let slug: String = entry.try_get("", "organisation")?;
            match entry.try_get::<Option<Uuid>>("", "project_id")? {
                None => organisations.push(OrganisationRef {
                    id: row::id(SCOPE, organisation)?,
                    slug,
                }),
                Some(project) => {
                    let name: String = entry.try_get("", "project")?;
                    projects.push(ProjectRef {
                        id: row::id(SCOPE, project)?,
                        organisation_id: row::id(SCOPE, organisation)?,
                        path: format!("{slug}/{name}"),
                    });
                }
            }
        }
    }
    tokens
        .into_iter()
        .map(|token| {
            let (organisations, projects) = reach.remove(&token.id).unwrap_or_default();
            Ok(PersonalToken {
                id: row::id(TABLE, token.id)?,
                name: token.name,
                permissions: token
                    .permissions
                    .as_deref()
                    .map_or_else(|| Permission::ALL.into_iter().collect(), roles::known),
                restricted: token.restricted,
                organisations,
                projects,
                created_at: token.created_at,
                expires_at: token.expires_at,
                revoked_at: token.revoked_at,
            })
        })
        .collect()
}
