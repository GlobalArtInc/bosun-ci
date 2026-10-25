use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "concurrency_member")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub project_id: Uuid,
    pub key: String,
    pub run_id: Uuid,
    pub job_id: Option<Uuid>,
    pub running: bool,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
