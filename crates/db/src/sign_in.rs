use chrono::{SubsecRound, TimeDelta};
use sea_orm::prelude::DateTimeUtc;
use sea_orm::{ConnectionTrait, DatabaseTransaction, TransactionTrait};

use crate::Error;
use crate::row::statement;

const TABLE: &str = "sign_in_failure";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Account,
    Address,
}

impl Kind {
    fn text(self) -> &'static str {
        match self {
            Kind::Account => "account",
            Kind::Address => "address",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subject {
    pub kind: Kind,
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lockout {
    pub failures: u32,
    pub first: TimeDelta,
    pub longest: TimeDelta,
    pub forget: TimeDelta,
}

impl Lockout {
    fn period(&self, failures: u32) -> Option<TimeDelta> {
        let doublings = failures.checked_sub(self.failures)?;
        let period = 2_i32
            .checked_pow(doublings)
            .and_then(|factor| self.first.checked_mul(factor));
        Some(period.map_or(self.longest, |period| period.min(self.longest)))
    }
}

pub enum Reservation {
    Granted(Granted),
    Locked {
        subject: Subject,
        until: DateTimeUtc,
    },
}

pub struct Granted {
    held: Vec<Held>,
}

struct Held {
    subject: Subject,
    before: State,
    after: State,
}

#[derive(Clone, Copy)]
struct State {
    last_failed_at: DateTimeUtc,
    locked_until: Option<DateTimeUtc>,
}

pub async fn reserve<C: TransactionTrait>(
    db: &C,
    subjects: Vec<Subject>,
    now: DateTimeUtc,
    lockout: &Lockout,
) -> Result<Reservation, Error> {
    let now = now.trunc_subsecs(6);
    let transaction = db.begin().await?;
    let mut counted = Vec::with_capacity(subjects.len());
    let mut locked: Option<(Subject, DateTimeUtc)> = None;
    for subject in subjects {
        let (failures, before) = hold(&transaction, &subject, now).await?;
        match before.locked_until.filter(|until| *until > now) {
            Some(until) => {
                if locked.as_ref().is_none_or(|(_, latest)| until > *latest) {
                    locked = Some((subject, until));
                }
            }
            None => {
                let failures = if before.last_failed_at <= now - lockout.forget {
                    0
                } else {
                    failures
                };
                counted.push((subject, failures.saturating_add(1), before));
            }
        }
    }
    if let Some((subject, until)) = locked {
        transaction.rollback().await?;
        return Ok(Reservation::Locked { subject, until });
    }
    let mut held = Vec::with_capacity(counted.len());
    for (subject, failures, before) in counted {
        let after = State {
            last_failed_at: now,
            locked_until: lockout.period(failures).map(|period| now + period),
        };
        transaction
            .execute(statement(
                "UPDATE sign_in_failure SET failures = $3, last_failed_at = $4, \
                 locked_until = $5 WHERE kind = $1 AND key = $2",
                [
                    subject.kind.text().into(),
                    subject.key.clone().into(),
                    stored(failures)?.into(),
                    after.last_failed_at.into(),
                    after.locked_until.into(),
                ],
            ))
            .await?;
        held.push(Held {
            subject,
            before,
            after,
        });
    }
    transaction.commit().await?;
    Ok(Reservation::Granted(Granted { held }))
}

pub async fn release<C: ConnectionTrait>(db: &C, granted: Granted) -> Result<(), Error> {
    for held in granted.held {
        let kind = held.subject.kind.text().into();
        let key = held.subject.key.into();
        match held.subject.kind {
            Kind::Account => {
                db.execute(statement(
                    "DELETE FROM sign_in_failure WHERE kind = $1 AND key = $2",
                    [kind, key],
                ))
                .await?;
            }
            Kind::Address => {
                db.execute(statement(
                    "UPDATE sign_in_failure SET failures = GREATEST(failures - 1, 0), \
                     last_failed_at = CASE WHEN last_failed_at = $3 THEN $4 \
                     ELSE last_failed_at END, \
                     locked_until = CASE WHEN locked_until IS NOT DISTINCT FROM $5 THEN $6 \
                     ELSE locked_until END \
                     WHERE kind = $1 AND key = $2",
                    [
                        kind,
                        key,
                        held.after.last_failed_at.into(),
                        held.before.last_failed_at.into(),
                        held.after.locked_until.into(),
                        held.before.locked_until.into(),
                    ],
                ))
                .await?;
            }
        }
    }
    Ok(())
}

async fn hold(
    transaction: &DatabaseTransaction,
    subject: &Subject,
    now: DateTimeUtc,
) -> Result<(u32, State), Error> {
    let key = || [subject.kind.text().into(), subject.key.clone().into()];
    transaction
        .execute(statement(
            "INSERT INTO sign_in_failure (kind, key, failures, last_failed_at) \
             VALUES ($1, $2, 0, $3) ON CONFLICT (kind, key) DO NOTHING",
            key().into_iter().chain([now.into()]),
        ))
        .await?;
    let found = transaction
        .query_one(statement(
            "SELECT failures, last_failed_at, locked_until FROM sign_in_failure \
             WHERE kind = $1 AND key = $2 FOR UPDATE",
            key(),
        ))
        .await?
        .ok_or_else(|| Error::Vanished(format!("the failed sign-ins of {}", subject.key)))?;
    let failures: i32 = found.try_get("", "failures")?;
    Ok((
        u32::try_from(failures).map_err(|error| Error::Row {
            table: TABLE,
            message: error.to_string(),
        })?,
        State {
            last_failed_at: found.try_get("", "last_failed_at")?,
            locked_until: found.try_get("", "locked_until")?,
        },
    ))
}

fn stored(failures: u32) -> Result<i32, Error> {
    i32::try_from(failures).map_err(|error| Error::Row {
        table: TABLE,
        message: error.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::Lockout;

    #[test]
    fn the_lockout_starts_at_the_configured_failure_and_doubles_up_to_its_longest() {
        let lockout = Lockout {
            failures: 3,
            first: TimeDelta::minutes(1),
            longest: TimeDelta::minutes(5),
            forget: TimeDelta::days(1),
        };
        let periods: Vec<Option<i64>> = (1..=8)
            .map(|failures| lockout.period(failures).map(|period| period.num_minutes()))
            .collect();
        assert_eq!(
            periods,
            [
                None,
                None,
                Some(1),
                Some(2),
                Some(4),
                Some(5),
                Some(5),
                Some(5)
            ]
        );
        assert_eq!(lockout.period(u32::MAX), Some(TimeDelta::minutes(5)));
    }
}
