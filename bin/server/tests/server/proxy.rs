use std::net::SocketAddr;

use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use url::Url;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Forward,
    Stall,
}

pub struct Proxy {
    address: SocketAddr,
    mode: watch::Sender<Mode>,
    accept: JoinHandle<()>,
}

impl Proxy {
    pub async fn start(database: &str) -> Proxy {
        let url = Url::parse(database).expect("a database URL");
        let upstream = format!(
            "{}:{}",
            url.host_str().expect("a database host"),
            url.port().unwrap_or(5432)
        );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port for the proxy");
        let address = listener.local_addr().expect("the address of the proxy");
        let (mode, watching) = watch::channel(Mode::Forward);
        let accept = tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                tokio::spawn(connection(client, upstream.clone(), watching.clone()));
            }
        });
        Proxy {
            address,
            mode,
            accept,
        }
    }

    pub fn url(&self, database: &str) -> String {
        let mut url = Url::parse(database).expect("a database URL");
        url.set_host(Some("127.0.0.1")).expect("a host");
        url.set_port(Some(self.address.port())).expect("a port");
        url.to_string()
    }

    pub fn stall(&self) {
        self.mode.send_replace(Mode::Stall);
    }

    pub fn forward(&self) {
        self.mode.send_replace(Mode::Forward);
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.accept.abort();
    }
}

async fn connection(mut client: TcpStream, upstream: String, mut mode: watch::Receiver<Mode>) {
    if *mode.borrow() == Mode::Stall {
        let _ = mode.wait_for(|mode| *mode == Mode::Forward).await;
        return;
    }
    let Ok(mut server) = TcpStream::connect(&upstream).await else {
        return;
    };
    let stalled = tokio::select! {
        _ = copy_bidirectional(&mut client, &mut server) => false,
        stalled = mode.wait_for(|mode| *mode == Mode::Stall) => stalled.is_ok(),
    };
    if stalled {
        let _ = mode.wait_for(|mode| *mode == Mode::Forward).await;
    }
}
