mod admission;
mod http;
pub(crate) mod progress;
mod replica;
mod stream;
mod sweep;

use std::future::Future;
use std::sync::Arc;

use bosun_db::notify::Listener;
use sea_orm::DatabaseConnection;
use tokio::task::JoinHandle;

use crate::config::Agents;
use crate::logs::Followers;
use crate::openapi::Api;
use crate::queue::Dispatcher;

pub(crate) use replica::{Offer, Replica, Turn, Turned};
pub(crate) use stream::Connector;

const NAME_RULE: &str = "an agent name is 1 to 64 characters without spaces or control characters";

const LABEL_RULE: &str =
    "a label is 1 to 64 characters without spaces, commas or control characters";

const LONGEST: usize = 64;

pub(crate) struct Tasks(Vec<JoinHandle<()>>);

impl Tasks {
    pub(crate) fn spawn(&mut self, task: impl Future<Output = ()> + Send + 'static) {
        self.0.push(tokio::spawn(task));
    }

    pub(crate) async fn stop(mut self) {
        for task in &self.0 {
            task.abort();
        }
        for task in self.0.drain(..) {
            let _ = task.await;
        }
    }
}

impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

pub(crate) fn routes() -> Api {
    Api::new()
        .get("/api/v1/agents", http::list)
        .post("/api/v1/registration-tokens", http::create_token)
        .merge(Api::public().post("/api/v1/agents/register", http::register))
}

fn valid_name(name: &str) -> bool {
    (1..=LONGEST).contains(&name.chars().count())
        && name
            .chars()
            .all(|character| !character.is_whitespace() && !character.is_control())
}

fn valid_label(label: &str) -> bool {
    valid_name(label) && !label.contains(',')
}

pub(crate) fn start(
    db: DatabaseConnection,
    replica: Arc<Replica>,
    listener: Listener,
    settings: Agents,
    dispatcher: Arc<Dispatcher>,
    followers: Arc<Followers>,
) -> Tasks {
    let site = dispatcher.site().clone();
    Tasks(vec![
        tokio::spawn(replica::forward(
            db.clone(),
            replica,
            listener,
            dispatcher,
            followers,
        )),
        tokio::spawn(sweep::run(db, settings, site)),
    ])
}

#[cfg(test)]
mod tests {
    use super::{valid_label, valid_name};

    #[test]
    fn names_and_labels_have_no_spaces_and_at_most_64_characters() {
        for name in ["runner1-1", "r", "agent.ünï", &"a".repeat(64)] {
            assert!(valid_name(name), "`{name}` was refused");
            assert!(valid_label(name), "`{name}` was refused as a label");
        }
        for text in ["", "runner 1", "tab\there", "line\n", &"a".repeat(65)] {
            assert!(!valid_name(text), "`{text}` was accepted");
            assert!(!valid_label(text), "`{text}` was accepted as a label");
        }
        assert!(valid_name("a,b"));
        assert!(!valid_label("a,b"));
    }
}
