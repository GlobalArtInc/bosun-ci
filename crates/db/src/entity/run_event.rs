use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "run_event")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub seq: i64,
    pub run_id: Uuid,
    pub attempt_id: Option<Uuid>,
    pub job_id: Option<Uuid>,
    pub execution_id: Option<Uuid>,
    pub step_id: Option<Uuid>,
    pub kind: String,
    #[sea_orm(column_type = "JsonBinary")]
    pub actor: Json,
    #[sea_orm(column_type = "JsonBinary")]
    pub data: Json,
    pub at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
