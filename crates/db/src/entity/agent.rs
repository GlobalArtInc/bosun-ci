use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "agent")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub pool_id: Uuid,
    pub name: String,
    pub credential_sha256: Vec<u8>,
    pub labels: Vec<String>,
    pub capabilities: Vec<String>,
    pub version: Option<String>,
    pub online: bool,
    pub last_seen_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub replica_id: Option<Uuid>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
