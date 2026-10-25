use std::fmt;
use std::str::FromStr;

use bosun_types::api::Agent;
use bosun_types::event::{Change, Event, RunEvent, StreamEvent};
use bosun_types::id::{AgentId, JobId, ProjectId, RunId};
use sea_orm::prelude::Uuid;
use sea_orm::{ConnectionTrait, FromQueryResult, Value as Parameter};
use serde_json::{Map, Value};

use crate::entity::{run_event, stream_event};
use crate::projects::Reach;
use crate::row::statement;
use crate::{Error, journal, row, views};

pub(crate) const CHANNEL: &str = "bosun_events";

const TABLE: &str = "stream_event";

const HORIZON: &str = "pg_snapshot_xmin(pg_current_snapshot())::text::bigint";

pub(crate) const XACT: &str = "pg_current_xact_id()::text::bigint";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Position {
    pub xact: u64,
    pub seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub journal: Position,
    pub stream: Position,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    Run(RunId),
    Agent(AgentId),
}

pub struct Filter {
    pub reach: Reach,
    pub installation: bool,
    pub projects: Vec<ProjectId>,
    pub runs: Vec<RunId>,
    pub kinds: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivered {
    Run(RunEvent),
    Stream(StreamEvent),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Read {
    pub events: Vec<(Cursor, Delivered)>,
    pub held: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorError(String);

impl fmt::Display for CursorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not the id of an event of this stream, which is four numbers joined by `.`",
            self.0
        )
    }
}

impl std::error::Error for CursorError {}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}.{}.{}",
            self.journal.xact, self.journal.seq, self.stream.xact, self.stream.seq
        )
    }
}

impl FromStr for Cursor {
    type Err = CursorError;

    fn from_str(text: &str) -> Result<Cursor, CursorError> {
        let refused = || CursorError(text.to_owned());
        let numbers = text
            .split('.')
            .map(|part| {
                Some(part)
                    .filter(|part| {
                        (1..=19).contains(&part.len())
                            && part.bytes().all(|byte| byte.is_ascii_digit())
                    })
                    .and_then(|part| part.parse::<u64>().ok())
                    .filter(|number| i64::try_from(*number).is_ok())
                    .ok_or_else(refused)
            })
            .collect::<Result<Vec<u64>, CursorError>>()?;
        let [journal_xact, journal_seq, stream_xact, stream_seq] = numbers.as_slice() else {
            return Err(refused());
        };
        Ok(Cursor {
            journal: Position {
                xact: *journal_xact,
                seq: *journal_seq,
            },
            stream: Position {
                xact: *stream_xact,
                seq: *stream_seq,
            },
        })
    }
}

impl Notice {
    fn payload(self) -> String {
        match self {
            Notice::Run(run) => format!("run {run}"),
            Notice::Agent(agent) => format!("agent {agent}"),
        }
    }
}

pub(crate) fn notice(payload: &str) -> Option<Notice> {
    match payload.split_once(' ')? {
        ("run", run) => run.parse().ok().map(Notice::Run),
        ("agent", agent) => agent.parse().ok().map(Notice::Agent),
        _ => None,
    }
}

pub(crate) fn announcement(run: RunId) -> String {
    Notice::Run(run).payload()
}

pub async fn start<C: ConnectionTrait>(db: &C) -> Result<Cursor, Error> {
    let found = db
        .query_one(statement(&format!("SELECT {HORIZON} AS horizon"), []))
        .await?
        .ok_or_else(|| Error::Row {
            table: TABLE,
            message: "the horizon of the event stream could not be read".to_owned(),
        })?;
    let horizon = row::unsigned(TABLE, found.try_get("", "horizon")?)?;
    let start = Position {
        xact: horizon,
        seq: 0,
    };
    Ok(Cursor {
        journal: start,
        stream: start,
    })
}

pub async fn read<C: ConnectionTrait>(
    db: &C,
    filter: &Filter,
    cursor: Cursor,
    limit: u64,
) -> Result<Read, Error> {
    let wanted = |kinds: &[&str]| -> Option<Vec<String>> {
        if filter.kinds.is_empty() {
            return Some(Vec::new());
        }
        let chosen: Vec<String> = filter
            .kinds
            .iter()
            .filter(|kind| kinds.contains(&kind.as_str()))
            .cloned()
            .collect();
        (!chosen.is_empty()).then_some(chosen)
    };
    let mut held = false;
    let mut journal = Vec::new();
    if let Some(kinds) = wanted(&Event::KINDS) {
        let (found, more) = journal_events(db, filter, &kinds, cursor.journal, limit).await?;
        journal = found;
        held |= more;
    }
    let mut stream = Vec::new();
    if let Some(kinds) = wanted(&Change::KINDS) {
        let (found, more) = stream_events(db, filter, &kinds, cursor.stream, limit).await?;
        stream = found;
        held |= more;
    }
    let mut events = Vec::with_capacity(journal.len() + stream.len());
    let mut position = cursor;
    let mut journal = journal.into_iter().peekable();
    let mut stream = stream.into_iter().peekable();
    loop {
        let from_journal = match (journal.peek(), stream.peek()) {
            (Some((next, _)), Some((other, _))) => next.xact <= other.xact,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => break,
        };
        if from_journal {
            if let Some((at, event)) = journal.next() {
                position.journal = at;
                events.push((position, Delivered::Run(event)));
            }
        } else if let Some((at, event)) = stream.next() {
            position.stream = at;
            events.push((position, Delivered::Stream(event)));
        }
    }
    Ok(Read {
        held: held && events.is_empty(),
        events,
    })
}

struct Sql {
    conditions: Vec<String>,
    values: Vec<Parameter>,
}

impl Sql {
    fn new() -> Sql {
        Sql {
            conditions: Vec::new(),
            values: Vec::new(),
        }
    }

    fn bind(&mut self, value: impl Into<Parameter>) -> String {
        self.values.push(value.into());
        format!("${}", self.values.len())
    }

    fn and(&mut self, condition: String) {
        self.conditions.push(condition);
    }

    fn reach(&mut self, reach: &Reach, project: &str) -> String {
        if reach.everywhere {
            return "true".to_owned();
        }
        let projects: Vec<Uuid> = reach.projects.iter().copied().map(Uuid::from).collect();
        let organisations: Vec<Uuid> = reach
            .organisations
            .iter()
            .copied()
            .map(Uuid::from)
            .collect();
        let projects = self.bind(projects);
        let organisations = self.bind(organisations);
        format!(
            "({project} = ANY({projects}) OR {project} IN \
             (SELECT id FROM project WHERE organisation_id = ANY({organisations})))"
        )
    }

    fn filters(&mut self, filter: &Filter, kinds: &[String], alias: &str, project: &str) {
        if !filter.projects.is_empty() {
            let projects: Vec<Uuid> = filter.projects.iter().copied().map(Uuid::from).collect();
            let projects = self.bind(projects);
            self.and(format!("{project} = ANY({projects})"));
        }
        if !filter.runs.is_empty() {
            let runs: Vec<Uuid> = filter.runs.iter().copied().map(Uuid::from).collect();
            let runs = self.bind(runs);
            self.and(format!("{alias}.run_id = ANY({runs})"));
        }
        if !kinds.is_empty() {
            let kinds = self.bind(kinds.to_vec());
            self.and(format!("{alias}.kind = ANY({kinds})"));
        }
    }

    fn after(&mut self, alias: &str, position: Position) -> Result<(), Error> {
        let xact = self.bind(signed(position.xact)?);
        let seq = self.bind(signed(position.seq)?);
        self.and(format!("({alias}.xact, {alias}.seq) > ({xact}, {seq})"));
        Ok(())
    }

    fn query(&self, select: &str, from: &str, alias: &str, limit: &str) -> String {
        format!(
            "SELECT {select}, ({alias}.xact >= h.horizon) AS held FROM {from} \
             CROSS JOIN (SELECT {HORIZON} AS horizon) h WHERE {} \
             ORDER BY {alias}.xact, {alias}.seq LIMIT {limit}",
            self.conditions.join(" AND ")
        )
    }
}

fn signed(number: u64) -> Result<i64, Error> {
    i64::try_from(number).map_err(|error| Error::Row {
        table: TABLE,
        message: error.to_string(),
    })
}

async fn journal_events<C: ConnectionTrait>(
    db: &C,
    filter: &Filter,
    kinds: &[String],
    after: Position,
    limit: u64,
) -> Result<(Vec<(Position, RunEvent)>, bool), Error> {
    let mut sql = Sql::new();
    sql.after("e", after)?;
    let reach = sql.reach(&filter.reach, "r.project_id");
    sql.and(reach);
    sql.filters(filter, kinds, "e", "r.project_id");
    let limit = sql.bind(signed(limit)?);
    let text = sql.query(
        "e.*",
        "run_event e JOIN run r ON r.id = e.run_id",
        "e",
        &limit,
    );
    let mut events = Vec::new();
    let mut held = false;
    for found in db.query_all(statement(&text, sql.values)).await? {
        if found.try_get::<bool>("", "held")? {
            held = true;
            break;
        }
        let stored = run_event::Model::from_query_result(&found, "")?;
        let position = Position {
            xact: row::unsigned("run_event", stored.xact)?,
            seq: row::unsigned("run_event", stored.seq)?,
        };
        events.push((position, journal::decode(stored)?));
    }
    Ok((events, held))
}

async fn stream_events<C: ConnectionTrait>(
    db: &C,
    filter: &Filter,
    kinds: &[String],
    after: Position,
    limit: u64,
) -> Result<(Vec<(Position, StreamEvent)>, bool), Error> {
    let mut sql = Sql::new();
    sql.after("s", after)?;
    let installation = sql.bind(filter.installation);
    let reach = sql.reach(&filter.reach, "s.project_id");
    sql.and(format!(
        "CASE WHEN s.project_id IS NULL THEN {installation} ELSE {reach} END"
    ));
    sql.filters(filter, kinds, "s", "s.project_id");
    let limit = sql.bind(signed(limit)?);
    let text = sql.query("s.*", "stream_event s", "s", &limit);
    let mut events = Vec::new();
    let mut held = false;
    for found in db.query_all(statement(&text, sql.values)).await? {
        if found.try_get::<bool>("", "held")? {
            held = true;
            break;
        }
        let stored = stream_event::Model::from_query_result(&found, "")?;
        let position = Position {
            xact: row::unsigned(TABLE, stored.xact)?,
            seq: row::unsigned(TABLE, stored.seq)?,
        };
        events.push((position, decode(stored)?));
    }
    Ok((events, held))
}

fn decode(stored: stream_event::Model) -> Result<StreamEvent, Error> {
    let mut tagged = Map::new();
    tagged.insert("kind".to_owned(), Value::String(stored.kind));
    tagged.insert("data".to_owned(), stored.data);
    Ok(StreamEvent {
        seq: row::unsigned(TABLE, stored.seq)?,
        project_id: row::optional_id(TABLE, stored.project_id)?,
        run_id: row::optional_id(TABLE, stored.run_id)?,
        at: stored.at,
        change: row::json(TABLE, Value::Object(tagged))?,
    })
}

async fn append<C: ConnectionTrait>(
    db: &C,
    subject: Uuid,
    project: Option<ProjectId>,
    run: Option<RunId>,
    change: &Change,
    notice: Notice,
) -> Result<(), Error> {
    let mut encoded = serde_json::to_value(change)?;
    let data = encoded
        .get_mut("data")
        .map(Value::take)
        .unwrap_or(Value::Null);
    db.execute(statement(
        &format!(
            "WITH stored AS (INSERT INTO stream_event \
             (subject, project_id, run_id, kind, data, xact) \
             VALUES ($1, $2, $3, $4, $5, greatest({XACT}, coalesce((SELECT xact FROM stream_event \
             WHERE subject = $1 ORDER BY seq DESC LIMIT 1), 0))) RETURNING seq) \
             SELECT stored.seq FROM stored CROSS JOIN (SELECT pg_notify($6, $7)) announced"
        ),
        [
            subject.into(),
            project.map(Uuid::from).into(),
            run.map(Uuid::from).into(),
            change.kind().into(),
            data.into(),
            CHANNEL.into(),
            notice.payload().into(),
        ],
    ))
    .await?;
    Ok(())
}

pub(crate) async fn agent_changed<C: ConnectionTrait>(db: &C, agent: Agent) -> Result<(), Error> {
    let id = agent.id;
    append(
        db,
        id.into(),
        None,
        None,
        &Change::AgentStatus(agent),
        Notice::Agent(id),
    )
    .await
}

pub(crate) async fn queue_changed<C: ConnectionTrait>(db: &C, job: JobId) -> Result<(), Error> {
    let Some(entry) = views::queued_job(db, job).await? else {
        return Ok(());
    };
    let (project, run) = (entry.project_id, entry.job.run_id);
    append(
        db,
        job.into(),
        Some(project),
        Some(run),
        &Change::QueueJob(entry),
        Notice::Run(run),
    )
    .await
}
