use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "forge_check_run")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub job_id: Uuid,
    pub id: i64,
    pub created_at: DateTimeUtc,
    pub annotations: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
