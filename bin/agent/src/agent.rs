use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bosun_proto::agent::v1::agent_message::Message;
use bosun_proto::agent::v1::server_message::Message as Incoming;
use bosun_proto::agent::v1::{
    ASSIGNMENT_REVISION, Accept, Assignment, Capability, Control, ControlAck, ControlKind, Hello,
    Idle, Log, LogAck, PROTOCOL, Reject, RenewForgeToken, RenewedForgeToken, RunningExecution,
};
use bosun_runner::container::{self, Recovery};
use bosun_runner::{Renewed, Tracking};
use bosun_types::id::ExecutionId;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;
use tokio::task::{JoinError, JoinHandle};
use tokio::time::{self, Instant, MissedTickBehavior, sleep, sleep_until};
use tracing::{error, info, warn};

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

const RECOVERY_FIRST: Duration = Duration::from_secs(5);

const RECOVERY_LAST: Duration = Duration::from_secs(300);

type Stop = Pin<Box<dyn Future<Output = String> + Send>>;

pub(crate) struct Agent {
    config: Config,
    capabilities: Vec<Capability>,
    spool: Arc<Spool>,
    tracking: Arc<Tracking>,
    job: Option<Running>,
    pending: VecDeque<Message>,
    out: UnboundedSender<Out>,
    results: UnboundedReceiver<Out>,
    controls: Controls,
    renewing: Option<(ExecutionId, oneshot::Sender<Renewed>)>,
    stopping: bool,
    recovery: Option<Recovery>,
    recovering: Option<Recovering>,
}

enum Recovering {
    Waiting {
        due: Instant,
        delay: Duration,
    },
    Running {
        attempt: JoinHandle<Result<(), String>>,
        delay: Duration,
    },
}

enum Recovered {
    Due,
    Done(Result<Result<(), String>, JoinError>),
}

fn next_delay(delay: Duration) -> Duration {
    delay.saturating_mul(2).min(RECOVERY_LAST)
}

async fn recovery_event(recovering: &mut Option<Recovering>) -> Recovered {
    match recovering {
        None => std::future::pending().await,
        Some(Recovering::Waiting { due, .. }) => {
            sleep_until(*due).await;
            Recovered::Due
        }
        Some(Recovering::Running { attempt, .. }) => Recovered::Done(attempt.await),
    }
}

enum Ended {
    Stopped,
    Lost { welcomed: bool, reason: String },
}

impl Agent {
    pub(crate) fn new(
        config: Config,
        capabilities: Vec<Capability>,
        spool: Spool,
        tracking: Tracking,
    ) -> Agent {
        let (out, results) = mpsc::unbounded_channel();
        let recovery = capabilities
            .contains(&Capability::Podman)
            .then(|| job::recovery(&config.dir));
        Agent {
            config,
            capabilities,
            spool: Arc::new(spool),
            tracking: Arc::new(tracking),
            job: None,
            pending: VecDeque::new(),
            out,
            results,
            controls: Controls::default(),
            renewing: None,
            stopping: false,
            recovery,
            recovering: None,
        }
    }

    fn marked(&self) -> bool {
        self.recovery
            .as_ref()
            .is_some_and(|recovery| container::is_marked(&recovery.state))
    }

    async fn offer(&mut self, connection: &Connection) -> Result<(), String> {
        if self.job.is_some() || self.stopping {
            return Ok(());
        }
        if self.marked() {
            if self.recovering.is_none() {
                error!(
                    retry_in_seconds = RECOVERY_FIRST.as_secs(),
                    "a container of an earlier job could not be removed, so the agent takes no \
                     job until it can: it asks for no job and retries the removal"
                );
                self.recovering = Some(Recovering::Waiting {
                    due: Instant::now() + RECOVERY_FIRST,
                    delay: RECOVERY_FIRST,
                });
            }
            return Ok(());
        }
        self.recovering = None;
        connection.send(Message::Idle(Idle {})).await
    }

    fn recover(&mut self) {
        let (Some(recovery), Some(Recovering::Waiting { delay, .. })) =
            (&self.recovery, &self.recovering)
        else {
            return;
        };
        let (engine, state, delay) = (Arc::clone(&recovery.engine), recovery.state.clone(), *delay);
        let attempt = tokio::spawn(async move { container::recover(&engine, &state).await });
        self.recovering = Some(Recovering::Running { attempt, delay });
    }

    async fn recovered(
        &mut self,
        connection: &Connection,
        result: Result<Result<(), String>, JoinError>,
    ) -> Result<(), String> {
        let delay = match self.recovering.take() {
            Some(Recovering::Running { delay, .. }) => delay,
            other => {
                self.recovering = other;
                return Ok(());
            }
        };
        let fault = match result {
            Ok(Ok(())) => {
                info!(
                    "the agent removed what an earlier job left, and offers itself for jobs again"
                );
                return self.offer(connection).await;
            }
            Ok(Err(fault)) => fault,
            Err(error) => {
                format!("the removal of what an earlier job left did not finish: {error}")
            }
        };
        let delay = next_delay(delay);
        error!(
            %fault,
            retry_in_seconds = delay.as_secs(),
            "the agent still takes no job"
        );
        self.recovering = Some(Recovering::Waiting {
            due: Instant::now() + delay,
            delay,
        });
        Ok(())
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
            Out::Renew { reply, .. } => {
                let _ = reply.send(Err(
                    "the agent has no connection to the server to ask for a fresh token".to_owned(),
                ));
            }
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
            capabilities: self
                .capabilities
                .iter()
                .map(|held| (*held).into())
                .collect(),
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
        let served = self.serve(&mut connection, stop).await;
        self.renewing = None;
        let reason = match served {
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
        self.offer(connection).await?;
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
                    Out::Renew { execution, reply } => {
                        self.renewing = Some((execution, reply));
                        connection
                            .send(Message::RenewForgeToken(RenewForgeToken {
                                execution_id: execution.to_string(),
                            }))
                            .await?;
                    }
                    Out::Finished(execution) => {
                        self.finished(execution);
                        if self.stopped() {
                            return Ok(());
                        }
                        self.offer(connection).await?;
                    }
                },
                recovered = recovery_event(&mut self.recovering), if !self.stopping => match recovered {
                    Recovered::Due => self.recover(),
                    Recovered::Done(result) => self.recovered(connection, result).await?,
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
            Incoming::Drain(_) => {
                match &self.job {
                    Some(job) => info!(
                        execution_id = %job.execution,
                        "the server drained this agent: it finishes the job it runs and is \
                         offered no other until the drain is lifted"
                    ),
                    None => info!(
                        "the server drained this agent: it is offered no job until the drain is \
                         lifted"
                    ),
                }
                Ok(())
            }
            Incoming::RenewedForgeToken(renewed) => {
                self.renewed(renewed);
                Ok(())
            }
            Incoming::Welcome(_) => {
                warn!("the server sent a second Welcome, which the agent ignores");
                Ok(())
            }
        }
    }

    fn renewed(&mut self, renewed: RenewedForgeToken) {
        let waiting = self
            .renewing
            .as_ref()
            .is_some_and(|(execution, _)| execution.to_string() == renewed.execution_id);
        let Some((execution, reply)) = self.renewing.take().filter(|_| waiting) else {
            warn!(
                execution_id = renewed.execution_id,
                "the server sent a forge token that no job asked for"
            );
            return;
        };
        let answer = match renewed.token {
            _ if !renewed.error.is_empty() => {
                warn!(
                    execution_id = %execution,
                    error = renewed.error,
                    "the server did not renew the forge token"
                );
                Err(renewed.error)
            }
            Some(token) => {
                info!(execution_id = %execution, "received a fresh forge token");
                Ok(job::received(token))
            }
            None => Err("the server answered without a token".to_owned()),
        };
        let _ = reply.send(answer);
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
        } else if self.marked() {
            Err(
                "this agent could not remove a container of an earlier job, so it takes no job \
                 until it can"
                    .to_owned(),
            )
        } else {
            job::prepare(
                assignment,
                &self.config,
                &connection.welcome.agent_name,
                self.capabilities.contains(&Capability::Podman),
            )
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
                self.offer(connection).await?;
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
    use std::time::Duration;

    use super::{Controls, REMEMBERED_CONTROLS, next_delay};

    #[test]
    fn the_wait_between_two_attempts_to_recover_doubles_from_five_seconds_to_five_minutes() {
        let mut delay = Duration::from_secs(5);
        let mut seen = Vec::new();
        for _ in 0..8 {
            seen.push(delay.as_secs());
            delay = next_delay(delay);
        }
        assert_eq!(seen, [5, 10, 20, 40, 80, 160, 300, 300]);
    }

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
