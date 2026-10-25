use std::time::Duration;

use chrono::Utc;

const FIRST_RETRY: Duration = Duration::from_secs(1);

const LAST_RETRY: Duration = Duration::from_secs(3600);

pub(crate) enum Failure {
    Retry { error: String, at_least: Duration },
    Refused(String),
}

impl From<bosun_db::Error> for Failure {
    fn from(error: bosun_db::Error) -> Failure {
        Failure::Retry {
            error: error.to_string(),
            at_least: Duration::ZERO,
        }
    }
}

impl From<bosun_forge::Error> for Failure {
    fn from(error: bosun_forge::Error) -> Failure {
        let at_least = match &error {
            bosun_forge::Error::RateLimited { reset } => {
                (*reset - Utc::now()).to_std().unwrap_or(Duration::ZERO)
            }
            bosun_forge::Error::Transport(_) => Duration::ZERO,
            bosun_forge::Error::Status { status, .. } if *status >= 500 => Duration::ZERO,
            _ => return Failure::Refused(format!("the forge refused: {error}")),
        };
        Failure::Retry {
            error: format!("the forge failed: {error}"),
            at_least,
        }
    }
}

pub(crate) fn backoff(attempts: u32) -> Duration {
    FIRST_RETRY
        .checked_mul(1 << attempts.min(12))
        .unwrap_or(LAST_RETRY)
        .min(LAST_RETRY)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::backoff;

    #[test]
    fn a_failure_waits_twice_as_long_each_time_up_to_an_hour() {
        let waits: Vec<u64> = (0..14)
            .map(|attempts| backoff(attempts).as_secs())
            .collect();
        assert_eq!(
            waits,
            [
                1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 3600, 3600
            ]
        );
        assert_eq!(backoff(u32::MAX), Duration::from_secs(3600));
    }
}
