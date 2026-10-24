use std::sync::Arc;

use bosun_db::agents::Connection;
use bosun_proto::agent::v1::agent_message::Message as Inbound;
use bosun_proto::agent::v1::agent_server::Agent;
use bosun_proto::agent::v1::server_message::Message as Outgoing;
use bosun_proto::agent::v1::{
    AgentMessage, Capability as Offered, Hello, ServerMessage, Settings, Welcome,
};
use bosun_types::api::{self, Capability};
use bosun_types::id::{AgentId, ExecutionId};
use sea_orm::DatabaseConnection;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tonic::metadata::MetadataMap;
use tonic::{Request, Response, Status, Streaming};
use tracing::{Instrument, Span, error, info, warn};
use uuid::Uuid;

use super::admission::{self, SERVER_VERSION};
use super::replica::{self, Outbound, Replica};
use super::{LABEL_RULE, valid_label};
use crate::auth;
use crate::config::Agents;
use crate::token::{AGENT, Token};

const AUTOMATIC: &str = "self-hosted";

#[derive(Clone)]
pub(crate) struct Connector {
    db: DatabaseConnection,
    replica: Arc<Replica>,
    settings: Agents,
}

impl Connector {
    pub(crate) fn new(
        db: DatabaseConnection,
        replica: Arc<Replica>,
        settings: Agents,
    ) -> Connector {
        Connector {
            db,
            replica,
            settings,
        }
    }

    async fn authenticate(&self, metadata: &MetadataMap) -> Result<api::Agent, Status> {
        let header = metadata
            .get("authorization")
            .ok_or_else(|| Status::unauthenticated("the stream carries no agent credential"))?;
        let credential = header.to_str().ok().and_then(auth::bearer).ok_or_else(|| {
            Status::unauthenticated("the authorization metadata does not hold a bearer token")
        })?;
        let token = Token::parse(AGENT, credential).ok_or_else(|| {
            Status::unauthenticated("the bearer token is not a well-formed agent credential")
        })?;
        bosun_db::agents::authenticate(&self.db, &token.sha256())
            .await
            .map_err(|failure| {
                error!(error = %failure, "an agent credential could not be looked up");
                Status::internal("the agent credential could not be checked")
            })?
            .ok_or_else(|| Status::unauthenticated("the agent credential is not known"))
    }

    async fn hello(&self, inbound: &mut Streaming<AgentMessage>) -> Result<Hello, Status> {
        let first = timeout(self.settings.offline_after, inbound.message())
            .await
            .map_err(|_| {
                Status::deadline_exceeded(format!(
                    "the agent sent no Hello within {:?}",
                    self.settings.offline_after
                ))
            })??;
        match first.and_then(|message| message.message) {
            Some(Inbound::Hello(hello)) => Ok(hello),
            _ => Err(Status::invalid_argument(
                "the first message on the stream must be a Hello",
            )),
        }
    }

    fn welcome(&self, agent: AgentId) -> ServerMessage {
        ServerMessage {
            message: Some(Outgoing::Welcome(Welcome {
                agent_id: agent.to_string(),
                server_version: SERVER_VERSION.to_owned(),
                settings: Some(Settings {
                    heartbeat_interval: prost_types::Duration::try_from(
                        self.settings.heartbeat_interval,
                    )
                    .ok(),
                }),
            })),
        }
    }
}

#[tonic::async_trait]
impl Agent for Connector {
    type ConnectStream = UnboundedReceiverStream<Result<ServerMessage, Status>>;

    async fn connect(
        &self,
        request: Request<Streaming<AgentMessage>>,
    ) -> Result<Response<Self::ConnectStream>, Status> {
        let agent = self.authenticate(request.metadata()).await?;
        let mut inbound = request.into_inner();
        let hello = self.hello(&mut inbound).await?;
        admission::admit(&hello, SERVER_VERSION).map_err(|refusal| {
            warn!(agent_id = %agent.id, name = agent.name, %refusal, "refused an agent");
            Status::failed_precondition(refusal)
        })?;
        let running = running(&hello)?;
        if let Some(label) = hello.labels.iter().find(|label| !valid_label(label)) {
            return Err(Status::invalid_argument(format!(
                "`{label}` is not a label: {LABEL_RULE}"
            )));
        }
        let (outbound, receiver) = mpsc::unbounded_channel();
        let _ = outbound.send(Ok(self.welcome(agent.id)));
        let number = self.replica.attach(agent.id, outbound.clone());
        let connected = bosun_db::agents::connect(
            &self.db,
            agent.id,
            Connection {
                replica_id: self.replica.id(),
                version: hello.version.clone(),
                labels: labels(&hello),
                capabilities: capabilities(&hello),
            },
        )
        .await;
        if let Err(failure) = connected {
            self.replica.detach(agent.id, number);
            error!(agent_id = %agent.id, error = %failure, "an agent could not be recorded as online");
            return Err(Status::internal(
                "the agent could not be recorded as online",
            ));
        }
        info!(
            agent_id = %agent.id,
            name = agent.name,
            version = hello.version,
            replica_id = %self.replica.id(),
            "an agent connected"
        );
        match bosun_db::agents::reconcile(&self.db, agent.id, &running).await {
            Ok(lost) => {
                for execution_id in lost {
                    warn!(
                        agent_id = %agent.id,
                        %execution_id,
                        "an execution failed because its agent came back without it"
                    );
                }
            }
            Err(failure) => warn!(
                agent_id = %agent.id,
                error = %failure,
                "the executions of an agent could not be reconciled"
            ),
        }
        replica::deliver_pending(&self.db, &self.replica, agent.id).await;
        let session = Session {
            db: self.db.clone(),
            replica: Arc::clone(&self.replica),
            settings: self.settings,
            agent: agent.id,
            number,
            outbound,
        };
        tokio::spawn(session.run(inbound).instrument(Span::current()));
        Ok(Response::new(UnboundedReceiverStream::new(receiver)))
    }
}

struct Session {
    db: DatabaseConnection,
    replica: Arc<Replica>,
    settings: Agents,
    agent: AgentId,
    number: u64,
    outbound: Outbound,
}

impl Session {
    async fn run(self, mut inbound: Streaming<AgentMessage>) {
        let reason = self.receive(&mut inbound).await;
        if !self.replica.detach(self.agent, self.number) {
            info!(agent_id = %self.agent, reason, "a replaced stream of an agent ended");
            return;
        }
        if let Err(failure) =
            bosun_db::agents::disconnect(&self.db, self.agent, self.replica.id()).await
        {
            warn!(agent_id = %self.agent, error = %failure, "an agent could not be recorded as disconnected");
        }
        info!(agent_id = %self.agent, reason, "an agent disconnected");
    }

    async fn receive(&self, inbound: &mut Streaming<AgentMessage>) -> String {
        loop {
            let next = tokio::select! {
                () = self.outbound.closed() => return "the stream to the agent closed".to_owned(),
                next = timeout(self.settings.offline_after, inbound.message()) => next,
            };
            let message = match next {
                Err(_) => {
                    let reason = format!(
                        "the agent sent no message within {:?}",
                        self.settings.offline_after
                    );
                    let _ = self.outbound.send(Err(Status::unavailable(reason.clone())));
                    return reason;
                }
                Ok(Err(status)) => return format!("the stream failed: {}", status.message()),
                Ok(Ok(None)) => return "the agent closed the stream".to_owned(),
                Ok(Ok(Some(message))) => message,
            };
            if let Err(reason) = self.handle(message).await {
                let _ = self
                    .outbound
                    .send(Err(Status::invalid_argument(reason.clone())));
                return reason;
            }
        }
    }

    async fn handle(&self, message: AgentMessage) -> Result<(), String> {
        match message.message {
            Some(Inbound::Heartbeat(_)) => {
                if let Err(failure) = bosun_db::agents::heartbeat(&self.db, self.agent).await {
                    warn!(agent_id = %self.agent, error = %failure, "a heartbeat could not be recorded");
                }
            }
            Some(Inbound::ControlAck(ack)) => {
                let id: Uuid = ack
                    .id
                    .parse()
                    .map_err(|_| format!("`{}` is not the id of a control message", ack.id))?;
                match bosun_db::control::acknowledge(&self.db, self.agent, id, ack.took_effect)
                    .await
                {
                    Ok(true) => info!(
                        agent_id = %self.agent,
                        control_id = %id,
                        took_effect = ack.took_effect,
                        "an agent acknowledged a control message"
                    ),
                    Ok(false) => {}
                    Err(failure) => warn!(
                        agent_id = %self.agent,
                        control_id = %id,
                        error = %failure,
                        "an acknowledgement could not be recorded"
                    ),
                }
            }
            Some(Inbound::Hello(_)) => {
                return Err("a stream carries one Hello, as its first message".to_owned());
            }
            Some(other) => warn!(
                agent_id = %self.agent,
                kind = kind(&other),
                "ignored a message this server does not handle yet"
            ),
            None => {}
        }
        Ok(())
    }
}

fn kind(message: &Inbound) -> &'static str {
    match message {
        Inbound::Hello(_) => "Hello",
        Inbound::Idle(_) => "Idle",
        Inbound::Accept(_) => "Accept",
        Inbound::Reject(_) => "Reject",
        Inbound::StepStarted(_) => "StepStarted",
        Inbound::StepFinished(_) => "StepFinished",
        Inbound::Log(_) => "Log",
        Inbound::JobFinished(_) => "JobFinished",
        Inbound::ControlAck(_) => "ControlAck",
        Inbound::Heartbeat(_) => "Heartbeat",
    }
}

fn running(hello: &Hello) -> Result<Vec<ExecutionId>, Status> {
    hello
        .executions
        .iter()
        .map(|running| {
            running.execution_id.parse().map_err(|_| {
                Status::invalid_argument(format!(
                    "`{}` is not the id of an execution",
                    running.execution_id
                ))
            })
        })
        .collect()
}

fn capabilities(hello: &Hello) -> Vec<Capability> {
    let mut capabilities = Vec::new();
    for offered in hello.capabilities() {
        let capability = match offered {
            Offered::Shell => Capability::Shell,
            Offered::Podman => Capability::Podman,
            Offered::Unspecified => continue,
        };
        if !capabilities.contains(&capability) {
            capabilities.push(capability);
        }
    }
    capabilities
}

fn labels(hello: &Hello) -> Vec<String> {
    let capabilities = capabilities(hello);
    let names = capabilities.iter().map(|capability| match capability {
        Capability::Shell => "shell",
        Capability::Podman => "podman",
    });
    let mut labels = Vec::new();
    for label in [AUTOMATIC, &hello.os, &hello.arch]
        .into_iter()
        .chain(names)
        .chain(hello.labels.iter().map(String::as_str))
    {
        let label = label.to_lowercase();
        if !label.is_empty() && !labels.contains(&label) {
            labels.push(label);
        }
    }
    labels
}
