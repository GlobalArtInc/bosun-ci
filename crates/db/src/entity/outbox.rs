use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "outbox")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub kind: String,
    pub collapse_key: String,
    #[sea_orm(column_type = "JsonBinary")]
    pub payload: Json,
    pub created_at: DateTimeUtc,
    pub attempts: i32,
    pub next_attempt_at: DateTimeUtc,
    pub last_error: Option<String>,
    pub failing_since: Option<DateTimeUtc>,
    pub dead_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
