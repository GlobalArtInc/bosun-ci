use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "execution")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub job_id: Uuid,
    pub number: i32,
    pub agent_id: Uuid,
    pub lease_until: Option<DateTimeUtc>,
    pub status: String,
    pub conclusion: Option<String>,
    pub reason: Option<String>,
    pub started_at: Option<DateTimeUtc>,
    pub completed_at: Option<DateTimeUtc>,
    pub log_bytes: i64,
    pub log_compacted_at: Option<DateTimeUtc>,
    pub stages: Option<Json>,
    pub logs_expired_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
