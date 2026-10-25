use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response as HttpResponse};
use axum::routing::post;
use bosun_proto::agent::v1::agent_message::Message as Inbound;
use bosun_proto::agent::v1::agent_server::{Agent, AgentServer};
use bosun_proto::agent::v1::server_message::Message as Outgoing;
use bosun_proto::agent::v1::{
    AgentMessage, Control, ControlAck, ControlKind, Hello, Log, LogAck, ServerMessage, Settings,
    Welcome,
};
use bosun_types::api::{RegisterAgentRequest, RegisterAgentResponse};
use bosun_types::error::{ErrorBody, ErrorCode, ErrorEnvelope};
use bosun_types::id::AgentId;
use tokio::net::TcpListener;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tonic::service::Routes;
use tonic::{Request, Response, Status, Streaming};
use uuid::Uuid;

use crate::WAIT;

pub const TOKEN: &str = "brt_fake";

pub const CREDENTIAL: &str = "bat_fake";

pub const HEARTBEAT: Duration = Duration::from_secs(1);

pub struct FakeServer {
    address: SocketAddr,
    agent_id: AgentId,
    streams: UnboundedReceiver<Stream>,
    registrations: UnboundedReceiver<RegisterAgentRequest>,
    task: JoinHandle<()>,
}

#[derive(Clone)]
struct Fake {
    streams: UnboundedSender<Stream>,
}

#[tonic::async_trait]
impl Agent for Fake {
    type ConnectStream = UnboundedReceiverStream<Result<ServerMessage, Status>>;

    async fn connect(
        &self,
        request: Request<Streaming<AgentMessage>>,
    ) -> Result<Response<Self::ConnectStream>, Status> {
        let bearer = request
            .metadata()
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        if bearer.as_deref() != Some(&format!("Bearer {CREDENTIAL}")) {
            return Err(Status::unauthenticated(
                "the fake server knows no such agent",
            ));
        }
        let mut inbound = request.into_inner();
        let hello = match inbound.message().await? {
            Some(AgentMessage {
                message: Some(Inbound::Hello(hello)),
            }) => hello,
            other => {
                return Err(Status::invalid_argument(format!(
                    "the first message is not a Hello but {other:?}"
                )));
            }
        };
        let (outbound, receiver) = mpsc::unbounded_channel();
        let _ = self.streams.send(Stream {
            hello,
            inbound,
            outbound,
            received: Vec::new(),
        });
        Ok(Response::new(UnboundedReceiverStream::new(receiver)))
    }
}

impl FakeServer {
    pub async fn start() -> FakeServer {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a port for the fake server");
        let address = listener
            .local_addr()
            .expect("the address of the fake server");
        let agent_id: AgentId = Uuid::now_v7().try_into().expect("an agent id");
        let (streams, accepted) = mpsc::unbounded_channel();
        let (registered, registrations) = mpsc::unbounded_channel();
        let routes = Routes::new(AgentServer::new(Fake { streams }))
            .into_axum_router()
            .route(
                "/api/v1/agents/register",
                post(
                    move |Json(request): Json<RegisterAgentRequest>| async move {
                        register(request, agent_id, &registered)
                    },
                ),
            );
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, routes).await;
        });
        FakeServer {
            address,
            agent_id,
            streams: accepted,
            registrations,
            task,
        }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    pub fn agent_id(&self) -> AgentId {
        self.agent_id
    }

    pub async fn registration(&mut self) -> RegisterAgentRequest {
        timeout(WAIT, self.registrations.recv())
            .await
            .unwrap_or_else(|_| panic!("nobody registered within {WAIT:?}"))
            .expect("the fake server runs")
    }

    pub async fn accept(&mut self) -> Stream {
        timeout(WAIT, self.streams.recv())
            .await
            .unwrap_or_else(|_| panic!("no agent connected within {WAIT:?}"))
            .expect("the fake server runs")
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn register(
    request: RegisterAgentRequest,
    agent_id: AgentId,
    registered: &UnboundedSender<RegisterAgentRequest>,
) -> HttpResponse {
    if request.token != TOKEN {
        let envelope = ErrorEnvelope {
            error: ErrorBody::new(ErrorCode::Unauthenticated, "the registration token expired"),
        };
        return (StatusCode::UNAUTHORIZED, Json(envelope)).into_response();
    }
    let _ = registered.send(request);
    (
        StatusCode::CREATED,
        Json(RegisterAgentResponse {
            agent_id,
            credential: CREDENTIAL.to_owned(),
        }),
    )
        .into_response()
}

pub struct Stream {
    pub hello: Hello,
    inbound: Streaming<AgentMessage>,
    outbound: UnboundedSender<Result<ServerMessage, Status>>,
    pub received: Vec<Inbound>,
}

impl Stream {
    pub fn send(&self, message: Outgoing) {
        let _ = self.outbound.send(Ok(ServerMessage {
            message: Some(message),
        }));
    }

    pub fn welcome(&self, agent_id: AgentId) {
        self.send(Outgoing::Welcome(Welcome {
            agent_id: agent_id.to_string(),
            server_version: "0.1.0".to_owned(),
            settings: Some(Settings {
                heartbeat_interval: prost_types::Duration::try_from(HEARTBEAT).ok(),
            }),
        }));
    }

    pub fn fail(self, status: Status) {
        let _ = self.outbound.send(Err(status));
    }

    pub async fn next(&mut self) -> Inbound {
        loop {
            let message = timeout(WAIT, self.inbound.message())
                .await
                .unwrap_or_else(|_| {
                    panic!(
                        "the agent sent nothing for {WAIT:?} after {:#?}",
                        self.received
                    )
                })
                .unwrap_or_else(|status| panic!("the stream from the agent failed: {status:?}"))
                .unwrap_or_else(|| {
                    panic!("the agent closed the stream after {:#?}", self.received)
                });
            match message.message {
                Some(Inbound::Heartbeat(_)) | None => {}
                Some(message) => {
                    self.received.push(message.clone());
                    return message;
                }
            }
        }
    }

    pub async fn until<T>(
        &mut self,
        what: &str,
        mut found: impl FnMut(&Inbound) -> Option<T>,
    ) -> T {
        loop {
            let message = self.next().await;
            if let Some(value) = found(&message) {
                return value;
            }
            assert!(
                !matches!(message, Inbound::Hello(_)),
                "a second Hello while waiting for {what}"
            );
        }
    }

    pub async fn idle(&mut self) {
        self.until("Idle", |message| {
            matches!(message, Inbound::Idle(_)).then_some(())
        })
        .await;
    }

    pub async fn closed(mut self) -> Vec<Inbound> {
        let mut after = Vec::new();
        loop {
            match timeout(WAIT, self.inbound.message()).await {
                Ok(Ok(Some(AgentMessage {
                    message: Some(Inbound::Heartbeat(_)) | None,
                }))) => {}
                Ok(Ok(Some(AgentMessage {
                    message: Some(message),
                }))) => after.push(message),
                Ok(Ok(None) | Err(_)) => return after,
                Err(_) => panic!("the agent did not close its stream within {WAIT:?}"),
            }
        }
    }

    pub fn acknowledge(&self, execution_id: &str, seq: u64) {
        self.send(Outgoing::LogAck(LogAck {
            execution_id: execution_id.to_owned(),
            seq,
        }));
    }

    pub async fn barrier(&mut self) {
        let id = Uuid::now_v7().to_string();
        self.send(Outgoing::Control(Control {
            id: id.clone(),
            execution_id: Uuid::now_v7().to_string(),
            kind: ControlKind::Cancel.into(),
        }));
        let ack: ControlAck = self
            .until("the barrier", |message| match message {
                Inbound::ControlAck(ack) if ack.id == id => Some(ack.clone()),
                _ => None,
            })
            .await;
        assert!(
            !ack.took_effect,
            "a cancel of an unknown execution took effect"
        );
    }
}

pub fn logs(messages: &[Inbound]) -> BTreeMap<u64, Log> {
    let mut logs = BTreeMap::new();
    for message in messages {
        if let Inbound::Log(log) = message
            && let Some(earlier) = logs.insert(log.seq, log.clone())
        {
            assert_eq!(
                earlier, *log,
                "the chunk {} arrived twice with other lines",
                log.seq
            );
        }
    }
    logs
}
