use std::collections::HashSet;
use std::future::Future;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::{self, JoinHandle};

use bosun_db::Error;
use sea_orm::sqlx::Postgres;
use sea_orm::sqlx::pool::PoolConnection;
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbErr, RuntimeErr, Statement,
};
use tokio::runtime::Runtime;
use tokio::sync::{Mutex, OnceCell, oneshot};
use url::Url;
use uuid::Uuid;

const VARIABLE: &str = "BOSUN_TEST_DATABASE_URL";
const PREFIX: &str = "bosun_test_";
const TEMPLATE: &str = "template";

pub struct TestDatabase {
    name: String,
    kind: Kind,
    connection: Option<DatabaseConnection>,
}

#[derive(Clone, Copy)]
enum Kind {
    Migrated,
    Empty,
}

impl TestDatabase {
    pub async fn migrated() -> TestDatabase {
        TestDatabase::open(Kind::Migrated).await
    }

    pub async fn empty() -> TestDatabase {
        TestDatabase::open(Kind::Empty).await
    }

    pub fn connection(&self) -> &DatabaseConnection {
        self.connection
            .as_ref()
            .expect("the connection lives as long as the database")
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn url(&self) -> String {
        admin().url(&self.name)
    }

    async fn open(kind: Kind) -> TestDatabase {
        let admin = admin();
        let (created, receiver) = oneshot::channel();
        admin.run(move |admin| async move {
            let _ = created.send(admin.create(kind).await);
        });
        let name = receiver
            .await
            .unwrap_or_else(|_| panic!("bosun-testkit stopped while creating a database"))
            .unwrap_or_else(|error| panic!("bosun-testkit could not create a database: {error}"));
        let mut database = TestDatabase {
            name,
            kind,
            connection: None,
        };
        let connection = bosun_db::connect(&admin.url(&database.name), 4)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "bosun-testkit could not connect to {}: {error}",
                    database.name
                )
            });
        database.connection = Some(connection);
        database
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        drop(self.connection.take());
        let admin = admin();
        let name = std::mem::take(&mut self.name);
        let kind = self.kind;
        let released = admin
            .run(move |admin| async move {
                let released = admin.release(&name, kind).await;
                released.map_err(|error| (name, error))
            })
            .join();
        if let Ok(Err((name, error))) = released {
            eprintln!("bosun-testkit could not drop {name}, the next run sweeps it: {error}");
        }
    }
}

struct Admin {
    runtime: Runtime,
    server: Url,
    owner: String,
    counter: AtomicU64,
    session: OnceCell<Session>,
    template: Mutex<Template>,
}

struct Session {
    pool: DatabaseConnection,
    _alive: PoolConnection<Postgres>,
}

#[derive(Default)]
struct Template {
    built: bool,
    users: usize,
}

fn admin() -> &'static Admin {
    static ADMIN: OnceLock<Admin> = OnceLock::new();
    ADMIN.get_or_init(Admin::new)
}

impl Admin {
    fn new() -> Admin {
        let url = std::env::var(VARIABLE).unwrap_or_else(|_| {
            panic!(
                "{VARIABLE} is not set: tests create their databases on that Postgres server, \
                 for example postgres://localhost/postgres"
            )
        });
        let server =
            Url::parse(&url).unwrap_or_else(|error| panic!("{VARIABLE} is not a URL: {error}"));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("bosun-testkit")
            .enable_all()
            .build()
            .unwrap_or_else(|error| panic!("bosun-testkit could not start its runtime: {error}"));
        Admin {
            runtime,
            server,
            owner: Uuid::now_v7().simple().to_string(),
            counter: AtomicU64::new(0),
            session: OnceCell::new(),
            template: Mutex::new(Template::default()),
        }
    }

    fn run<T, F, W>(&'static self, work: W) -> JoinHandle<T>
    where
        T: Send + 'static,
        F: Future<Output = T>,
        W: FnOnce(&'static Admin) -> F + Send + 'static,
    {
        thread::Builder::new()
            .name("bosun-testkit".to_owned())
            .spawn(move || self.runtime.block_on(work(self)))
            .unwrap_or_else(|error| panic!("bosun-testkit could not start a thread: {error}"))
    }

    fn url(&self, database: &str) -> String {
        let mut url = self.server.clone();
        url.set_path(&format!("/{database}"));
        url.to_string()
    }

    fn template(&self) -> String {
        format!("{PREFIX}{}_{TEMPLATE}", self.owner)
    }

    async fn session(&self) -> Result<&Session, Error> {
        self.session
            .get_or_try_init(|| async {
                let mut url = self.server.clone();
                url.query_pairs_mut()
                    .append_pair("application_name", &format!("{PREFIX}{}", self.owner));
                let mut options = ConnectOptions::new(url.to_string());
                options.max_connections(4);
                let pool = Database::connect(options).await?;
                let alive = pool
                    .get_postgres_connection_pool()
                    .acquire()
                    .await
                    .map_err(|error| DbErr::Conn(RuntimeErr::SqlxError(error)))?;
                sweep(&pool, &self.owner).await?;
                Ok(Session {
                    pool,
                    _alive: alive,
                })
            })
            .await
    }

    async fn create(&self, kind: Kind) -> Result<String, Error> {
        let session = self.session().await?;
        let number = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let name = format!("{PREFIX}{}_{number}", self.owner);
        let template = match kind {
            Kind::Empty => "template0".to_owned(),
            Kind::Migrated => self.enter(session).await?,
        };
        let created = session
            .pool
            .execute_unprepared(&format!(
                r#"CREATE DATABASE "{name}" TEMPLATE "{template}""#
            ))
            .await;
        match created {
            Ok(_) => Ok(name),
            Err(error) => {
                if let Kind::Migrated = kind {
                    let _ = self.leave(session).await;
                }
                Err(error.into())
            }
        }
    }

    async fn release(&self, name: &str, kind: Kind) -> Result<(), Error> {
        let session = self.session().await?;
        let dropped = session.pool.execute_unprepared(&drop_database(name)).await;
        if let Kind::Migrated = kind {
            self.leave(session).await?;
        }
        dropped?;
        Ok(())
    }

    async fn enter(&self, session: &Session) -> Result<String, Error> {
        let mut template = self.template.lock().await;
        if !template.built {
            self.build(session).await?;
            template.built = true;
        }
        template.users += 1;
        Ok(self.template())
    }

    async fn leave(&self, session: &Session) -> Result<(), Error> {
        let mut template = self.template.lock().await;
        template.users -= 1;
        if template.users == 0 && template.built {
            session
                .pool
                .execute_unprepared(&drop_database(&self.template()))
                .await?;
            template.built = false;
        }
        Ok(())
    }

    async fn build(&self, session: &Session) -> Result<(), Error> {
        let name = self.template();
        session
            .pool
            .execute_unprepared(&drop_database(&name))
            .await?;
        session
            .pool
            .execute_unprepared(&format!(r#"CREATE DATABASE "{name}" TEMPLATE template0"#))
            .await?;
        let migrated = migrate(&self.url(&name)).await;
        if migrated.is_err() {
            let _ = session.pool.execute_unprepared(&drop_database(&name)).await;
        }
        migrated
    }
}

async fn migrate(url: &str) -> Result<(), Error> {
    let db = bosun_db::connect(url, 4).await?;
    let migrated = bosun_db::migrate(&db).await;
    db.close().await?;
    migrated.map(|_| ())
}

async fn sweep(pool: &DatabaseConnection, owner: &str) -> Result<(), Error> {
    let databases = names(pool, "SELECT datname AS name FROM pg_database").await?;
    let alive: HashSet<String> = names(
        pool,
        "SELECT application_name AS name FROM pg_stat_activity",
    )
    .await?
    .into_iter()
    .filter(|name| name.strip_prefix(PREFIX).is_some_and(is_owner))
    .collect();
    for database in databases {
        let Some(other) = owner_of(&database) else {
            continue;
        };
        if other != owner && !alive.contains(&format!("{PREFIX}{other}")) {
            let _ = pool.execute_unprepared(&drop_database(&database)).await;
        }
    }
    Ok(())
}

async fn names(pool: &DatabaseConnection, sql: &str) -> Result<Vec<String>, Error> {
    let rows = pool
        .query_all(Statement::from_string(pool.get_database_backend(), sql))
        .await?;
    let mut names = Vec::with_capacity(rows.len());
    for row in rows {
        if let Some(name) = row.try_get::<Option<String>>("", "name")? {
            names.push(name);
        }
    }
    Ok(names)
}

fn owner_of(database: &str) -> Option<&str> {
    let (owner, suffix) = database.strip_prefix(PREFIX)?.split_once('_')?;
    let numbered = !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit());
    (is_owner(owner) && (numbered || suffix == TEMPLATE)).then_some(owner)
}

fn is_owner(token: &str) -> bool {
    token.len() == 32
        && token
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn drop_database(database: &str) -> String {
    format!(r#"DROP DATABASE IF EXISTS "{database}" WITH (FORCE)"#)
}
