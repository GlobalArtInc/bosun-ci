use axum::{Extension, Json};
use bosun_types::api::Me;

use crate::auth::Principal;

pub(crate) async fn show(Extension(principal): Extension<Principal>) -> Json<Me> {
    Json(Me {
        csrf_token: principal.csrf_token(),
        password_change_required: principal.password_change_required,
        user: principal.user,
    })
}
