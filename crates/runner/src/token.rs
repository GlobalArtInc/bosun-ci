use std::time::Duration;

use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;
use tokio::time::{Instant, timeout};

const RENEW_BEFORE: Duration = Duration::from_secs(45 * 60);

const WAIT: Duration = Duration::from_secs(30);

pub(crate) const SECRET: &str = "GITHUB_TOKEN";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeToken {
    pub token: String,
    pub expires_at: Instant,
}

pub type Renewed = Result<ForgeToken, String>;

#[derive(Debug)]
pub struct Renewal {
    pub token: ForgeToken,
    pub renew: UnboundedSender<oneshot::Sender<Renewed>>,
}

impl Renewal {
    pub(crate) fn due(&self) -> bool {
        self.token
            .expires_at
            .saturating_duration_since(Instant::now())
            < RENEW_BEFORE
    }

    pub(crate) async fn ask(&self) -> Renewed {
        let (reply, answer) = oneshot::channel();
        if self.renew.send(reply).is_err() {
            return Err("the agent no longer takes requests for a fresh token".to_owned());
        }
        match timeout(WAIT, answer).await {
            Ok(Ok(renewed)) => renewed,
            Ok(Err(_)) => Err("the connection to the server ended before it answered".to_owned()),
            Err(_) => Err(format!(
                "the server sent no fresh token within {} seconds",
                WAIT.as_secs()
            )),
        }
    }
}
