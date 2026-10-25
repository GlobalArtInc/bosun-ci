use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "run")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub project_id: Uuid,
    pub workflow_id: Uuid,
    pub revision_id: Uuid,
    #[sea_orm(column_type = "JsonBinary")]
    pub ir: Json,
    pub number: i64,
    pub global_number: i64,
    pub display_name: Option<String>,
    pub event: String,
    #[sea_orm(column_type = "JsonBinary")]
    pub trigger: Json,
    #[sea_orm(column_name = "ref")]
    pub git_ref: Option<String>,
    pub sha: Option<String>,
    #[sea_orm(column_type = "JsonBinary")]
    pub inputs: Json,
    pub status: String,
    pub conclusion: Option<String>,
    pub created_at: DateTimeUtc,
    pub started_at: Option<DateTimeUtc>,
    pub completed_at: Option<DateTimeUtc>,
    pub cancel_requested_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
