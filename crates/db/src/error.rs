use sea_orm::DbErr;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Database(#[from] DbErr),
    #[error(transparent)]
    Migration(#[from] sqlmig::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("a row of {table} holds a value bosun-types does not accept: {message}")]
    Row {
        table: &'static str,
        message: String,
    },
    #[error("{0} does not exist; the migrations create it")]
    Missing(&'static str),
}
