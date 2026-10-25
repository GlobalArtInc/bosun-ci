use std::sync::Arc;

use bosun_db::agents::Connection;
use bosun_db::logs::{Chunk, Ingested};
use bosun_proto::agent::v1::agent_message::Message as Inbound;
use bosun_proto::agent::v1::agent_server::Agent;
use bosun_proto::agent::v1::server_message::Message as Outgoing;
use bosun_proto::agent::v1::{
    AgentMessage, Capability as Offered, Conclusion as Reported, Hello, Log, LogAck, ServerMessage,
    Settings, Welcome,
};
use bosun_types::api::{self, Capability};
use bosun_types::id::{AgentId, ExecutionId};
use bosun_types::status::Conclusion;
use chrono::{DateTime, Utc};
use sea_orm::DatabaseConnection;
use tokio::sync::mpsc;
use tokio::time::timeout;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tonic::metadata::MetadataMap;
use tonic::{Request, Response, Status, Streaming};
use tracing::{Instrument, Span, error, info, warn};
use uuid::Uuid;

use super::admission::{self, SERVER_VERSION};
use super::progress::{self, JobEnd, Progress, Reporter, StepEnd};
use super::replica::{self, Outbound, Replica};
use super::{LABEL_RULE, valid_label};
use crate::auth;
use crate::config::Agents;
use crate::logs;
use crate::queue::Dispatcher;
use crate::token::{AGENT, Token};

const AUTOMATIC: &str = "self-hosted";

enum Ending {
    Breach(String),
    Unavailable(String),
}

impl From<String> for Ending {
    fn from(reason: String) -> Ending {
        Ending::Breach(reason)
    }
}

#[derive(Clone)]
pub(crate) struct Connector {
    db: DatabaseConnection,
    replica: Arc<Replica>,
    settings: Agents,
    dispatcher: Arc<Dispatcher>,
}

impl Connector {
    pub(crate) fn new(
        db: DatabaseConnection,
        replica: Arc<Replica>,
        settings: Agents,
        dispatcher: Arc<Dispatcher>,
    ) -> Connector {
        Connector {
            db,
            replica,
            settings,
            dispatcher,
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
        match bosun_db::agents::abandoned(&self.db, agent.id, &running).await {
            Ok(abandoned) => {
                for execution_id in abandoned {
                    match progress::lose_forgotten(
                        &self.db,
                        agent.id,
                        execution_id,
                        self.dispatcher.site(),
                    )
                    .await
                    {
                        Ok(true) => warn!(
                            agent_id = %agent.id,
                            %execution_id,
                            "an execution failed because its agent came back without it"
                        ),
                        Ok(false) => {}
                        Err(failure) => warn!(
                            agent_id = %agent.id,
                            %execution_id,
                            error = %failure,
                            "an execution its agent came back without could not be failed"
                        ),
                    }
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
            dispatcher: Arc::clone(&self.dispatcher),
            settings: self.settings,
            reporter: Reporter {
                id: agent.id,
                name: agent.name.clone(),
            },
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
    dispatcher: Arc<Dispatcher>,
    settings: Agents,
    reporter: Reporter,
    number: u64,
    outbound: Outbound,
}

impl Session {
    async fn run(self, mut inbound: Streaming<AgentMessage>) {
        let reason = self.receive(&mut inbound).await;
        if !self.replica.detach(self.reporter.id, self.number) {
            info!(agent_id = %self.reporter.id, reason, "a replaced stream of an agent ended");
            return;
        }
        if let Err(failure) =
            bosun_db::agents::disconnect(&self.db, self.reporter.id, self.replica.id()).await
        {
            warn!(agent_id = %self.reporter.id, error = %failure, "an agent could not be recorded as disconnected");
        }
        info!(agent_id = %self.reporter.id, reason, "an agent disconnected");
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
            if let Err(ending) = self.handle(message).await {
                let (status, reason) = match ending {
                    Ending::Breach(reason) => (Status::invalid_argument(reason.clone()), reason),
                    Ending::Unavailable(reason) => (Status::unavailable(reason.clone()), reason),
                };
                let _ = self.outbound.send(Err(status));
                return reason;
            }
        }
    }

    async fn handle(&self, message: AgentMessage) -> Result<(), Ending> {
        match message.message {
            Some(Inbound::Heartbeat(_)) => {
                if let Err(failure) = bosun_db::agents::heartbeat(&self.db, self.reporter.id).await
                {
                    warn!(agent_id = %self.reporter.id, error = %failure, "a heartbeat could not be recorded");
                }
            }
            Some(Inbound::ControlAck(ack)) => {
                let id: Uuid = ack
                    .id
                    .parse()
                    .map_err(|_| format!("`{}` is not the id of a control message", ack.id))?;
                match bosun_db::control::acknowledge(
                    &self.db,
                    self.reporter.id,
                    id,
                    ack.took_effect,
                )
                .await
                {
                    Ok(true) => info!(
                        agent_id = %self.reporter.id,
                        control_id = %id,
                        took_effect = ack.took_effect,
                        "an agent acknowledged a control message"
                    ),
                    Ok(false) => {}
                    Err(failure) => warn!(
                        agent_id = %self.reporter.id,
                        control_id = %id,
                        error = %failure,
                        "an acknowledgement could not be recorded"
                    ),
                }
            }
            Some(Inbound::Idle(_)) => self.dispatcher.idle(self.reporter.id, self.number),
            Some(Inbound::Reject(reject)) => {
                let execution = execution_id(&reject.execution_id)?;
                self.replica
                    .rejected(self.reporter.id, self.number, execution);
                info!(
                    agent_id = %self.reporter.id,
                    execution_id = %execution,
                    reason = reject.reason,
                    "an agent rejected a job"
                );
                let rejected = progress::rejected(&self.db, &self.reporter, execution).await;
                self.report("Reject", execution, rejected);
            }
            Some(Inbound::Accept(accept)) => {
                let execution = execution_id(&accept.execution_id)?;
                let accepted =
                    progress::accepted(&self.db, &self.reporter, execution, self.dispatcher.site())
                        .await;
                self.report("Accept", execution, accepted);
            }
            Some(Inbound::StepStarted(started)) => {
                let execution = execution_id(&started.execution_id)?;
                let recorded = progress::step_started(
                    &self.db,
                    &self.reporter,
                    execution,
                    started.step_index,
                    started.started_at.and_then(time),
                )
                .await;
                self.report("StepStarted", execution, recorded);
            }
            Some(Inbound::StepFinished(finished)) => {
                let execution = execution_id(&finished.execution_id)?;
                let end = StepEnd {
                    index: finished.step_index,
                    at: finished.completed_at.and_then(time),
                    outcome: conclusion(finished.outcome(), "the outcome of a step")?,
                    conclusion: conclusion(finished.conclusion(), "the conclusion of a step")?,
                };
                let recorded =
                    progress::step_finished(&self.db, &self.reporter, execution, end).await;
                self.report("StepFinished", execution, recorded);
            }
            Some(Inbound::JobFinished(finished)) => {
                let execution = execution_id(&finished.execution_id)?;
                let ended = JobEnd {
                    conclusion: conclusion(finished.conclusion(), "the conclusion of a job")?,
                    outputs: finished.outputs.into_iter().collect(),
                };
                let recorded = progress::job_finished(
                    &self.db,
                    &self.reporter,
                    execution,
                    ended,
                    self.dispatcher.site(),
                )
                .await;
                self.report("JobFinished", execution, recorded);
            }
            Some(Inbound::Log(log)) => self.log(log).await?,
            Some(Inbound::Hello(_)) => {
                return Err(Ending::Breach(
                    "a stream carries one Hello, as its first message".to_owned(),
                ));
            }
            None => {}
        }
        Ok(())
    }
}

impl Session {
    async fn log(&self, log: Log) -> Result<(), Ending> {
        let execution = execution_id(&log.execution_id)?;
        if log.seq == 0 || i64::try_from(log.seq).is_err() {
            return Err(Ending::Breach(format!(
                "{} is not the sequence number of a log chunk, which counts from 1",
                log.seq
            )));
        }
        let mut lines = log.lines;
        if lines.last().is_some_and(|last| *last != b'\n') {
            lines.push(b'\n');
        }
        let chunk = Chunk {
            agent: self.reporter.id,
            execution,
            step: log.step_index,
            seq: log.seq,
            lines: &lines,
        };
        let ingested = bosun_db::logs::ingest(&self.db, &chunk, logs::LIMIT)
            .await
            .map_err(|failure| {
                warn!(
                    agent_id = %self.reporter.id,
                    execution_id = %execution,
                    seq = log.seq,
                    error = %failure,
                    "a log chunk could not be stored"
                );
                Ending::Unavailable(
                    "the server could not store a log chunk; send it again after reconnecting"
                        .to_owned(),
                )
            })?;
        let ignored = match ingested {
            Ingested::Stored | Ingested::Duplicate | Ingested::Dropped { first: false } => None,
            Ingested::Dropped { first: true } => {
                warn!(
                    agent_id = %self.reporter.id,
                    execution_id = %execution,
                    limit_bytes = logs::LIMIT,
                    "the log of an execution reached its limit, and its further output is dropped"
                );
                None
            }
            Ingested::Closed => Some("the log of its execution is already compacted"),
            Ingested::NoExecution => Some("there is no such execution"),
            Ingested::OtherAgent => Some("its execution belongs to another agent"),
            Ingested::NoStep => Some("its execution has no step of that index"),
        };
        if let Some(reason) = ignored {
            warn!(
                agent_id = %self.reporter.id,
                execution_id = %execution,
                step_index = log.step_index,
                seq = log.seq,
                reason,
                "dropped a log chunk"
            );
        }
        let _ = self.outbound.send(Ok(ServerMessage {
            message: Some(Outgoing::LogAck(LogAck {
                execution_id: log.execution_id,
                seq: log.seq,
            })),
        }));
        Ok(())
    }

    fn report(
        &self,
        message: &'static str,
        execution_id: ExecutionId,
        outcome: Result<Progress, bosun_db::Error>,
    ) {
        match outcome {
            Ok(Progress::Applied) => {}
            Ok(Progress::Ignored(reason)) => warn!(
                agent_id = %self.reporter.id,
                %execution_id,
                message,
                reason,
                "ignored a report that does not fit the state of its execution"
            ),
            Err(failure) => warn!(
                agent_id = %self.reporter.id,
                %execution_id,
                message,
                error = %failure,
                "a report of an agent could not be recorded"
            ),
        }
    }
}

fn execution_id(text: &str) -> Result<ExecutionId, String> {
    text.parse()
        .map_err(|_| format!("`{text}` is not the id of an execution"))
}

fn conclusion(reported: Reported, what: &str) -> Result<Conclusion, String> {
    match reported {
        Reported::Success => Ok(Conclusion::Success),
        Reported::Failure => Ok(Conclusion::Failure),
        Reported::Cancelled => Ok(Conclusion::Cancelled),
        Reported::Skipped => Ok(Conclusion::Skipped),
        Reported::TimedOut => Ok(Conclusion::TimedOut),
        Reported::Unspecified => Err(format!("{what} is not specified")),
    }
}

fn time(at: prost_types::Timestamp) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(at.seconds, u32::try_from(at.nanos).ok()?)
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
