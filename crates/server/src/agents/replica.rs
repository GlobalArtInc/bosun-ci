use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bosun_db::control::{self, Control, Kind};
use bosun_db::notify::{Listener, Notification};
use bosun_proto::agent::v1::server_message::Message;
use bosun_proto::agent::v1::{self as proto, ControlKind, ServerMessage};
use bosun_types::id::{AgentId, ExecutionId, JobId};
use sea_orm::DatabaseConnection;
use tokio::sync::mpsc::UnboundedSender;
use tonic::Status;
use tracing::{info, warn};
use uuid::Uuid;

use crate::logs::Followers;
use crate::queue::Dispatcher;

const RETRY: Duration = Duration::from_secs(1);

pub(crate) type Outbound = UnboundedSender<Result<ServerMessage, Status>>;

pub(crate) struct Replica {
    id: Uuid,
    next: AtomicU64,
    streams: Mutex<HashMap<AgentId, Stream>>,
}

struct Stream {
    number: u64,
    outbound: Outbound,
    sent: HashSet<Uuid>,
    idle: bool,
    claiming: bool,
    again: bool,
    offered: Option<(ExecutionId, JobId)>,
    passed_over: HashSet<JobId>,
}

pub(crate) struct Turn {
    pub(crate) number: u64,
    pub(crate) passed_over: Vec<JobId>,
}

pub(crate) struct Offer {
    pub(crate) job: JobId,
    pub(crate) execution: ExecutionId,
    pub(crate) message: ServerMessage,
}

pub(crate) enum Turned {
    Offered(Box<Offer>),
    Contended,
    Nothing,
}

impl Stream {
    fn turn(&self) -> Turn {
        Turn {
            number: self.number,
            passed_over: self.passed_over.iter().copied().collect(),
        }
    }
}

impl Replica {
    pub(crate) fn new() -> Arc<Replica> {
        Arc::new(Replica {
            id: Uuid::now_v7(),
            next: AtomicU64::new(0),
            streams: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn id(&self) -> Uuid {
        self.id
    }

    pub(crate) fn attach(&self, agent: AgentId, outbound: Outbound) -> u64 {
        let number = self.next.fetch_add(1, Ordering::Relaxed);
        let replaced = self.streams().insert(
            agent,
            Stream {
                number,
                outbound,
                sent: HashSet::new(),
                idle: false,
                claiming: false,
                again: false,
                offered: None,
                passed_over: HashSet::new(),
            },
        );
        if let Some(replaced) = replaced {
            let _ = replaced.outbound.send(Err(Status::aborted(
                "a newer connection of this agent replaced this stream",
            )));
        }
        number
    }

    pub(crate) fn detach(&self, agent: AgentId, number: u64) -> bool {
        let mut streams = self.streams();
        let current = streams
            .get(&agent)
            .is_some_and(|stream| stream.number == number);
        if current {
            streams.remove(&agent);
        }
        current
    }

    pub(crate) fn deliver(&self, control: &Control) {
        let mut streams = self.streams();
        let Some(stream) = streams.get_mut(&control.agent_id) else {
            return;
        };
        if !stream.sent.insert(control.id) {
            return;
        }
        let kind = match control.kind {
            Kind::Cancel => ControlKind::Cancel,
        };
        let _ = stream.outbound.send(Ok(ServerMessage {
            message: Some(Message::Control(proto::Control {
                id: control.id.to_string(),
                execution_id: control.execution_id.to_string(),
                kind: kind.into(),
            })),
        }));
    }

    pub(crate) fn set_idle(&self, agent: AgentId, number: u64) {
        let mut streams = self.streams();
        if let Some(stream) = current(&mut streams, agent, number) {
            stream.idle = true;
        }
    }

    pub(crate) fn rejected(&self, agent: AgentId, number: u64, execution: ExecutionId) {
        let mut streams = self.streams();
        if let Some(stream) = current(&mut streams, agent, number)
            && let Some((offered, job)) = stream.offered
            && offered == execution
        {
            stream.passed_over.insert(job);
        }
    }

    pub(crate) fn take_turn(&self, agent: AgentId) -> Option<Turn> {
        let mut streams = self.streams();
        let stream = streams.get_mut(&agent).filter(|stream| stream.idle)?;
        if stream.claiming {
            stream.again = true;
            return None;
        }
        stream.claiming = true;
        stream.again = false;
        Some(stream.turn())
    }

    pub(crate) fn end_turn(&self, agent: AgentId, number: u64, turned: Turned) -> Option<Turn> {
        let mut streams = self.streams();
        let stream = current(&mut streams, agent, number)?;
        let retry = match turned {
            Turned::Offered(offer) => {
                let _ = stream.outbound.send(Ok(offer.message));
                stream.offered = Some((offer.execution, offer.job));
                stream.idle = false;
                false
            }
            Turned::Contended => stream.idle,
            Turned::Nothing => stream.again && stream.idle,
        };
        stream.again = false;
        stream.claiming = retry;
        retry.then(|| stream.turn())
    }

    pub(crate) fn idle_agents(&self) -> Vec<AgentId> {
        self.streams()
            .iter()
            .filter(|(_, stream)| stream.idle)
            .map(|(agent, _)| *agent)
            .collect()
    }

    fn holds(&self, agent: AgentId) -> bool {
        self.streams().contains_key(&agent)
    }

    fn agents(&self) -> Vec<AgentId> {
        self.streams().keys().copied().collect()
    }

    fn streams(&self) -> MutexGuard<'_, HashMap<AgentId, Stream>> {
        self.streams
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn current(
    streams: &mut HashMap<AgentId, Stream>,
    agent: AgentId,
    number: u64,
) -> Option<&mut Stream> {
    streams
        .get_mut(&agent)
        .filter(|stream| stream.number == number)
}

pub(crate) async fn deliver_pending(db: &DatabaseConnection, replica: &Replica, agent: AgentId) {
    match control::pending(db, agent).await {
        Ok(pending) => {
            for control in &pending {
                replica.deliver(control);
            }
        }
        Err(error) => warn!(
            agent_id = %agent,
            %error,
            "the pending control messages of an agent could not be read"
        ),
    }
}

pub(crate) async fn forward(
    db: DatabaseConnection,
    replica: Arc<Replica>,
    mut listener: Listener,
    dispatcher: Arc<Dispatcher>,
    followers: Arc<Followers>,
) {
    loop {
        match listener.next().await {
            Ok(Some(Notification::Queued(_))) => dispatcher.wake_all(),
            Ok(Some(Notification::Log(notice))) => followers.announce(notice.execution_id),
            Ok(Some(Notification::Control(notice))) => {
                if !replica.holds(notice.agent_id) {
                    continue;
                }
                match control::find_pending(&db, notice.control_id).await {
                    Ok(Some(control)) => replica.deliver(&control),
                    Ok(None) => {}
                    Err(error) => warn!(
                        control_id = %notice.control_id,
                        %error,
                        "a control message could not be read"
                    ),
                }
            }
            Ok(None) => {
                info!("the notification connection was re-established");
                resend(&db, &replica).await;
                dispatcher.wake_all();
                followers.wake_all();
            }
            Err(error) => {
                warn!(%error, "the notification connection failed");
                tokio::time::sleep(RETRY).await;
                resend(&db, &replica).await;
                dispatcher.wake_all();
                followers.wake_all();
            }
        }
    }
}

async fn resend(db: &DatabaseConnection, replica: &Replica) {
    for agent in replica.agents() {
        deliver_pending(db, replica, agent).await;
    }
}
