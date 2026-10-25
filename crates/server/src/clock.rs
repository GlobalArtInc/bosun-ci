#[cfg(test)]
use std::sync::{Arc, Mutex};

#[cfg(test)]
use chrono::TimeDelta;
use chrono::{DateTime, SubsecRound, Utc};

#[derive(Clone)]
pub(crate) struct Clock(Source);

#[derive(Clone)]
enum Source {
    System,
    #[cfg(test)]
    Manual(Arc<Mutex<DateTime<Utc>>>),
}

impl Clock {
    pub(crate) fn system() -> Clock {
        Clock(Source::System)
    }

    #[cfg(test)]
    pub(crate) fn manual(start: DateTime<Utc>) -> Clock {
        Clock(Source::Manual(Arc::new(Mutex::new(start.trunc_subsecs(6)))))
    }

    #[cfg(test)]
    pub(crate) fn advance(&self, by: TimeDelta) {
        if let Source::Manual(now) = &self.0 {
            let mut now = now.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            *now += by;
        }
    }

    pub(crate) fn now(&self) -> DateTime<Utc> {
        match &self.0 {
            Source::System => Utc::now().trunc_subsecs(6),
            #[cfg(test)]
            Source::Manual(now) => *now.lock().unwrap_or_else(|poisoned| poisoned.into_inner()),
        }
    }
}
