use sea_orm::entity::prelude::*;

#[derive(Clone, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "signing_key")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: Uuid,
    pub public_key: Vec<u8>,
    pub ciphertext: Option<Vec<u8>>,
    pub data_key: Option<Vec<u8>>,
    pub kek_id: Option<String>,
    pub created_at: DateTimeUtc,
    pub retired_at: Option<DateTimeUtc>,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Model")
            .field("id", &self.id)
            .field("kek_id", &self.kek_id)
            .field("created_at", &self.created_at)
            .field("retired_at", &self.retired_at)
            .finish_non_exhaustive()
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
