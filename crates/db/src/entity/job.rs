use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "job")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub key: String,
    pub name: String,
    pub group_id: Option<Uuid>,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub matrix: Option<Json>,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub strategy: Option<Json>,
    pub needs: Vec<String>,
    pub status: String,
    pub waiting_reason: Option<String>,
    pub conclusion: Option<String>,
    pub conclusion_source: Option<String>,
    pub continue_on_error: bool,
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub evaluated: Option<Json>,
    #[sea_orm(column_type = "JsonBinary")]
    pub outputs: Json,
    pub priority: i32,
    pub labels: Vec<String>,
    pub pool_ids: Vec<Uuid>,
    pub queued_at: Option<DateTimeUtc>,
    pub started_at: Option<DateTimeUtc>,
    pub completed_at: Option<DateTimeUtc>,
    pub capabilities: Vec<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
