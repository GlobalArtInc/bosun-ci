use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use bosun_types::id::ExecutionId;
use tokio::sync::watch;

#[derive(Default)]
pub(crate) struct Followers {
    executions: Mutex<HashMap<ExecutionId, watch::Sender<()>>>,
}

pub(crate) struct Subscription {
    followers: Arc<Followers>,
    execution: ExecutionId,
    announced: watch::Receiver<()>,
}

impl Followers {
    pub(crate) fn subscribe(self: &Arc<Self>, execution: ExecutionId) -> Subscription {
        let announced = self
            .executions()
            .entry(execution)
            .or_insert_with(|| watch::channel(()).0)
            .subscribe();
        Subscription {
            followers: Arc::clone(self),
            execution,
            announced,
        }
    }

    pub(crate) fn announce(&self, execution: ExecutionId) {
        if let Some(sender) = self.executions().get(&execution) {
            sender.send_replace(());
        }
    }

    pub(crate) fn wake_all(&self) {
        for sender in self.executions().values() {
            sender.send_replace(());
        }
    }

    fn executions(&self) -> MutexGuard<'_, HashMap<ExecutionId, watch::Sender<()>>> {
        self.executions
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
        let mut executions = self.followers.executions();
        if executions
            .get(&self.execution)
            .is_some_and(|sender| sender.receiver_count() <= 1)
        {
            executions.remove(&self.execution);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use bosun_types::id::ExecutionId;
    use tokio::time::timeout;
    use uuid::Uuid;

    use super::Followers;

    fn execution() -> ExecutionId {
        Uuid::now_v7().try_into().expect("an execution id")
    }

    #[tokio::test]
    async fn an_announcement_wakes_the_followers_of_its_execution_only() {
        let followers = Arc::new(Followers::default());
        let (one, other) = (execution(), execution());
        let mut first = followers.subscribe(one);
        let mut second = followers.subscribe(one);
        let mut elsewhere = followers.subscribe(other);
        followers.announce(one);
        timeout(Duration::from_secs(5), first.announced())
            .await
            .expect("the first follower was woken");
        timeout(Duration::from_secs(5), second.announced())
            .await
            .expect("the second follower was woken");
        assert!(
            timeout(Duration::from_millis(100), elsewhere.announced())
                .await
                .is_err(),
            "a follower of another execution was woken"
        );
        followers.wake_all();
        timeout(Duration::from_secs(5), elsewhere.announced())
            .await
            .expect("waking all wakes every follower");
    }

    #[test]
    fn the_last_subscription_of_an_execution_takes_its_entry_along() {
        let followers = Arc::new(Followers::default());
        let one = execution();
        let first = followers.subscribe(one);
        let second = followers.subscribe(one);
        drop(first);
        assert_eq!(followers.executions().len(), 1);
        drop(second);
        assert!(followers.executions().is_empty());
    }
}
