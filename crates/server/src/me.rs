use axum::Json;
use bosun_types::api::Me;

use crate::access::{Authenticated, Caller};

pub(crate) async fn show(caller: Caller<Authenticated>) -> Json<Me> {
    let principal = caller.principal;
    Json(Me {
        csrf_token: principal.csrf_token(),
        password_change_required: principal.password_change_required,
        user: principal.user,
        permissions: principal.permissions,
    })
}
