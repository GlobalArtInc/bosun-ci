use std::collections::{HashSet, VecDeque};

use bosun_db::Error;
use bosun_db::concurrency::Member;
use bosun_db::control::{self, Kind};
use bosun_db::runs::{Grouped, JobState, Locked};
use bosun_expr::{Scope, Value};
use bosun_types::actor::Actor;
use bosun_types::event::{Arrival, ConcurrencyCancelled, Event};
use bosun_types::id::{JobId, RunId};
use bosun_types::ir::{self, Evaluable, JobTemplate, Queue};
use bosun_types::status::{JobStatus, StatusReason, WaitingReason};

use super::{
    Fault, INVALID_TYPE, INVALID_VALUE, Place, START, cancel_jobs, cover, described, key, stop,
};

const MOST_WAITING: usize = 100;

const QUEUE_FULL: &str = "concurrency_queue_full";

const DEADLOCK: &str = "concurrency_deadlock";

#[derive(Default)]
pub(super) struct Later(VecDeque<RunId>);

impl Later {
    pub(super) fn push(&mut self, run: RunId) {
        if !self.0.contains(&run) {
            self.0.push_back(run);
        }
    }

    pub(super) fn next(&mut self) -> Option<RunId> {
        self.0.pop_front()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Entered {
    Running,
    Waiting,
    Full,
    Deadlock,
}

pub(super) enum Owner<'a> {
    Workflow,
    Job(&'a JobTemplate),
}

impl Owner<'_> {
    fn place(&self, what: &str, path: &[&str], templates: &[&ir::Template]) -> Place {
        match self {
            Owner::Workflow => Place {
                what: format!("`{what}` of the workflow"),
                path: path.iter().map(|segment| key(segment)).collect(),
                span: cover(templates).unwrap_or(START),
            },
            Owner::Job(job) => Place::new(job, what, path, templates),
        }
    }
}

pub(super) fn evaluate(
    concurrency: &ir::Concurrency,
    owner: &Owner<'_>,
    scope: &dyn Scope,
) -> Result<Grouped, Fault> {
    let place = owner.place("concurrency", &["concurrency"], &[&concurrency.group]);
    let group = place.text(&concurrency.group, scope)?;
    if group.is_empty() {
        return Err(place.refuse(INVALID_VALUE, "names an empty group".to_owned()));
    }
    let cancel_in_progress = match &concurrency.cancel_in_progress {
        Evaluable::Literal(flag) => *flag,
        Evaluable::Template(template) => {
            let place = owner.place(
                "concurrency.cancel-in-progress",
                &["concurrency", "cancel-in-progress"],
                &[template],
            );
            match place.typed(template, scope)? {
                Value::Boolean(flag) => {
                    if flag && concurrency.queue == Queue::Max {
                        return Err(place.refuse(
                            INVALID_VALUE,
                            "gives `true` together with `queue: max`, which keeps every waiting \
                             member and cancels none"
                                .to_owned(),
                        ));
                    }
                    flag
                }
                other => {
                    return Err(place.refuse(
                        INVALID_TYPE,
                        format!(
                            "must be `true` or `false`, and its expression gives {}",
                            described(&other)
                        ),
                    ));
                }
            }
        }
    };
    Ok(Grouped {
        group,
        cancel_in_progress,
        queue: concurrency.queue,
    })
}

pub(super) fn refusal(
    entered: Entered,
    concurrency: &ir::Concurrency,
    owner: &Owner<'_>,
    grouped: &Grouped,
) -> Option<Fault> {
    let (code, problem) = match entered {
        Entered::Running | Entered::Waiting => return None,
        Entered::Full => (
            QUEUE_FULL,
            format!(
                "names the group `{}`, where {MOST_WAITING} members wait already under \
                 `queue: max`, so it cannot wait there",
                grouped.group
            ),
        ),
        Entered::Deadlock => (
            DEADLOCK,
            format!(
                "names the group `{}`, which its own run holds through the `concurrency` of the \
                 workflow, so it would wait for itself",
                grouped.group
            ),
        ),
    };
    Some(
        owner
            .place("concurrency", &["concurrency"], &[&concurrency.group])
            .refuse(code, problem),
    )
}

pub(super) async fn enter(
    run: &mut Locked<'_>,
    job: Option<JobId>,
    grouped: &Grouped,
    later: &mut Later,
) -> Result<Entered, Error> {
    let group = grouped.group.to_lowercase();
    if job.is_some()
        && run
            .memberships()
            .await?
            .iter()
            .any(|member| member.job_id.is_none() && member.key == group)
    {
        return Ok(Entered::Deadlock);
    }
    let arrival = Arrival {
        run_id: run.state().id,
        job_id: job,
    };
    let members = run.members(&group).await?;
    if grouped.queue == Queue::Single {
        for member in members.iter().filter(|member| !member.running) {
            run.leave(member.id).await?;
            cancel(run, member, &grouped.group, arrival, true, later).await?;
        }
    }
    if grouped.cancel_in_progress
        && let Some(member) = members.iter().find(|member| member.running)
    {
        cancel(run, member, &grouped.group, arrival, false, later).await?;
    }
    let members = run.members(&group).await?;
    let waiting = members.iter().filter(|member| !member.running).count();
    if grouped.queue == Queue::Max && waiting >= MOST_WAITING {
        return Ok(Entered::Full);
    }
    let running = waiting == 0 && !members.iter().any(|member| member.running);
    run.join(&group, job, running).await?;
    Ok(if running {
        Entered::Running
    } else {
        Entered::Waiting
    })
}

pub(super) async fn held(run: &Locked<'_>) -> Result<bool, Error> {
    if !run.state().concurrency {
        return Ok(false);
    }
    Ok(run
        .memberships()
        .await?
        .iter()
        .any(|member| member.job_id.is_none() && !member.running))
}

pub(super) enum Reconciled {
    Unchanged,
    Changed,
    Admit(JobId),
}

pub(super) async fn reconcile(
    run: &mut Locked<'_>,
    jobs: &[JobState],
    later: &mut Later,
) -> Result<Reconciled, Error> {
    let memberships = run.memberships().await?;
    let mut changed = false;
    for member in &memberships {
        let Some(job) = member.job_id else {
            continue;
        };
        match jobs.iter().find(|state| state.id == job) {
            Some(state) if state.status != JobStatus::Completed => {
                if member.running
                    && state.status == JobStatus::Waiting
                    && state.waiting_reason == Some(WaitingReason::Concurrency)
                {
                    run.release_job(job, &Actor::System).await?;
                    return Ok(Reconciled::Changed);
                }
            }
            _ => {
                run.leave(member.id).await?;
                if member.running {
                    changed |= admit_head(run, &member.key, later).await?;
                }
            }
        }
    }
    if memberships
        .iter()
        .any(|member| member.job_id.is_none() && member.running)
    {
        let grouped: HashSet<JobId> = memberships
            .iter()
            .filter_map(|member| member.job_id)
            .collect();
        let held = jobs
            .iter()
            .filter(|state| {
                state.status == JobStatus::Waiting
                    && state.waiting_reason == Some(WaitingReason::Concurrency)
                    && !grouped.contains(&state.id)
            })
            .min_by_key(|state| (state.group, state.strategy.map(|s| s.job_index), state.id));
        if let Some(state) = held {
            return Ok(Reconciled::Admit(state.id));
        }
    }
    Ok(if changed {
        Reconciled::Changed
    } else {
        Reconciled::Unchanged
    })
}

pub(super) async fn depart(run: &Locked<'_>, later: &mut Later) -> Result<(), Error> {
    let memberships = run.memberships().await?;
    for member in &memberships {
        run.leave(member.id).await?;
    }
    for member in memberships.iter().filter(|member| member.running) {
        admit_head(run, &member.key, later).await?;
    }
    Ok(())
}

async fn admit_head(run: &Locked<'_>, group: &str, later: &mut Later) -> Result<bool, Error> {
    let members = run.members(group).await?;
    if members.iter().any(|member| member.running) {
        return Ok(false);
    }
    let Some(head) = members.first() else {
        return Ok(false);
    };
    run.admit_member(head.id).await?;
    if head.run_id == run.state().id {
        Ok(true)
    } else {
        later.push(head.run_id);
        Ok(false)
    }
}

async fn cancel(
    run: &mut Locked<'_>,
    member: &Member,
    group: &str,
    arrival: Arrival,
    waiting: bool,
    later: &mut Later,
) -> Result<(), Error> {
    let event = Event::ConcurrencyCancelled(ConcurrencyCancelled {
        group: group.to_owned(),
        arrival,
    });
    if member.run_id == run.state().id {
        return cancel_member(run, member, event, waiting).await;
    }
    let mut target = bosun_db::runs::lock(run.transaction(), member.run_id)
        .await?
        .ok_or_else(|| Error::Vanished(format!("run {}", member.run_id)))?;
    cancel_member(&mut target, member, event, waiting).await?;
    later.push(member.run_id);
    Ok(())
}

async fn cancel_member(
    target: &mut Locked<'_>,
    member: &Member,
    event: Event,
    waiting: bool,
) -> Result<(), Error> {
    let jobs = target.jobs().await?;
    match member.job_id {
        Some(job) => {
            let Some(state) = jobs.iter().find(|state| state.id == job) else {
                return Ok(());
            };
            if !stoppable(target, state).await? {
                return Ok(());
            }
            target.record(Some(job), &Actor::System, event).await?;
            stop(target, state, StatusReason::Concurrency, &Actor::System).await?;
        }
        None => {
            if target.state().cancel_requested && !waiting {
                return Ok(());
            }
            target.record(None, &Actor::System, event).await?;
            target.request_cancel().await?;
            cancel_jobs(
                target,
                &jobs,
                StatusReason::Concurrency,
                &Actor::System,
                waiting,
            )
            .await?;
        }
    }
    Ok(())
}

async fn stoppable(run: &Locked<'_>, job: &JobState) -> Result<bool, Error> {
    Ok(match job.status {
        JobStatus::Completed => false,
        JobStatus::Pending | JobStatus::Waiting | JobStatus::Queued | JobStatus::Assigned => true,
        JobStatus::InProgress | JobStatus::Paused | JobStatus::Held => match &job.execution {
            Some(execution) => {
                !control::sent(run.transaction(), execution.id, Kind::Cancel).await?
            }
            None => false,
        },
    })
}
