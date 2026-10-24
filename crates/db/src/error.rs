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
    #[error("a user with the login `{0}` already exists")]
    LoginTaken(String),
    #[error("could not hold the migration lock: {0}")]
    Lock(#[source] DbErr),
    #[error(
        "the schema belongs to major version {found} of Bosun and this server is major version \
         {supported}, which never runs against a schema of a newer major version"
    )]
    NewerSchema { found: i32, supported: i32 },
}
