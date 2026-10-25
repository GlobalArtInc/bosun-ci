use sea_orm::DatabaseConnection;
use tokio::time::{MissedTickBehavior, interval};
use tracing::{info, warn};

use super::progress;
use crate::config::Agents;
use crate::site::Site;

pub(crate) async fn run(db: DatabaseConnection, settings: Agents, site: Site) {
    let mut ticks = interval(settings.heartbeat_interval);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        match bosun_db::agents::sweep(&db, settings.offline_after, settings.grace).await {
            Ok(Some(sweep)) => {
                for (agent_id, name) in sweep.offline {
                    info!(
                        %agent_id,
                        name,
                        offline_after_ms = settings.offline_after.as_millis(),
                        "an agent went offline without a heartbeat"
                    );
                }
                for execution_id in sweep.abandoned {
                    match progress::lose_after_grace(&db, execution_id, &site).await {
                        Ok(true) => warn!(
                            %execution_id,
                            grace_ms = settings.grace.as_millis(),
                            "an execution failed because its agent did not come back within the \
                             grace period"
                        ),
                        Ok(false) => {}
                        Err(error) => warn!(
                            %execution_id,
                            %error,
                            "an execution whose agent did not come back could not be failed"
                        ),
                    }
                }
            }
            Ok(None) => {}
            Err(error) => warn!(%error, "the sweep of agents failed"),
        }
    }
}
