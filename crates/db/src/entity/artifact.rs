use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "artifact")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i64,
    pub project_id: Uuid,
    pub attempt_id: Uuid,
    pub execution_id: Uuid,
    pub name: String,
    pub key: String,
    pub content_type: String,
    pub created_at: DateTimeUtc,
    pub expires_at: DateTimeUtc,
    pub finalized_at: Option<DateTimeUtc>,
    pub size: Option<i64>,
    pub sha256: Option<Vec<u8>>,
    pub blocks: i32,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
