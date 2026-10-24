use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bosun_db::control::{self, Control, Kind, Listener};
use bosun_proto::agent::v1::server_message::Message;
use bosun_proto::agent::v1::{self as proto, ControlKind, ServerMessage};
use bosun_types::id::AgentId;
use sea_orm::DatabaseConnection;
use tokio::sync::mpsc::UnboundedSender;
use tonic::Status;
use tracing::{info, warn};
use uuid::Uuid;

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

pub(crate) async fn forward(db: DatabaseConnection, replica: Arc<Replica>, mut listener: Listener) {
    loop {
        match listener.next().await {
            Ok(Some(notice)) => {
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
            }
            Err(error) => {
                warn!(%error, "the notification connection failed");
                tokio::time::sleep(RETRY).await;
                resend(&db, &replica).await;
            }
        }
    }
}

async fn resend(db: &DatabaseConnection, replica: &Replica) {
    for agent in replica.agents() {
        deliver_pending(db, replica, agent).await;
    }
}
