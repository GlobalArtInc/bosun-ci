use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "stream_event")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub seq: i64,
    pub xact: i64,
    pub subject: Uuid,
    pub project_id: Option<Uuid>,
    pub run_id: Option<Uuid>,
    pub kind: String,
    #[sea_orm(column_type = "JsonBinary")]
    pub data: Json,
    pub at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
