use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "annotation")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub step_id: Uuid,
    #[sea_orm(primary_key, auto_increment = false)]
    pub ordinal: i32,
    pub kind: String,
    pub message: String,
    pub title: Option<String>,
    pub file: Option<String>,
    pub line: Option<i64>,
    pub end_line: Option<i64>,
    pub start_column: Option<i64>,
    pub end_column: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
