pub(crate) mod compaction;
mod followers;
mod http;
mod object;
pub(crate) mod reader;
mod search;

use chrono::{DateTime, SecondsFormat, Utc};

pub(crate) use compaction::sweep;
pub(crate) use followers::Followers;
pub(crate) use http::routes;

const LIMIT_MIB: u64 = 128;

pub(crate) const LIMIT: u64 = LIMIT_MIB * 1024 * 1024;

pub(crate) fn notice(completed_at: DateTime<Utc>, dropped: u64) -> Vec<u8> {
    format!(
        "{} {dropped} bytes of output were dropped because the log of the execution reached its \
         limit of {LIMIT_MIB} MiB",
        completed_at.to_rfc3339_opts(SecondsFormat::Nanos, true)
    )
    .into_bytes()
}
