use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "artifact_block")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub artifact_id: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub block: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
