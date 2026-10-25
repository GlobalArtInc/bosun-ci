use axum::Json;
use axum::extract::State;
use bosun_db::users::NewUser;
use bosun_types::api::{CreateUserRequest, Page, User};
use bosun_types::error::ErrorCode;
use bosun_types::id::UserId;
use sea_orm::{ConnectionTrait, TransactionTrait};
use tracing::info;

use crate::access::{Caller, OnInstallation, OrganisationManage};
use crate::envelope::ApiError;
use crate::openapi::Api;
use crate::reply::{Created, Removed};
use crate::request::{Faults, Idempotent, Listing, Segment, database, failed};
use crate::{AppState, idempotency, owner, password, slug};

const EMAIL_LENGTH: usize = 254;

pub(crate) fn routes() -> Api {
    Api::new()
        .get("/api/v1/users", list)
        .post("/api/v1/users", create)
        .delete("/api/v1/users/{user}", delete)
}

pub(crate) async fn find<C: ConnectionTrait>(
    db: &C,
    reference: &str,
) -> Result<Option<User>, ApiError> {
    match reference.parse::<UserId>() {
        Ok(id) => bosun_db::users::find(db, id).await,
        Err(_) => bosun_db::users::find_by_login(db, reference).await,
    }
    .map_err(database)
}

pub(crate) fn missing(reference: &str) -> String {
    format!("there is no user `{reference}`")
}

async fn list(
    State(state): State<AppState>,
    caller: Caller<OnInstallation<OrganisationManage>>,
    listing: Result<Listing, ApiError>,
) -> Result<Json<Page<User>>, ApiError> {
    caller.installation()?;
    let listing = listing?;
    let users = bosun_db::users::list(&state.db, listing.cursor(), listing.fetch())
        .await
        .map_err(database)?;
    Ok(Json(listing.page(users, |user| user.id.as_uuid())))
}

async fn create(
    State(state): State<AppState>,
    caller: Caller<OnInstallation<OrganisationManage>>,
    body: Result<Idempotent<CreateUserRequest>, ApiError>,
) -> Result<Created<User>, ApiError> {
    caller.installation()?;
    let body = body?;
    let request = &body.value;
    let mut faults = Faults::default();
    if !owner::is_login(&request.login) {
        faults.add(
            "login",
            format!("`{}` is not a login: {}", request.login, owner::LOGIN_RULE),
        );
    }
    slug::check_name(&mut faults, "name", &request.name);
    if !is_email(&request.email) {
        faults.add(
            "email",
            format!(
                "`{}` is not an email address: it is up to {EMAIL_LENGTH} characters without \
                 white space or control characters, with one `@` between a local part and a \
                 domain",
                request.email
            ),
        );
    }
    if !password::acceptable(&request.password) {
        faults.add("password", password::RULE);
    }
    faults.finish("the user is not valid")?;

    let password_hash = state
        .sign_in
        .hasher
        .hash(request.password.clone())
        .await
        .map_err(failed)?;
    let transaction = state.db.begin().await.map_err(database)?;
    if let Some(replayed) = idempotency::replay(&transaction, body.idempotency.as_ref()).await? {
        return Ok(replayed);
    }
    let user = bosun_db::users::create_local(
        &transaction,
        NewUser {
            login: &request.login,
            name: &request.name,
            email: &request.email,
            password_hash,
        },
    )
    .await
    .map_err(database)?;
    let (id, login) = (user.id, user.login.clone());
    let created =
        idempotency::commit(&state.db, transaction, body.idempotency.as_ref(), user).await?;
    info!(user_id = %id, %login, by = %caller.principal.user.login, "created a user");
    Ok(created)
}

async fn delete(
    State(state): State<AppState>,
    caller: Caller<OnInstallation<OrganisationManage>>,
    Segment(reference): Segment,
) -> Result<Removed, ApiError> {
    caller.installation()?;
    let absent = || ApiError::new(ErrorCode::NotFound, missing(&reference));
    let id: UserId = reference.parse().map_err(|_| absent())?;
    let caller = &caller.principal.user;
    if id == caller.id {
        return Err(ApiError::new(
            ErrorCode::InvalidState,
            format!(
                "`{}` cannot delete themselves, so that an installation always keeps an owner; \
                 another owner can",
                caller.login
            ),
        ));
    }
    if !bosun_db::users::delete(&state.db, id)
        .await
        .map_err(database)?
    {
        return Err(absent());
    }
    info!(user_id = %id, by = %caller.login, "deleted a user");
    Ok(Removed)
}

fn is_email(text: &str) -> bool {
    let Some((local, domain)) = text.split_once('@') else {
        return false;
    };
    text.chars().count() <= EMAIL_LENGTH
        && !local.is_empty()
        && !domain.is_empty()
        && !domain.contains('@')
        && !text
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
}

#[cfg(test)]
mod tests {
    use super::is_email;

    #[test]
    fn an_email_address_has_one_at_between_two_parts_and_no_white_space() {
        for address in ["dana@example.org", "a@b", "first.last+ci@mail.example.org"] {
            assert!(is_email(address), "`{address}` was refused");
        }
        for text in [
            "",
            "dana",
            "@example.org",
            "dana@",
            "dana@@example.org",
            "da na@example.org",
            "dana@example.org\n",
            &format!("{}@example.org", "a".repeat(250)),
        ] {
            assert!(!is_email(text), "`{text}` was accepted");
        }
    }
}
