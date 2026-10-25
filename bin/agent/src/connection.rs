use std::error::Error;
use std::time::Duration;

use bosun_proto::agent::v1::agent_client::AgentClient;
use bosun_proto::agent::v1::agent_message::Message;
use bosun_proto::agent::v1::server_message::Message as Incoming;
use bosun_proto::agent::v1::{AgentMessage, Hello, ServerMessage, Welcome};
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_stream::wrappers::ReceiverStream;
use tonic::metadata::MetadataValue;
use tonic::transport::{ClientTlsConfig, Endpoint};
use tonic::{Request, Status, Streaming};

use crate::config::Config;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);

const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(20);

const OUTBOUND: usize = 64;

pub(crate) struct Connection {
    outbound: mpsc::Sender<AgentMessage>,
    inbound: Streaming<ServerMessage>,
    pub(crate) welcome: Welcome,
}

fn chain(error: &dyn Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn describe(status: &Status) -> String {
    format!("{:?}: {}", status.code(), status.message())
}

impl Connection {
    pub(crate) async fn open(config: &Config, hello: Hello) -> Result<Connection, String> {
        let mut endpoint = Endpoint::from_shared(config.url.clone())
            .map_err(|error| format!("`{}` is not an address: {error}", config.url))?
            .connect_timeout(CONNECT_TIMEOUT)
            .http2_keep_alive_interval(KEEPALIVE_INTERVAL)
            .keep_alive_timeout(KEEPALIVE_TIMEOUT)
            .keep_alive_while_idle(true)
            .tcp_nodelay(true);
        if config.url.starts_with("https:") {
            endpoint = endpoint
                .tls_config(ClientTlsConfig::new().with_webpki_roots())
                .map_err(|error| format!("TLS cannot be set up for {}: {error}", config.url))?;
        }
        let channel = endpoint
            .connect()
            .await
            .map_err(|error| format!("{} could not be reached: {}", config.url, chain(&error)))?;
        let (outbound, receiver) = mpsc::channel(OUTBOUND);
        outbound
            .send(AgentMessage {
                message: Some(Message::Hello(hello)),
            })
            .await
            .map_err(|_| "the stream closed before the Hello".to_owned())?;
        let mut request = Request::new(ReceiverStream::new(receiver));
        let bearer = MetadataValue::try_from(format!("Bearer {}", config.credential))
            .map_err(|_| "the credential cannot travel as metadata".to_owned())?;
        request.metadata_mut().insert("authorization", bearer);
        let response = timeout(ANSWER_TIMEOUT, AgentClient::new(channel).connect(request))
            .await
            .map_err(|_| format!("the server did not answer within {ANSWER_TIMEOUT:?}"))?
            .map_err(|status| format!("the server refused the stream: {}", describe(&status)))?;
        let mut inbound = response.into_inner();
        let first = timeout(ANSWER_TIMEOUT, inbound.message())
            .await
            .map_err(|_| format!("the server sent no Welcome within {ANSWER_TIMEOUT:?}"))?
            .map_err(|status| format!("the server ended the stream: {}", describe(&status)))?;
        match first {
            Some(ServerMessage {
                message: Some(Incoming::Welcome(welcome)),
            }) => Ok(Connection {
                outbound,
                inbound,
                welcome,
            }),
            Some(_) => Err("the first message of the server is not a Welcome".to_owned()),
            None => Err("the server ended the stream before its Welcome".to_owned()),
        }
    }

    pub(crate) async fn send(&self, message: Message) -> Result<(), String> {
        self.outbound
            .send(AgentMessage {
                message: Some(message),
            })
            .await
            .map_err(|_| "the stream to the server closed".to_owned())
    }

    pub(crate) async fn receive(&mut self) -> Result<Option<Incoming>, String> {
        loop {
            match self.inbound.message().await {
                Ok(Some(ServerMessage {
                    message: Some(message),
                })) => return Ok(Some(message)),
                Ok(Some(ServerMessage { message: None })) => {}
                Ok(None) => return Ok(None),
                Err(status) => {
                    return Err(format!("the stream failed: {}", describe(&status)));
                }
            }
        }
    }

    pub(crate) async fn close(self, wait: Duration) {
        let Connection {
            outbound,
            mut inbound,
            ..
        } = self;
        drop(outbound);
        let _ = timeout(wait, async {
            while let Ok(Some(_)) = inbound.message().await {}
        })
        .await;
    }
}
