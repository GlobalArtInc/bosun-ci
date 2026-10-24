use axum::{Extension, Json};
use bosun_types::api::Me;

use crate::auth::Principal;

pub(crate) async fn show(Extension(principal): Extension<Principal>) -> Json<Me> {
    Json(Me {
        user: principal.user,
    })
}
