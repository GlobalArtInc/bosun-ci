use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use bosun_db::Error;
use bosun_db::control::{self, Kind};
use bosun_db::queue::{Claim, Claimed, ClaimedJob};
use bosun_proto::agent::v1::server_message::Message;
use bosun_proto::agent::v1::step::Action as Offered;
use bosun_proto::agent::v1::{self as proto, Assignment, ServerMessage};
use bosun_types::id::{AgentId, ExecutionId, JobId};
use bosun_types::ir::{Action, JobKind, Segment, Template};
use sea_orm::{DatabaseConnection, TransactionTrait};
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

use crate::agents::{Offer, Replica, Turn, Turned};
use crate::config::Queue;
use crate::planner;
use crate::site::Site;

const SWEEP: Duration = Duration::from_secs(5);

const FIRST_RETRY: Duration = Duration::from_millis(10);

const LAST_RETRY: Duration = Duration::from_secs(1);

pub(crate) struct Dispatcher {
    db: DatabaseConnection,
    replica: Arc<Replica>,
    settings: Queue,
    site: Site,
}

impl Dispatcher {
    pub(crate) fn new(
        db: DatabaseConnection,
        replica: Arc<Replica>,
        settings: Queue,
        site: Site,
    ) -> Arc<Dispatcher> {
        Arc::new(Dispatcher {
            db,
            replica,
            settings,
            site,
        })
    }

    pub(crate) fn site(&self) -> &Site {
        &self.site
    }

    pub(crate) fn idle(self: &Arc<Self>, agent: AgentId, number: u64) {
        self.replica.set_idle(agent, number);
        self.wake(agent);
    }

    pub(crate) fn wake_all(self: &Arc<Self>) {
        for agent in self.replica.idle_agents() {
            self.wake(agent);
        }
    }

    fn wake(self: &Arc<Self>, agent: AgentId) {
        if let Some(turn) = self.replica.take_turn(agent) {
            tokio::spawn(Arc::clone(self).claim(agent, turn));
        }
    }

    async fn claim(self: Arc<Self>, agent: AgentId, mut turn: Turn) {
        let mut contended = 0;
        loop {
            let claimed = bosun_db::queue::claim(
                &self.db,
                Claim {
                    agent,
                    replica_id: self.replica.id(),
                    lease: self.settings.lease,
                    passed_over: &turn.passed_over,
                },
            )
            .await;
            let turned = match claimed {
                Ok(Claimed::Job(claimed)) => {
                    info!(
                        agent_id = %agent,
                        job_id = %claimed.job_id,
                        execution_id = %claimed.execution_id,
                        "assigned a job to an agent"
                    );
                    let job_id = claimed.job_id;
                    match offer(*claimed) {
                        Ok(offer) => Turned::Offered(Box::new(offer)),
                        Err(error) => {
                            warn!(
                                agent_id = %agent,
                                %job_id,
                                %error,
                                "a claimed job cannot be offered"
                            );
                            contended = 0;
                            Turned::Nothing
                        }
                    }
                }
                Ok(Claimed::Contended) => {
                    contended += 1;
                    tokio::time::sleep(pause(contended)).await;
                    Turned::Contended
                }
                Ok(Claimed::Nothing) => {
                    contended = 0;
                    Turned::Nothing
                }
                Err(error) => {
                    warn!(agent_id = %agent, %error, "a claim for an idle agent failed");
                    contended = 0;
                    Turned::Nothing
                }
            };
            match self.replica.end_turn(agent, turn.number, turned) {
                Some(next) => turn = next,
                None => return,
            }
        }
    }
}

fn pause(contended: u32) -> Duration {
    let longest = FIRST_RETRY
        .saturating_mul(1 << contended.saturating_sub(1).min(16))
        .min(LAST_RETRY);
    let half = longest / 2;
    let spread = u64::try_from(half.as_micros()).unwrap_or(u64::MAX).max(1);
    half + Duration::from_micros(getrandom::u64().unwrap_or(0) % spread)
}

pub(crate) async fn sweep(dispatcher: Arc<Dispatcher>) {
    let mut ticks = interval(SWEEP);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        dispatcher.wake_all();
        expire_leases(&dispatcher.db, dispatcher.settings.lease).await;
        time_out(
            &dispatcher.db,
            dispatcher.settings.max_queue_time,
            &dispatcher.site,
        )
        .await;
    }
}

async fn expire_leases(db: &DatabaseConnection, lease: Duration) {
    let expired = match bosun_db::queue::expired_leases(db).await {
        Ok(Some(expired)) => expired,
        Ok(None) => return,
        Err(error) => {
            warn!(%error, "the expiry of assignment leases failed");
            return;
        }
    };
    for execution_id in expired {
        match expire(db, execution_id).await {
            Ok(Some(job_id)) => info!(
                %execution_id,
                %job_id,
                lease_ms = lease.as_millis(),
                "a job went back to the queue because its agent did not accept it within the lease"
            ),
            Ok(None) => {}
            Err(error) => warn!(
                %execution_id,
                %error,
                "an assignment whose lease expired could not be returned to the queue"
            ),
        }
    }
}

pub(crate) async fn expire(
    db: &DatabaseConnection,
    execution: ExecutionId,
) -> Result<Option<JobId>, Error> {
    let transaction = db.begin().await?;
    let Some((run, _)) = bosun_db::runs::lock_execution(&transaction, execution).await? else {
        return Ok(None);
    };
    let Some(job) = run.expire_lease(execution).await? else {
        return Ok(None);
    };
    control::publish(run.transaction(), execution, Kind::Cancel).await?;
    transaction.commit().await?;
    Ok(Some(job))
}

async fn time_out(db: &DatabaseConnection, max_queue_time: Duration, site: &Site) {
    let overdue = match bosun_db::queue::overdue(db, max_queue_time).await {
        Ok(Some(overdue)) => overdue,
        Ok(None) => return,
        Err(error) => {
            warn!(%error, "the queue timeout failed");
            return;
        }
    };
    for job_id in overdue {
        match cancel_unclaimed(db, job_id, max_queue_time, site).await {
            Ok(true) => warn!(
                %job_id,
                max_queue_time_ms = max_queue_time.as_millis(),
                "a job was cancelled because no agent took it within max_queue_time"
            ),
            Ok(false) => {}
            Err(error) => warn!(
                %job_id,
                %error,
                "a job that stayed queued too long could not be cancelled"
            ),
        }
    }
}

pub(crate) async fn cancel_unclaimed(
    db: &DatabaseConnection,
    job: JobId,
    max_queue_time: Duration,
    site: &Site,
) -> Result<bool, Error> {
    let transaction = db.begin().await?;
    let Some(mut run) = bosun_db::runs::lock_job(&transaction, job).await? else {
        return Ok(false);
    };
    if !run.time_out(job, max_queue_time).await? {
        return Ok(false);
    }
    planner::plan(&mut run, site).await?;
    transaction.commit().await?;
    Ok(true)
}

pub(crate) fn offer(claimed: ClaimedJob) -> Result<Offer, Error> {
    let merged: HashMap<&String, &Template> = claimed.env.iter().chain(&claimed.job.env).collect();
    let env = merged
        .into_iter()
        .filter_map(|(name, value)| Some((name.clone(), value.as_literal()?)))
        .collect();
    let JobKind::Steps(steps) = claimed.job.kind;
    let steps = (0..)
        .zip(steps)
        .map(|(index, step)| {
            let Action::Run { script } = &step.action else {
                return Err(Error::Row {
                    table: "run",
                    message: format!(
                        "step {index} of job `{}` uses an action, which an agent runs from M1 \
                         step 33",
                        claimed.key
                    ),
                });
            };
            Ok(proto::Step {
                index,
                key: step.key.map(|key| key.to_string()),
                name: step.name.as_ref().map(template),
                action: Some(Offered::Run(template(script))),
                shell: step.shell.map(|shell| shell.to_string()),
                working_directory: step.working_directory.as_ref().map(template),
                env: step
                    .env
                    .iter()
                    .map(|(name, value)| (name.clone(), template(value)))
                    .collect(),
            })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    Ok(Offer {
        job: claimed.job_id,
        execution: claimed.execution_id,
        message: ServerMessage {
            message: Some(Message::Assignment(Assignment {
                run_id: claimed.run_id.to_string(),
                attempt_id: claimed.attempt_id.to_string(),
                job_id: claimed.job_id.to_string(),
                execution_id: claimed.execution_id.to_string(),
                job_key: claimed.key.to_string(),
                job_name: claimed.name,
                env,
                steps,
                organisation: claimed.organisation,
                project: claimed.project,
            })),
        },
    })
}

fn template(template: &Template) -> proto::Template {
    proto::Template {
        segments: template
            .segments()
            .iter()
            .map(|segment| proto::Segment {
                segment: match segment {
                    Segment::Text(text) => Some(proto::segment::Segment::Text(text.clone())),
                    Segment::Expression(_) => None,
                },
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::pause;

    #[test]
    fn a_contended_claim_pauses_twice_as_long_each_time_in_a_row_up_to_a_second() {
        for (contended, longest) in [(1, 10), (2, 20), (3, 40), (7, 640), (8, 1000), (40, 1000)] {
            let longest = Duration::from_millis(longest);
            for _ in 0..200 {
                let paused = pause(contended);
                assert!(
                    (longest / 2..=longest).contains(&paused),
                    "after {contended} contended claims a pause of {paused:?}"
                );
            }
        }
    }
}
