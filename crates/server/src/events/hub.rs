use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bosun_db::stream::Notice;
use bosun_types::id::RunId;
use tokio::sync::watch;

const LIFETIME: Duration = Duration::from_secs(60);

pub(crate) struct Hub {
    lifetime: Duration,
    next: AtomicU64,
    subscribers: Mutex<HashMap<u64, Subscriber>>,
}

struct Subscriber {
    runs: Vec<RunId>,
    sender: watch::Sender<()>,
}

pub(crate) struct Subscription {
    hub: Arc<Hub>,
    number: u64,
    announced: watch::Receiver<()>,
}

impl Default for Hub {
    fn default() -> Hub {
        Hub::lasting(LIFETIME)
    }
}

impl Hub {
    pub(crate) fn lasting(lifetime: Duration) -> Hub {
        Hub {
            lifetime,
            next: AtomicU64::new(0),
            subscribers: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn lifetime(&self) -> Duration {
        self.lifetime
    }

    pub(crate) fn subscribe(self: &Arc<Self>, runs: Vec<RunId>) -> Subscription {
        let number = self.next.fetch_add(1, Ordering::Relaxed);
        let (sender, announced) = watch::channel(());
        self.subscribers()
            .insert(number, Subscriber { runs, sender });
        Subscription {
            hub: Arc::clone(self),
            number,
            announced,
        }
    }

    pub(crate) fn announce(&self, notice: Notice) {
        for subscriber in self.subscribers().values() {
            let concerned = match notice {
                Notice::Run(run) => subscriber.runs.is_empty() || subscriber.runs.contains(&run),
                Notice::Agent(_) => subscriber.runs.is_empty(),
            };
            if concerned {
                subscriber.sender.send_replace(());
            }
        }
    }

    pub(crate) fn wake_all(&self) {
        for subscriber in self.subscribers().values() {
            subscriber.sender.send_replace(());
        }
    }

    fn subscribers(&self) -> MutexGuard<'_, HashMap<u64, Subscriber>> {
        self.subscribers
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Subscription {
    pub(crate) async fn announced(&mut self) {
        if self.announced.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        self.hub.subscribers().remove(&self.number);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use bosun_db::stream::Notice;
    use bosun_types::id::{AgentId, RunId};
    use tokio::time::timeout;
    use uuid::Uuid;

    use super::Hub;

    fn id<T: TryFrom<Uuid>>() -> T
    where
        T::Error: std::fmt::Debug,
    {
        Uuid::now_v7().try_into().expect("a version 7 id")
    }

    async fn woken(subscription: &mut super::Subscription) -> bool {
        timeout(Duration::from_millis(200), subscription.announced())
            .await
            .is_ok()
    }

    #[tokio::test]
    async fn a_run_wakes_its_subscribers_and_those_of_everything_and_an_agent_only_the_latter() {
        let hub = Arc::new(Hub::default());
        let (one, other): (RunId, RunId) = (id(), id());
        let mut everything = hub.subscribe(Vec::new());
        let mut watching = hub.subscribe(vec![one]);
        let mut elsewhere = hub.subscribe(vec![other]);

        hub.announce(Notice::Run(one));
        assert!(woken(&mut everything).await);
        assert!(woken(&mut watching).await);
        assert!(!woken(&mut elsewhere).await);

        hub.announce(Notice::Agent(id::<AgentId>()));
        assert!(woken(&mut everything).await);
        assert!(!woken(&mut watching).await);
        assert!(!woken(&mut elsewhere).await);

        hub.wake_all();
        assert!(woken(&mut watching).await);
        assert!(woken(&mut elsewhere).await);
    }

    #[test]
    fn a_dropped_subscription_leaves_the_hub() {
        let hub = Arc::new(Hub::default());
        let subscription = hub.subscribe(Vec::new());
        assert_eq!(hub.subscribers().len(), 1);
        drop(subscription);
        assert!(hub.subscribers().is_empty());
    }
}
