use std::time::Duration;

use bosun_testkit::TestDatabase;
use tokio::net::TcpStream;
use tokio::time::{Instant, sleep};

use crate::proxy::Proxy;
use crate::{Launch, WAIT, eventually, get, message};

#[tokio::test]
async fn a_terminated_server_stops_accepting_drains_its_requests_and_exits_cleanly() {
    let database = TestDatabase::migrated().await;
    let proxy = Proxy::start(&database.url()).await;
    let mut server = Launch::serve(&proxy.url(&database.url())).spawn();
    let address = server.listening().await;
    eventually(address, "/readyz", 200).await;

    proxy.stall();
    let in_flight = tokio::spawn(async move { get(address, "/readyz").await.status() });
    sleep(Duration::from_millis(300)).await;
    server.terminate();
    server
        .expect("that it shuts down", |line| {
            message(line) == "shutting down"
        })
        .await;

    let deadline = Instant::now() + WAIT;
    while TcpStream::connect(address).await.is_ok() {
        assert!(Instant::now() < deadline, "the server still accepts");
        sleep(Duration::from_millis(20)).await;
    }
    assert!(
        !in_flight.is_finished(),
        "the request ended before the drain"
    );
    assert_eq!(in_flight.await.expect("the request in flight"), 503);

    let (status, lines) = server.exit().await;
    assert!(status.success(), "{lines:#?}");
    let shutting_down = lines
        .iter()
        .find(|line| message(line) == "shutting down")
        .expect("a line that it shuts down");
    assert_eq!(shutting_down["reason"], "received SIGTERM");
    assert_eq!(
        lines.last().map(message),
        Some("stopped"),
        "the last line is not that it stopped: {lines:#?}"
    );
}
