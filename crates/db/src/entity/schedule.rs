use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "schedule")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub workflow_id: Uuid,
    pub revision_id: Uuid,
    pub expression: String,
    pub timezone: String,
    #[sea_orm(column_type = "JsonBinary")]
    pub cron: Json,
    pub git_ref: Option<String>,
    pub sha: Option<String>,
    #[sea_orm(column_type = "JsonBinary")]
    pub actor: Json,
    pub next_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
