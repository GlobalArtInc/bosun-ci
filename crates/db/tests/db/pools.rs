use bosun_db::entity::pool;
use bosun_testkit::TestDatabase;
use bosun_types::api::TrustLevel;
use sea_orm::{EntityTrait, PaginatorTrait};

#[tokio::test]
async fn the_trusted_pool_default_is_seeded_once() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    let default = bosun_db::pools::default_pool(db)
        .await
        .expect("the default pool");
    assert_eq!(default.name, "default");
    assert_eq!(default.trust, TrustLevel::Trusted);
    assert_eq!(pool::Entity::find().count(db).await.expect("a count"), 1);
}
