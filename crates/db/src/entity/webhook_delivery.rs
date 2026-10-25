use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "webhook_delivery")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub delivery: String,
    pub event: String,
    #[sea_orm(column_type = "JsonBinary")]
    pub payload: Json,
    pub received_at: DateTimeUtc,
    pub attempts: i32,
    pub next_attempt_at: DateTimeUtc,
    pub last_error: Option<String>,
    pub processed_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
