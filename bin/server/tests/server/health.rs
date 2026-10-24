use bosun_testkit::TestDatabase;

use crate::proxy::Proxy;
use crate::{Launch, eventually, get};

#[tokio::test]
async fn readyz_fails_while_the_database_is_unreachable_and_recovers_when_it_returns() {
    let database = TestDatabase::migrated().await;
    let proxy = Proxy::start(&database.url()).await;
    let mut server = Launch::serve(&proxy.url(&database.url())).spawn();
    let address = server.listening().await;
    assert_eq!(eventually(address, "/readyz", 200).await["status"], "ready");

    proxy.stall();
    let unready = eventually(address, "/readyz", 503).await;
    assert_eq!(unready["status"], "unready");
    assert_eq!(unready["reason"], "the database does not answer");
    assert_eq!(get(address, "/healthz").await.status(), 200);

    proxy.forward();
    assert_eq!(eventually(address, "/readyz", 200).await["status"], "ready");
}
