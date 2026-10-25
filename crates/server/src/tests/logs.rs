use std::ops::RangeBounds;
use std::sync::Arc;
use std::time::Duration;

use bosun_blob::{Directory, Key, Object, Store};
use bosun_db::entity::step;
use bosun_db::logs::{Chunk, Compacted, Ingested};
use bosun_db::notify::Listener;
use bosun_types::id::{ExecutionId, StepId};
use bosun_types::status::Conclusion;
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, EntityTrait};
use tempfile::TempDir;
use tokio::io::AsyncRead;
use tokio::sync::Notify;
use tokio::time::timeout;

use super::{World, template, workflow};
use crate::agents::progress::{self, StepEnd};
use crate::agents::{self, Replica};
use crate::config::{Agents, Queue};
use crate::logs::compaction::compact;
use crate::logs::reader::{Line, Read, Reader};
use crate::logs::{Followers, LIMIT, notice};
use crate::queue::Dispatcher;

const WAIT: Duration = Duration::from_secs(30);

const HOUR: Duration = Duration::from_secs(3600);

struct Logged {
    world: World,
    execution: ExecutionId,
    steps: Vec<StepId>,
    store: Arc<Directory>,
    _directory: TempDir,
}

impl Logged {
    async fn new() -> Logged {
        let world = World::new().await;
        let run = world
            .dispatch(&workflow(vec![template("build", &[], 2)]))
            .await;
        let (build, _) = world.project(run).await.job("build");
        let execution = world.assign(run, build).await;
        progress::accepted(world.db(), world.agent(), execution)
            .await
            .expect("an acceptance");
        progress::step_started(world.db(), world.agent(), execution, 0, None)
            .await
            .expect("a started step");
        let projection = world.project(run).await;
        let mut steps: Vec<(u32, StepId)> = projection
            .steps
            .iter()
            .filter(|(_, step)| step.execution == execution)
            .map(|(id, step)| (step.index, *id))
            .collect();
        steps.sort();
        let directory = tempfile::tempdir().expect("a directory for the store");
        let store = Directory::open(directory.path().join("store"))
            .await
            .expect("a store");
        Logged {
            world,
            execution,
            steps: steps.into_iter().map(|(_, id)| id).collect(),
            store: Arc::new(store),
            _directory: directory,
        }
    }

    async fn ingest(&self, step: u32, seq: u64, lines: &[u8]) -> Ingested {
        bosun_db::logs::ingest(
            self.world.db(),
            &Chunk {
                agent: self.world.agent().id,
                execution: self.execution,
                step,
                seq,
                lines,
            },
            LIMIT,
        )
        .await
        .expect("an ingestion")
    }

    async fn finish(&self) {
        let (db, agent) = (self.world.db(), self.world.agent());
        progress::step_finished(db, agent, self.execution, end(0))
            .await
            .expect("a finished step");
        progress::step_started(db, agent, self.execution, 1, None)
            .await
            .expect("a started step");
        progress::step_finished(db, agent, self.execution, end(1))
            .await
            .expect("a finished step");
        progress::job_finished(db, agent, self.execution, Conclusion::Success)
            .await
            .expect("a finished job");
    }

    async fn compact(&self) -> Compacted {
        compact(self.world.db(), self.store.as_ref(), self.execution)
            .await
            .expect("a compaction")
    }

    fn reader(&self, step: usize, offset: u64) -> Reader {
        Reader::new(
            self.world.db().clone(),
            Arc::clone(&self.store),
            self.execution,
            self.steps[step],
            offset,
        )
    }
}

struct Gated {
    store: Arc<Directory>,
    reached: Notify,
    open: Notify,
}

impl Store for Gated {
    async fn put<R>(&self, key: &Key, body: R) -> Result<(), bosun_blob::Error>
    where
        R: AsyncRead + Send + Unpin,
    {
        self.reached.notify_one();
        self.open.notified().await;
        self.store.put(key, body).await
    }

    async fn get<B>(&self, key: &Key, range: B) -> Result<Object, bosun_blob::Error>
    where
        B: RangeBounds<u64> + Send,
    {
        self.store.get(key, range).await
    }

    async fn delete(&self, key: &Key) -> Result<(), bosun_blob::Error> {
        self.store.delete(key).await
    }

    async fn list(&self, prefix: &str) -> Result<Vec<Key>, bosun_blob::Error> {
        self.store.list(prefix).await
    }
}

fn end(index: u32) -> StepEnd {
    StepEnd {
        index,
        at: None,
        outcome: Conclusion::Success,
        conclusion: Conclusion::Success,
    }
}

fn text(from: u64, to: u64) -> Vec<u8> {
    (from..=to)
        .flat_map(|number| format!("2026-10-25T10:00:00.000000000Z line {number}\n").into_bytes())
        .collect()
}

fn expected(from: u64, to: u64) -> Vec<Line> {
    (from..=to)
        .map(|number| Line {
            number,
            text: format!("2026-10-25T10:00:00.000000000Z line {number}").into_bytes(),
        })
        .collect()
}

async fn drain(reader: &mut Reader) -> (Vec<Line>, Read) {
    let mut lines = Vec::new();
    loop {
        match reader.read().await.expect("a read") {
            Read::Lines(read) => lines.extend(read),
            ended => return (lines, ended),
        }
    }
}

#[tokio::test]
async fn a_reader_in_the_middle_of_a_step_delivers_every_line_once_across_its_compaction() {
    let logged = Logged::new().await;
    for (seq, from) in (1..=10).zip((1..).step_by(100)) {
        assert_eq!(
            logged.ingest(0, seq, &text(from, from + 99)).await,
            Ingested::Stored
        );
    }
    let mut follower = logged.reader(0, 0);
    let (before, waiting) = drain(&mut follower).await;
    assert_eq!(before, expected(1, 1000));
    assert_eq!(waiting, Read::Waiting, "a running step's log ended");

    for (seq, from) in (11..=15).zip((1001..).step_by(100)) {
        assert_eq!(
            logged.ingest(0, seq, &text(from, from + 99)).await,
            Ingested::Stored
        );
    }
    logged.finish().await;
    assert_eq!(logged.compact().await, Compacted::Done);
    let (after, ended) = drain(&mut follower).await;
    assert_eq!(after, expected(1001, 1500));
    assert_eq!(ended, Read::End(1500));

    let (all, ended) = drain(&mut logged.reader(0, 0)).await;
    assert_eq!(all, expected(1, 1500));
    assert_eq!(ended, Read::End(1500));
    let (rest, _) = drain(&mut logged.reader(0, 1234)).await;
    assert_eq!(rest, expected(1235, 1500));
    let (nothing, ended) = drain(&mut logged.reader(1, 0)).await;
    assert_eq!((nothing, ended), (Vec::new(), Read::End(0)));

    assert_eq!(logged.ingest(0, 16, b"late\n").await, Ingested::Closed);
    assert_eq!(logged.compact().await, Compacted::Already);
}

#[tokio::test]
async fn a_completed_step_reads_the_same_before_and_after_compaction() {
    let logged = Logged::new().await;
    assert_eq!(logged.ingest(0, 1, &text(1, 3)).await, Ingested::Stored);
    assert_eq!(logged.ingest(0, 2, &text(4, 5)).await, Ingested::Stored);
    logged.finish().await;
    let (before, ended_before) = drain(&mut logged.reader(0, 0)).await;
    assert_eq!(logged.compact().await, Compacted::Done);
    let (after, ended_after) = drain(&mut logged.reader(0, 0)).await;
    assert_eq!(before, expected(1, 5));
    assert_eq!(after, before);
    assert_eq!((ended_before, ended_after), (Read::End(5), Read::End(5)));
}

#[tokio::test]
async fn a_step_whose_output_passed_the_limit_ends_with_a_notice_before_and_after_compaction() {
    let logged = Logged::new().await;
    logged
        .world
        .db()
        .execute_unprepared(&format!(
            "UPDATE bosun.execution SET log_bytes = {} WHERE id = '{}'",
            LIMIT - 64,
            logged.execution
        ))
        .await
        .expect("a log near its limit");
    let first = text(1, 1);
    assert_eq!(logged.ingest(0, 1, &first).await, Ingested::Stored);
    assert_eq!(
        logged.ingest(0, 2, &text(2, 3)).await,
        Ingested::Dropped { first: true }
    );
    assert_eq!(
        logged.ingest(0, 3, b"x\n").await,
        Ingested::Dropped { first: false }
    );
    let (live, waiting) = drain(&mut logged.reader(0, 0)).await;
    assert_eq!(live, expected(1, 1), "a running step showed its notice");
    assert_eq!(waiting, Read::Waiting);

    logged.finish().await;
    let completed_at = step::Entity::find_by_id(logged.steps[0].as_uuid())
        .one(logged.world.db())
        .await
        .expect("a step")
        .and_then(|step| step.completed_at)
        .expect("a completed step");
    let dropped = text(2, 3).len() as u64 + 2;
    let (before, ended_before) = drain(&mut logged.reader(0, 0)).await;
    assert_eq!(logged.compact().await, Compacted::Done);
    let (after, ended_after) = drain(&mut logged.reader(0, 0)).await;
    let mut lines = expected(1, 1);
    lines.push(Line {
        number: 2,
        text: notice(completed_at, dropped),
    });
    assert_eq!(before, lines);
    assert_eq!(after, before);
    assert_eq!((ended_before, ended_after), (Read::End(2), Read::End(2)));
    let shown = String::from_utf8_lossy(&lines[1].text).into_owned();
    assert!(
        shown.ends_with(&format!(
            " {dropped} bytes of output were dropped because the log of the execution reached \
             its limit of 128 MiB"
        )),
        "{shown}"
    );
}

#[tokio::test]
async fn a_compaction_of_an_execution_another_compactor_holds_is_skipped_and_the_object_reads_back_whole()
 {
    let logged = Logged::new().await;
    for seq in 1..=300 {
        let from = seq * 10 - 9;
        assert_eq!(
            logged.ingest(0, seq, &text(from, from + 9)).await,
            Ingested::Stored
        );
    }
    logged.finish().await;
    let gated = Arc::new(Gated {
        store: Arc::clone(&logged.store),
        reached: Notify::new(),
        open: Notify::new(),
    });
    let first = tokio::spawn({
        let db = logged.world.db().clone();
        let gated = Arc::clone(&gated);
        let execution = logged.execution;
        async move {
            compact(&db, gated.as_ref(), execution)
                .await
                .expect("the first compaction")
        }
    });
    timeout(WAIT, gated.reached.notified())
        .await
        .expect("the first compaction read every chunk and began to write");

    assert_eq!(logged.compact().await, Compacted::Busy);
    assert_eq!(
        logged.store.list("logs/").await.expect("a listing"),
        [],
        "a skipped compaction wrote an object"
    );
    assert_eq!(
        logged.ingest(0, 301, &text(3001, 3005)).await,
        Ingested::Stored
    );
    gated.open.notify_one();
    assert_eq!(
        timeout(WAIT, first)
            .await
            .expect("the first compaction ended")
            .expect("the first compaction did not panic"),
        Compacted::Changed
    );
    assert_eq!(
        logged.store.list("logs/").await.expect("a listing"),
        [],
        "a compaction that recorded nothing left its object"
    );

    assert_eq!(logged.compact().await, Compacted::Done);
    assert_eq!(logged.compact().await, Compacted::Already);
    let (all, ended) = drain(&mut logged.reader(0, 0)).await;
    assert_eq!(all.len(), 3005);
    assert_eq!(all, expected(1, 3005));
    assert_eq!(ended, Read::End(3005));
    let keys: Vec<String> = logged
        .store
        .list("logs/")
        .await
        .expect("a listing")
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(keys.len(), 1, "{keys:?}");
    assert!(
        keys[0].ends_with(&format!(
            "/{}/301/{}.log.zst",
            logged.execution, logged.steps[0]
        )),
        "{keys:?}"
    );
}

#[tokio::test]
async fn a_stored_chunk_wakes_the_followers_of_its_execution_through_bosun_log() {
    let logged = Logged::new().await;
    let db = logged.world.db().clone();
    let replica = Replica::new();
    let listener = Listener::connect(&logged.world.database.url(), replica.id())
        .await
        .expect("a listener");
    let dispatcher = Dispatcher::new(
        db.clone(),
        Arc::clone(&replica),
        Queue {
            lease: HOUR,
            max_queue_time: HOUR,
        },
    );
    let followers = Arc::new(Followers::default());
    let mut following = followers.subscribe(logged.execution);
    let other: ExecutionId = Uuid::now_v7().try_into().expect("an execution id");
    let mut elsewhere = followers.subscribe(other);
    let _tasks = agents::start(
        db,
        replica,
        listener,
        Agents {
            heartbeat_interval: HOUR,
            offline_after: HOUR,
            grace: HOUR,
        },
        dispatcher,
        Arc::clone(&followers),
    );

    assert_eq!(logged.ingest(0, 1, b"a\n").await, Ingested::Stored);
    timeout(WAIT, following.announced())
        .await
        .expect("the follower of the execution was woken by the announcement");
    assert!(
        timeout(Duration::from_millis(300), elsewhere.announced())
            .await
            .is_err(),
        "a follower of another execution was woken"
    );
}
