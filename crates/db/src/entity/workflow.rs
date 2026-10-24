use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "workflow")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub source_kind: String,
    pub enabled: bool,
    pub revision_id: Option<Uuid>,
    pub created_at: DateTimeUtc,
    pub last_run_number: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
