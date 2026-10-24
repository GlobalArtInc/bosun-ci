use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "idempotency_key")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub principal: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    pub request_sha256: Vec<u8>,
    pub status: i32,
    #[sea_orm(column_type = "JsonBinary")]
    pub response: Json,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
