use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bosun_proto::agent::v1::agent_message::Message;
use bosun_proto::agent::v1::server_message::Message as Incoming;
use bosun_proto::agent::v1::{
    ASSIGNMENT_REVISION, Accept, Assignment, Capability, Control, ControlAck, ControlKind, Hello,
    Idle, Log, LogAck, PROTOCOL, Reject, RunningExecution,
};
use bosun_runner::Tracking;
use bosun_types::id::ExecutionId;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time::{self, MissedTickBehavior, sleep};
use tracing::{info, warn};

use crate::backoff;
use crate::config::Config;
use crate::connection::Connection;
use crate::host;
use crate::job::{self, Out, Running};
use crate::spool::Spool;

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

const HEARTBEAT: Duration = Duration::from_secs(30);

const CLOSING: Duration = Duration::from_secs(5);

const REMEMBERED_CONTROLS: usize = 1024;

type Stop = Pin<Box<dyn Future<Output = String> + Send>>;

pub(crate) struct Agent {
    config: Config,
    spool: Arc<Spool>,
    tracking: Arc<Tracking>,
    job: Option<Running>,
    pending: VecDeque<Message>,
    out: UnboundedSender<Out>,
    results: UnboundedReceiver<Out>,
    controls: Controls,
    stopping: bool,
}

enum Ended {
    Stopped,
    Lost { welcomed: bool, reason: String },
}

impl Agent {
    pub(crate) fn new(config: Config, spool: Spool, tracking: Tracking) -> Agent {
        let (out, results) = mpsc::unbounded_channel();
        Agent {
            config,
            spool: Arc::new(spool),
            tracking: Arc::new(tracking),
            job: None,
            pending: VecDeque::new(),
            out,
            results,
            controls: Controls::default(),
            stopping: false,
        }
    }

    pub(crate) async fn run(mut self, mut stop: Stop) {
        let mut failures = 0;
        loop {
            match self.attempt(&mut stop).await {
                Ended::Stopped => break,
                Ended::Lost {
                    welcomed: true,
                    reason,
                } => {
                    warn!(%reason, "the connection to the server was lost");
                    failures = 0;
                }
                Ended::Lost {
                    welcomed: false,
                    reason,
                } => warn!(%reason, "the agent could not connect to the server"),
            }
            if self.stopped() {
                break;
            }
            let delay = backoff::delay(failures);
            failures = failures.saturating_add(1);
            info!(delay_ms = delay.as_millis() as u64, "reconnecting");
            if self.offline_for(delay, &mut stop).await {
                break;
            }
        }
        info!("stopped");
    }

    fn stopped(&self) -> bool {
        self.stopping && self.job.is_none()
    }

    fn stop(&mut self, reason: &str) {
        self.stopping = true;
        info!(%reason, "stopping");
        if let Some(job) = &self.job {
            info!(execution_id = %job.execution, "cancelling the running job before stopping");
            job.cancel();
        }
    }

    async fn offline_for(&mut self, delay: Duration, stop: &mut Stop) -> bool {
        let pause = sleep(delay);
        tokio::pin!(pause);
        loop {
            tokio::select! {
                () = &mut pause => return false,
                Some(out) = self.results.recv() => self.offline(out),
                reason = stop.as_mut(), if !self.stopping => self.stop(&reason),
            }
            if self.stopped() {
                return true;
            }
        }
    }

    fn offline(&mut self, out: Out) {
        match out {
            Out::Log { .. } => {}
            Out::Report(message) => self.pending.push_back(message),
            Out::Finished(execution) => self.finished(execution),
        }
    }

    fn finished(&mut self, execution: ExecutionId) {
        if self
            .job
            .as_ref()
            .is_some_and(|job| job.execution == execution)
        {
            self.job = None;
        }
    }

    fn hello(&self) -> Hello {
        let mut executions: Vec<ExecutionId> = self.job.iter().map(|job| job.execution).collect();
        for message in &self.pending {
            if let Message::JobFinished(finished) = message
                && let Ok(execution) = finished.execution_id.parse()
                && !executions.contains(&execution)
            {
                executions.push(execution);
            }
        }
        Hello {
            version: VERSION.to_owned(),
            protocol: PROTOCOL.to_owned(),
            capabilities: vec![Capability::Shell.into()],
            labels: self.config.labels.clone(),
            os: host::os().to_owned(),
            arch: host::arch().to_owned(),
            hostname: host::hostname(),
            executions: executions
                .into_iter()
                .map(|execution| RunningExecution {
                    execution_id: execution.to_string(),
                    last_log_seq: self.spool.last(execution),
                })
                .collect(),
            assignment_revision: ASSIGNMENT_REVISION,
        }
    }

    async fn attempt(&mut self, stop: &mut Stop) -> Ended {
        let config = self.config.clone();
        let hello = self.hello();
        let opening = Connection::open(&config, hello);
        tokio::pin!(opening);
        let mut connection = loop {
            tokio::select! {
                opened = &mut opening => match opened {
                    Ok(connection) => break connection,
                    Err(reason) => return Ended::Lost { welcomed: false, reason },
                },
                Some(out) = self.results.recv() => self.offline(out),
                reason = stop.as_mut(), if !self.stopping => {
                    self.stop(&reason);
                }
            }
            if self.stopped() {
                return Ended::Stopped;
            }
        };
        info!(
            url = self.config.url,
            agent_id = connection.welcome.agent_id,
            server_version = connection.welcome.server_version,
            "connected"
        );
        let reason = match self.serve(&mut connection, stop).await {
            Ok(()) => {
                connection.close(CLOSING).await;
                return Ended::Stopped;
            }
            Err(reason) => reason,
        };
        Ended::Lost {
            welcomed: true,
            reason,
        }
    }

    async fn serve(&mut self, connection: &mut Connection, stop: &mut Stop) -> Result<(), String> {
        let interval = connection
            .welcome
            .settings
            .and_then(|settings| settings.heartbeat_interval)
            .and_then(|interval| Duration::try_from(interval).ok())
            .filter(|interval| !interval.is_zero())
            .unwrap_or(HEARTBEAT);
        let mut sent: HashMap<ExecutionId, u64> = HashMap::new();
        for (execution, seq) in self.spool.unacknowledged() {
            self.send_log(connection, &mut sent, execution, seq).await?;
        }
        while let Some(message) = self.pending.pop_front() {
            if let Err(reason) = connection.send(message.clone()).await {
                self.pending.push_front(message);
                return Err(reason);
            }
        }
        if self.stopped() {
            return Ok(());
        }
        if self.job.is_none() && !self.stopping {
            connection.send(Message::Idle(Idle {})).await?;
        }
        let mut heartbeats = time::interval(interval);
        heartbeats.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                received = connection.receive() => match received? {
                    Some(message) => self.handle(connection, message).await?,
                    None => return Err("the server ended the stream".to_owned()),
                },
                Some(out) = self.results.recv() => match out {
                    Out::Log { execution, seq } => {
                        self.send_log(connection, &mut sent, execution, seq).await?;
                    }
                    Out::Report(message) => {
                        if let Err(reason) = connection.send(message.clone()).await {
                            self.pending.push_back(message);
                            return Err(reason);
                        }
                    }
                    Out::Finished(execution) => {
                        self.finished(execution);
                        if self.stopped() {
                            return Ok(());
                        }
                        if self.job.is_none() && !self.stopping {
                            connection.send(Message::Idle(Idle {})).await?;
                        }
                    }
                },
                _ = heartbeats.tick() => {
                    connection
                        .send(Message::Heartbeat(host::heartbeat(&self.config.dir)))
                        .await?;
                }
                reason = stop.as_mut(), if !self.stopping => {
                    self.stop(&reason);
                    if self.stopped() {
                        return Ok(());
                    }
                }
            }
        }
    }

    async fn send_log(
        &self,
        connection: &Connection,
        sent: &mut HashMap<ExecutionId, u64>,
        execution: ExecutionId,
        seq: u64,
    ) -> Result<(), String> {
        if sent.get(&execution).is_some_and(|last| *last >= seq) {
            return Ok(());
        }
        let chunk = match self.spool.read(execution, seq) {
            Ok(Some(chunk)) => chunk,
            Ok(None) => return Ok(()),
            Err(error) => {
                warn!(execution_id = %execution, seq, %error, "a spooled log chunk could not be read");
                return Ok(());
            }
        };
        connection
            .send(Message::Log(Log {
                execution_id: execution.to_string(),
                step_index: chunk.step,
                seq: chunk.seq,
                lines: chunk.lines,
            }))
            .await?;
        sent.insert(execution, seq);
        Ok(())
    }

    async fn handle(&mut self, connection: &Connection, message: Incoming) -> Result<(), String> {
        match message {
            Incoming::Assignment(assignment) => self.assigned(connection, assignment).await,
            Incoming::Control(control) => self.control(connection, control).await,
            Incoming::LogAck(LogAck { execution_id, seq }) => {
                let Ok(execution) = execution_id.parse::<ExecutionId>() else {
                    warn!(execution_id, "a log acknowledgement names no execution");
                    return Ok(());
                };
                if let Err(error) = self.spool.acknowledge(execution, seq) {
                    warn!(%execution, seq, %error, "acknowledged log chunks could not be removed");
                }
                Ok(())
            }
            Incoming::Welcome(_) => {
                warn!("the server sent a second Welcome, which the agent ignores");
                Ok(())
            }
        }
    }

    async fn assigned(
        &mut self,
        connection: &Connection,
        assignment: Assignment,
    ) -> Result<(), String> {
        let execution_id = assignment.execution_id.clone();
        let prepared = if self.stopping {
            Err("the agent is stopping".to_owned())
        } else if let Some(job) = &self.job {
            Err(format!(
                "the agent is running the execution {}",
                job.execution
            ))
        } else {
            job::prepare(assignment, &self.config, &connection.welcome.agent_name)
        };
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(reason) => {
                warn!(%execution_id, %reason, "rejected a job");
                connection
                    .send(Message::Reject(Reject {
                        execution_id,
                        reason,
                    }))
                    .await?;
                if self.job.is_none() && !self.stopping {
                    connection.send(Message::Idle(Idle {})).await?;
                }
                return Ok(());
            }
        };
        connection
            .send(Message::Accept(Accept {
                execution_id: execution_id.clone(),
            }))
            .await?;
        info!(execution_id = %prepared.execution(), "accepted a job");
        self.job = Some(job::start(
            prepared,
            Arc::clone(&self.tracking),
            Arc::clone(&self.spool),
            self.out.clone(),
        ));
        Ok(())
    }

    async fn control(&mut self, connection: &Connection, control: Control) -> Result<(), String> {
        let took_effect = self
            .controls
            .answer(&control.id, || match (control.kind(), &self.job) {
                (ControlKind::Cancel, Some(job))
                    if job.execution.to_string() == control.execution_id =>
                {
                    info!(execution_id = %job.execution, control_id = control.id, "cancelling a job");
                    job.cancel();
                    true
                }
                _ => false,
            });
        connection
            .send(Message::ControlAck(ControlAck {
                id: control.id,
                took_effect,
            }))
            .await
    }
}

#[derive(Default)]
struct Controls {
    order: VecDeque<String>,
    results: HashMap<String, bool>,
}

impl Controls {
    fn answer(&mut self, id: &str, act: impl FnOnce() -> bool) -> bool {
        if let Some(took_effect) = self.results.get(id) {
            return *took_effect;
        }
        let took_effect = act();
        self.results.insert(id.to_owned(), took_effect);
        self.order.push_back(id.to_owned());
        while self.order.len() > REMEMBERED_CONTROLS {
            if let Some(oldest) = self.order.pop_front() {
                self.results.remove(&oldest);
            }
        }
        took_effect
    }
}

#[cfg(test)]
mod tests {
    use super::{Controls, REMEMBERED_CONTROLS};

    #[test]
    fn a_control_seen_before_gets_its_first_answer_until_it_is_one_of_the_oldest() {
        let mut controls = Controls::default();
        assert!(controls.answer("cancel", || true));
        assert!(controls.answer("cancel", || panic!("the agent acted on a control again")));
        assert!(!controls.answer("other", || false));
        assert!(!controls.answer("other", || panic!("the agent acted on a control again")));
        for newer in 2..REMEMBERED_CONTROLS {
            assert!(!controls.answer(&format!("newer {newer}"), || false));
        }
        assert!(controls.answer("cancel", || panic!("a control was forgotten too early")));
        assert!(!controls.answer("newest", || false));
        assert!(!controls.answer("cancel", || false));
    }
}
