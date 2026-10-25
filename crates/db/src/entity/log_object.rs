use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "log_object")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub step_id: Uuid,
    pub execution_id: Uuid,
    pub key: String,
    pub size: i64,
    pub lines: i64,
    pub created_at: DateTimeUtc,
    pub frames: Vec<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
