use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "project")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub organisation_id: Uuid,
    pub slug: String,
    pub name: String,
    pub created_at: DateTimeUtc,
    pub github_workflows: bool,
    pub fork_policy: String,
    #[sea_orm(column_type = "JsonBinary")]
    pub token_permissions: Json,
    pub report: String,
    pub settings_version: i64,
    pub repository_id: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
