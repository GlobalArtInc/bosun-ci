use std::net::SocketAddr;
use std::time::Duration;

use bosun_proto::agent::v1::agent_client::AgentClient;
use bosun_proto::agent::v1::agent_message::Message as Outgoing;
use bosun_proto::agent::v1::server_message::Message as Incoming;
use bosun_proto::agent::v1::{
    AgentMessage, Assignment, Capability, Control, ControlAck, Heartbeat, Hello, Idle, LogAck,
    PROTOCOL, ServerMessage, Welcome,
};
use bosun_types::api::{RegisterAgentRequest, RegisterAgentResponse};
use bosun_types::id::AgentId;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tonic::metadata::MetadataValue;
use tonic::transport::Endpoint;
use tonic::{Request, Status, Streaming};

const WAIT: Duration = Duration::from_secs(60);

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone)]
pub struct FakeAgent {
    id: AgentId,
    name: String,
    labels: Vec<String>,
    credential: String,
}

pub struct Connection {
    outbound: UnboundedSender<AgentMessage>,
    inbound: Streaming<ServerMessage>,
    welcome: Welcome,
    heartbeats: Option<JoinHandle<()>>,
}

impl FakeAgent {
    pub async fn register(
        server: SocketAddr,
        token: &str,
        name: &str,
        labels: &[&str],
    ) -> FakeAgent {
        let labels: Vec<String> = labels.iter().map(|label| (*label).to_owned()).collect();
        let response = reqwest::Client::builder()
            .no_proxy()
            .timeout(WAIT)
            .build()
            .unwrap_or_else(|error| panic!("the fake agent has no HTTP client: {error}"))
            .post(format!("http://{server}/api/v1/agents/register"))
            .json(&RegisterAgentRequest {
                token: token.to_owned(),
                name: name.to_owned(),
                labels: labels.clone(),
            })
            .send()
            .await
            .unwrap_or_else(|error| panic!("the fake agent could not register: {error}"));
        let status = response.status();
        let text = response
            .text()
            .await
            .unwrap_or_else(|error| panic!("the registration has no body: {error}"));
        assert_eq!(status, 201, "the registration was refused: {text}");
        let registered: RegisterAgentResponse = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("the registration answered `{text}`: {error}"));
        FakeAgent {
            id: registered.agent_id,
            name: name.to_owned(),
            labels,
            credential: registered.credential,
        }
    }

    pub fn id(&self) -> AgentId {
        self.id
    }

    pub fn credential(&self) -> &str {
        &self.credential
    }

    pub fn with_credential(&self, credential: &str) -> FakeAgent {
        FakeAgent {
            credential: credential.to_owned(),
            ..self.clone()
        }
    }

    pub fn hello(&self) -> Hello {
        Hello {
            version: VERSION.to_owned(),
            protocol: PROTOCOL.to_owned(),
            capabilities: vec![Capability::Shell.into()],
            labels: self.labels.clone(),
            os: "Linux".to_owned(),
            arch: "X64".to_owned(),
            hostname: format!("{}.example.org", self.name),
            executions: Vec::new(),
        }
    }

    pub async fn connect(&self, server: SocketAddr) -> Connection {
        self.connect_with(server, self.hello())
            .await
            .unwrap_or_else(|status| panic!("the server refused the fake agent: {status:?}"))
    }

    pub async fn connect_with(
        &self,
        server: SocketAddr,
        hello: Hello,
    ) -> Result<Connection, Status> {
        let channel = Endpoint::from_shared(format!("http://{server}"))
            .unwrap_or_else(|error| panic!("{server} is not an address: {error}"))
            .connect()
            .await
            .unwrap_or_else(|error| panic!("the fake agent could not reach {server}: {error}"));
        let (outbound, receiver) = mpsc::unbounded_channel();
        let _ = outbound.send(AgentMessage {
            message: Some(Outgoing::Hello(hello)),
        });
        let mut request = Request::new(UnboundedReceiverStream::new(receiver));
        let bearer = MetadataValue::try_from(format!("Bearer {}", self.credential))
            .unwrap_or_else(|error| panic!("the credential is not metadata: {error}"));
        request.metadata_mut().insert("authorization", bearer);
        let mut inbound = timeout(WAIT, AgentClient::new(channel).connect(request))
            .await
            .unwrap_or_else(|_| panic!("the server did not answer the fake agent within {WAIT:?}"))?
            .into_inner();
        let welcome = match next(&mut inbound).await? {
            Some(Incoming::Welcome(welcome)) => welcome,
            other => panic!("the first message of the server is not a Welcome: {other:?}"),
        };
        Ok(Connection {
            outbound,
            inbound,
            welcome,
            heartbeats: None,
        })
    }
}

impl Connection {
    pub fn welcome(&self) -> &Welcome {
        &self.welcome
    }

    pub fn send(&self, message: Outgoing) {
        let _ = self.outbound.send(AgentMessage {
            message: Some(message),
        });
    }

    pub fn beat_every(&mut self, interval: Duration) {
        let outbound = self.outbound.clone();
        self.stop_beating();
        self.heartbeats = Some(tokio::spawn(async move {
            let mut ticks = tokio::time::interval(interval);
            loop {
                ticks.tick().await;
                let beat = AgentMessage {
                    message: Some(Outgoing::Heartbeat(Heartbeat::default())),
                };
                if outbound.send(beat).is_err() {
                    break;
                }
            }
        }));
    }

    pub fn stop_beating(&mut self) {
        if let Some(heartbeats) = self.heartbeats.take() {
            heartbeats.abort();
        }
    }

    pub async fn message(&mut self) -> Result<Option<Incoming>, Status> {
        next(&mut self.inbound).await
    }

    pub fn idle(&self) {
        self.send(Outgoing::Idle(Idle {}));
    }

    pub async fn assignment(&mut self) -> Assignment {
        match self.message().await {
            Ok(Some(Incoming::Assignment(assignment))) => assignment,
            other => panic!("the server sent no assignment but {other:?}"),
        }
    }

    pub async fn control(&mut self) -> Control {
        match self.message().await {
            Ok(Some(Incoming::Control(control))) => control,
            other => panic!("the server sent no control message but {other:?}"),
        }
    }

    pub async fn log_ack(&mut self) -> LogAck {
        match self.message().await {
            Ok(Some(Incoming::LogAck(ack))) => ack,
            other => panic!("the server sent no log acknowledgement but {other:?}"),
        }
    }

    pub fn acknowledge(&self, control: &Control, took_effect: bool) {
        self.send(Outgoing::ControlAck(ControlAck {
            id: control.id.clone(),
            took_effect,
        }));
    }

    pub async fn closed(mut self) -> Status {
        loop {
            match self.message().await {
                Ok(Some(_)) => {}
                Ok(None) => return Status::ok("the server ended the stream"),
                Err(status) => return status,
            }
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.stop_beating();
    }
}

async fn next(inbound: &mut Streaming<ServerMessage>) -> Result<Option<Incoming>, Status> {
    loop {
        let message = timeout(WAIT, inbound.message())
            .await
            .unwrap_or_else(|_| panic!("the server sent nothing to the fake agent for {WAIT:?}"))?;
        match message {
            Some(ServerMessage {
                message: Some(message),
            }) => return Ok(Some(message)),
            Some(ServerMessage { message: None }) => {}
            None => return Ok(None),
        }
    }
}
