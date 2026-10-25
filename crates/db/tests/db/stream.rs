use std::time::Duration;

use bosun_db::agents::{self, Connection};
use bosun_db::entity::{project, run};
use bosun_db::journal::{self, NewEvent};
use bosun_db::projects::Reach;
use bosun_db::stream::{self, Cursor, Delivered, Filter};
use bosun_testkit::TestDatabase;
use bosun_types::actor::Actor;
use bosun_types::api::Capability;
use bosun_types::event::{AttemptCreated, Change, Event, RunEvent};
use bosun_types::id::{AgentId, ProjectId, RunId};
use sea_orm::prelude::Uuid;
use sea_orm::{
    ActiveModelTrait, ActiveValue, ConnectionTrait, DatabaseConnection, EntityTrait,
    TransactionTrait,
};
use tokio::time::{Instant, sleep};

use crate::rows::{self, AGENT, PROJECT, RUN};

const WAIT: Duration = Duration::from_secs(30);

const OTHER_PROJECT: u8 = 0x22;

const OTHER_RUN: u8 = 0x23;

fn project_id(number: u8) -> ProjectId {
    rows::id(number).try_into().expect("a project id")
}

fn run_id(number: u8) -> RunId {
    rows::id(number).try_into().expect("a run id")
}

fn agent_id() -> AgentId {
    rows::id(AGENT).try_into().expect("an agent id")
}

fn attempt_created(run: RunId, number: u32) -> NewEvent {
    NewEvent {
        run_id: run,
        attempt_id: None,
        job_id: None,
        execution_id: None,
        step_id: None,
        actor: Actor::System,
        event: Event::AttemptCreated(AttemptCreated { number }),
    }
}

fn everything() -> Filter {
    Filter {
        reach: Reach {
            everywhere: true,
            organisations: Vec::new(),
            projects: Vec::new(),
        },
        installation: true,
        projects: Vec::new(),
        runs: Vec::new(),
        kinds: Vec::new(),
    }
}

fn viewer_of(project: ProjectId) -> Filter {
    Filter {
        reach: Reach {
            everywhere: false,
            organisations: Vec::new(),
            projects: vec![project],
        },
        installation: false,
        ..everything()
    }
}

async fn seed(db: &DatabaseConnection, label: &str) {
    rows::write(db, label).await;
    db.execute_unprepared(
        "UPDATE bosun.run_event SET xact = 0; UPDATE bosun.stream_event SET xact = 0",
    )
    .await
    .expect("the seeded events written before the stream began");
}

async fn connect(db: &DatabaseConnection) {
    agents::connect(
        db,
        agent_id(),
        Connection {
            replica_id: Uuid::now_v7(),
            version: "0.1.0".to_owned(),
            labels: vec!["self-hosted".to_owned()],
            capabilities: vec![Capability::Shell],
        },
    )
    .await
    .expect("the agent connected");
}

async fn other_run(db: &DatabaseConnection) -> RunId {
    let first = project::Entity::find_by_id(rows::id(PROJECT))
        .one(db)
        .await
        .expect("a query")
        .expect("the project");
    project::ActiveModel {
        id: ActiveValue::Set(rows::id(OTHER_PROJECT)),
        slug: ActiveValue::Set("hidden".to_owned()),
        repository_id: ActiveValue::Set(None),
        created_at: ActiveValue::NotSet,
        ..project::ActiveModel::from(first)
    }
    .insert(db)
    .await
    .expect("another project");
    let seen = run::Entity::find_by_id(rows::id(RUN))
        .one(db)
        .await
        .expect("a query")
        .expect("the run");
    run::ActiveModel {
        id: ActiveValue::Set(rows::id(OTHER_RUN)),
        project_id: ActiveValue::Set(rows::id(OTHER_PROJECT)),
        number: ActiveValue::Set(2),
        global_number: ActiveValue::NotSet,
        created_at: ActiveValue::NotSet,
        ..run::ActiveModel::from(seen)
    }
    .insert(db)
    .await
    .expect("a run of the other project");
    run_id(OTHER_RUN)
}

async fn delivered(
    db: &DatabaseConnection,
    filter: &Filter,
    mut cursor: Cursor,
    count: usize,
) -> Vec<(Cursor, Delivered)> {
    let deadline = Instant::now() + WAIT;
    let mut heard = Vec::new();
    while heard.len() < count {
        assert!(
            Instant::now() < deadline,
            "the stream delivered {} of {count} events within {WAIT:?}: {heard:#?}",
            heard.len()
        );
        let read = stream::read(db, filter, cursor, 2)
            .await
            .expect("a read of the stream");
        if read.events.is_empty() {
            sleep(Duration::from_millis(50)).await;
            continue;
        }
        for (position, event) in read.events {
            assert!(
                position.journal >= cursor.journal && position.stream >= cursor.stream,
                "a position went back from {cursor} to {position}"
            );
            cursor = position;
            heard.push((position, event));
        }
    }
    let rest = stream::read(db, filter, cursor, 100)
        .await
        .expect("a read of the stream");
    assert_eq!(rest.events, Vec::new(), "more events than the test wrote");
    heard
}

fn journal_seqs(events: &[(Cursor, Delivered)]) -> Vec<u64> {
    events
        .iter()
        .filter_map(|(_, event)| match event {
            Delivered::Run(event) => Some(event.seq),
            Delivered::Stream(_) => None,
        })
        .collect()
}

fn agent_states(events: &[(Cursor, Delivered)]) -> Vec<bool> {
    events
        .iter()
        .filter_map(|(_, event)| match event {
            Delivered::Stream(event) => match &event.change {
                Change::AgentStatus(agent) => Some(agent.online),
                Change::QueueJob(_) => None,
            },
            Delivered::Run(_) => None,
        })
        .collect()
}

#[tokio::test]
async fn an_event_follows_the_earlier_events_of_its_run_even_when_its_transaction_began_first() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    seed(db, "stream").await;
    let run = run_id(RUN);
    let start = stream::start(db).await.expect("a start");

    let early = db.begin().await.expect("a transaction");
    early
        .execute_unprepared("SELECT pg_current_xact_id()")
        .await
        .expect("a transaction id");
    let first: RunEvent = journal::append(db, attempt_created(run, 2))
        .await
        .expect("an event");
    let second = journal::append(&early, attempt_created(run, 3))
        .await
        .expect("an event");
    early.commit().await.expect("a commit");

    let heard = delivered(db, &everything(), start, 2).await;
    assert_eq!(journal_seqs(&heard), vec![first.seq, second.seq]);
    assert!(heard[0].0.journal < heard[1].0.journal);
}

#[tokio::test]
async fn a_reader_stops_at_a_transaction_still_running_and_delivers_once_it_ended() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    seed(db, "horizon").await;
    let start = stream::start(db).await.expect("a start");

    let older = db.begin().await.expect("a transaction");
    older
        .execute_unprepared("SELECT pg_current_xact_id()")
        .await
        .expect("a transaction id");
    let event = journal::append(db, attempt_created(run_id(RUN), 2))
        .await
        .expect("an event");
    let read = stream::read(db, &everything(), start, 100)
        .await
        .expect("a read of the stream");
    assert_eq!(read.events, Vec::new());
    assert!(
        read.held,
        "the read does not say that events wait behind the horizon"
    );
    older.commit().await.expect("a commit");

    let heard = delivered(db, &everything(), start, 1).await;
    assert_eq!(journal_seqs(&heard), vec![event.seq]);
}

#[tokio::test]
async fn a_cursor_resumes_both_sequences_after_their_positions() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    seed(db, "resume").await;
    let run = run_id(RUN);
    let start = stream::start(db).await.expect("a start");

    let first = journal::append(db, attempt_created(run, 2))
        .await
        .expect("an event");
    connect(db).await;
    let heard = delivered(db, &everything(), start, 2).await;
    assert_eq!(journal_seqs(&heard), vec![first.seq]);
    assert_eq!(agent_states(&heard), vec![true]);

    let (cursor, _) = heard[1];
    let text = cursor.to_string();
    let parsed: Cursor = text.parse().expect("the id of an event");
    assert_eq!(parsed, cursor);
    assert_eq!(text.split('.').count(), 4);
    for refused in [
        "",
        "1.2.3",
        "1.2.3.4.5",
        "1.2.x.4",
        "-1.2.3.4",
        "+1.+2.+3.+4",
        "1.2.3.+4",
        "1.2.3.99999999999999999999",
    ] {
        assert!(refused.parse::<Cursor>().is_err(), "`{refused}` was taken");
    }

    let second = journal::append(db, attempt_created(run, 3))
        .await
        .expect("an event");
    db.execute_unprepared(&format!(
        "UPDATE bosun.agent SET online = false WHERE id = '{}'",
        rows::id(AGENT)
    ))
    .await
    .expect("the agent offline");
    agents::heartbeat(db, agent_id())
        .await
        .expect("a heartbeat");
    agents::heartbeat(db, agent_id())
        .await
        .expect("a heartbeat");
    let rest = delivered(db, &everything(), parsed, 2).await;
    assert_eq!(journal_seqs(&rest), vec![second.seq]);
    assert_eq!(agent_states(&rest), vec![true]);
    let middle = Cursor {
        journal: heard[0].0.journal,
        stream: start.stream,
    };
    let again = delivered(db, &everything(), middle, 3).await;
    assert_eq!(journal_seqs(&again), vec![second.seq]);
    assert_eq!(agent_states(&again), vec![true, true]);
}

#[tokio::test]
async fn the_reach_and_the_filters_choose_the_events() {
    let database = TestDatabase::migrated().await;
    let db = database.connection();
    seed(db, "filters").await;
    let seen = run_id(RUN);
    let hidden = other_run(db).await;
    let start = stream::start(db).await.expect("a start");
    let ours = journal::append(db, attempt_created(seen, 2))
        .await
        .expect("an event");
    let theirs = journal::append(db, attempt_created(hidden, 2))
        .await
        .expect("an event");
    connect(db).await;
    let all = delivered(db, &everything(), start, 3).await;
    assert_eq!(journal_seqs(&all), vec![ours.seq, theirs.seq]);
    assert_eq!(agent_states(&all), vec![true]);

    let viewer = viewer_of(project_id(PROJECT));
    let seen_by_viewer = delivered(db, &viewer, start, 1).await;
    assert_eq!(journal_seqs(&seen_by_viewer), vec![ours.seq]);
    let nothing = Filter {
        projects: vec![project_id(OTHER_PROJECT)],
        ..viewer_of(project_id(PROJECT))
    };
    delivered(db, &nothing, start, 0).await;
    let nothing = Filter {
        runs: vec![hidden],
        ..viewer_of(project_id(PROJECT))
    };
    delivered(db, &nothing, start, 0).await;
    let agents_only = Filter {
        reach: Reach {
            everywhere: false,
            organisations: Vec::new(),
            projects: Vec::new(),
        },
        ..everything()
    };
    assert_eq!(
        agent_states(&delivered(db, &agents_only, start, 1).await),
        vec![true]
    );
    let by_project = Filter {
        projects: vec![project_id(OTHER_PROJECT)],
        ..everything()
    };
    assert_eq!(
        journal_seqs(&delivered(db, &by_project, start, 1).await),
        vec![theirs.seq]
    );
    let by_run = Filter {
        runs: vec![seen],
        ..everything()
    };
    assert_eq!(
        journal_seqs(&delivered(db, &by_run, start, 1).await),
        vec![ours.seq]
    );
    let by_kind = Filter {
        kinds: vec!["agent.status".to_owned()],
        ..everything()
    };
    assert_eq!(
        agent_states(&delivered(db, &by_kind, start, 1).await),
        vec![true]
    );
    let by_journal_kind = Filter {
        kinds: vec!["attempt.created".to_owned(), "run.status".to_owned()],
        ..everything()
    };
    assert_eq!(
        journal_seqs(&delivered(db, &by_journal_kind, start, 2).await),
        vec![ours.seq, theirs.seq]
    );
    let by_organisation = Filter {
        reach: Reach {
            everywhere: false,
            organisations: vec![rows::id(rows::ORGANISATION).try_into().expect("an id")],
            projects: Vec::new(),
        },
        installation: false,
        ..everything()
    };
    assert_eq!(
        journal_seqs(&delivered(db, &by_organisation, start, 2).await),
        vec![ours.seq, theirs.seq]
    );
}
