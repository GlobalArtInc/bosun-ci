use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "log_chunk")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub execution_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub seq: i64,
    pub step_id: Uuid,
    pub data: Vec<u8>,
    pub dropped: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
