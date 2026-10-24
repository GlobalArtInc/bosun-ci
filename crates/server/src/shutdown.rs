use std::future;
use std::sync::Arc;

use tokio::sync::watch;

pub(crate) struct Closer(watch::Sender<Option<Arc<str>>>);

#[derive(Clone)]
pub(crate) struct Shutdown(watch::Receiver<Option<Arc<str>>>);

pub(crate) fn channel() -> (Closer, Shutdown) {
    let (sender, receiver) = watch::channel(None);
    (Closer(sender), Shutdown(receiver))
}

impl Closer {
    pub(crate) fn close(self, reason: String) {
        self.0.send_replace(Some(reason.into()));
    }
}

impl Shutdown {
    pub(crate) async fn reason(mut self) -> Arc<str> {
        if let Ok(reason) = self.0.wait_for(Option::is_some).await
            && let Some(reason) = reason.as_ref()
        {
            return Arc::clone(reason);
        }
        future::pending().await
    }
}
