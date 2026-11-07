use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "triggered_run")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub source_run_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub attempt_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub workflow_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub transition: String,
    pub run_id: Option<Uuid>,
    pub skipped: Option<String>,
    pub depth: i32,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
