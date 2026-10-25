use std::net::SocketAddr;

use bosun_testkit::TestDatabase;

use crate::proxy::Proxy;
use crate::{Finished, Launch, errors, eventually};

async fn check(database: &str, listen: &str) -> Finished {
    Launch::check(database)
        .env("BOSUN_SERVER_LISTEN", listen)
        .finish()
        .await
}

fn refused(finished: &Finished, expected: &str) {
    assert!(!finished.status.success(), "{finished:#?}");
    assert_eq!(finished.stdout, "", "{finished:#?}");
    assert!(
        errors(&finished.logs)
            .iter()
            .any(|error| error.starts_with(expected)),
        "expected an error starting with `{expected}`: {finished:#?}"
    );
}

#[tokio::test]
async fn check_passes_while_the_server_is_ready_and_says_why_it_fails_otherwise() {
    let database = TestDatabase::migrated().await;
    let proxy = Proxy::start(&database.url()).await;
    let url = proxy.url(&database.url());
    let mut server = Launch::serve(&url).spawn();
    let address = server.listening().await;
    eventually(address, "/readyz", 200).await;

    let finished = check(&url, &address.to_string()).await;
    assert!(finished.status.success(), "{finished:#?}");
    assert_eq!(
        finished.stdout,
        format!("the server at {address} is ready\n")
    );
    let everywhere = format!("0.0.0.0:{}", address.port());
    let finished = check(&url, &everywhere).await;
    assert!(finished.status.success(), "{finished:#?}");
    assert_eq!(
        finished.stdout,
        format!("the server at {address} is ready\n")
    );

    proxy.stall();
    eventually(address, "/readyz", 503).await;
    refused(
        &check(&url, &address.to_string()).await,
        &format!("the server at {address} is not ready: the database does not answer"),
    );
    proxy.forward();
    eventually(address, "/readyz", 200).await;

    server.terminate();
    let (status, lines) = server.exit().await;
    assert!(status.success(), "{lines:#?}");
    refused(
        &check(&url, &address.to_string()).await,
        &format!("the server at {address} does not answer: "),
    );
}

#[tokio::test]
async fn check_needs_the_port_of_the_server() {
    let any: SocketAddr = "127.0.0.1:0".parse().expect("an address");
    refused(
        &check("postgres://bosun@127.0.0.1:1/bosun", &any.to_string()).await,
        "server.listen names port 0, so there is no port at which to check the server",
    );
}
