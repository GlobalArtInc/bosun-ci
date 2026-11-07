pub(crate) mod authentication;

use bosun_db::queue::ClaimedJob;
use bosun_proto::assignment::Runtime;
use bosun_services::JobIdentity;
use chrono::{DateTime, TimeDelta, Utc};
use sea_orm::DatabaseConnection;

use crate::keys::Keyring;
use crate::signing_key::{self, Failure};
use crate::site::Site;

const MAX_JOB_MINUTES: u32 = 7 * 24 * 60;

const EXTRA_MINUTES: u32 = 24 * 60 + 2 * 60 + 60;

pub(crate) const MAX_LIFETIME: TimeDelta =
    TimeDelta::minutes((MAX_JOB_MINUTES + EXTRA_MINUTES) as i64);

pub(crate) fn lifetime(timeout_minutes: u32) -> TimeDelta {
    let minutes = match timeout_minutes {
        0 => MAX_JOB_MINUTES,
        minutes => minutes.min(MAX_JOB_MINUTES),
    };
    TimeDelta::minutes(i64::from(minutes + EXTRA_MINUTES))
}

pub(crate) async fn runtime(
    db: &DatabaseConnection,
    keys: &Keyring,
    site: &Site,
    claimed: &ClaimedJob,
    now: DateTime<Utc>,
) -> Result<Option<Runtime>, Failure> {
    let origin = site.url();
    if origin.is_empty() {
        return Ok(None);
    }
    let key = signing_key::signer(db, keys, now).await?;
    let token = key
        .mint(
            JobIdentity {
                attempt: claimed.attempt_id,
                execution: claimed.execution_id,
            },
            now,
            now + lifetime(claimed.evaluated.timeout_minutes),
        )
        .map_err(Failure::Sign)?;
    Ok(Some(Runtime {
        token,
        runtime_url: format!("{origin}/_runtime/"),
        results_url: format!("{origin}/"),
        cache_url: format!("{origin}/_runtime/cache/"),
    }))
}
