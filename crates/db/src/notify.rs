use bosun_types::id::JobId;
use sea_orm::prelude::Uuid;
use sea_orm::sqlx::postgres::PgListener;

use crate::control::{self, Notice};
use crate::{Error, queue, sqlx_error};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notification {
    Control(Notice),
    Queued(JobId),
}

pub struct Listener(PgListener);

impl Listener {
    pub async fn connect(url: &str, replica_id: Uuid) -> Result<Listener, Error> {
        let mut listener = PgListener::connect(url).await.map_err(sqlx_error)?;
        let replica = control::channel(replica_id);
        listener
            .listen_all([replica.as_str(), queue::CHANNEL])
            .await
            .map_err(sqlx_error)?;
        Ok(Listener(listener))
    }

    pub async fn next(&mut self) -> Result<Option<Notification>, Error> {
        loop {
            let Some(notification) = self.0.try_recv().await.map_err(sqlx_error)? else {
                return Ok(None);
            };
            let payload = notification.payload();
            let heard = if notification.channel() == queue::CHANNEL {
                payload.parse().ok().map(Notification::Queued)
            } else {
                control::notice(payload).map(Notification::Control)
            };
            if let Some(heard) = heard {
                return Ok(Some(heard));
            }
        }
    }
}
