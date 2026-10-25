mod access;
mod actions;
mod address;
mod agents;
mod aliases;
mod auth;
mod bindings;
mod check;
mod clock;
mod commands;
mod config;
mod digest;
mod dispatch;
mod envelope;
mod error;
mod events;
mod forge;
mod forge_events;
mod grpc;
mod health;
mod hooks;
mod idempotency;
mod install;
mod interface;
mod keys;
mod lint;
mod logs;
mod me;
mod names;
mod oidc;
mod openapi;
mod organisations;
mod outbox;
mod owner;
mod password;
mod planner;
mod pools;
mod projects;
mod queue;
mod reply;
mod request;
mod retry;
mod roles;
mod runs;
mod runtime;
mod scope;
mod secrets;
mod sessions;
mod settings;
mod shutdown;
mod sign_in;
mod site;
mod slug;
mod store;
mod summaries;
mod teams;
#[cfg(test)]
mod tests;
mod token;
mod tokens;
mod tools;
mod trace;
mod users;
mod variables;
mod workflows;

use std::future::Future;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::extract::Request;
use axum::http::{Method, Uri};
use axum::middleware;
use axum::routing::get;
use bosun_proto::agent::v1::agent_server::AgentServer;
use sea_orm::DatabaseConnection;
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tonic::service::Routes;
use tower::{ServiceExt, service_fn};
use tracing::{info, warn};

pub use check::check;
pub use config::Config;
pub use error::Error;

#[derive(Clone)]
struct AppState {
    db: DatabaseConnection,
    site: site::Site,
    store: Arc<store::Backend>,
    followers: Arc<logs::Followers>,
    events: Arc<events::Hub>,
    shutdown: shutdown::Shutdown,
    agent_binaries: Arc<Path>,
    clock: clock::Clock,
    sign_in: Arc<sign_in::Settings>,
    forge: Option<Arc<forge::Forge>>,
    parses: Arc<forge_events::Parses>,
    keys: Arc<keys::Keyring>,
    tools: Arc<tools::Tools>,
    oidc: Arc<oidc::Providers>,
}

pub struct Rewrapped {
    pub secrets: u64,
    pub kek_id: String,
}

pub async fn serve(
    config: Config,
    stop: impl Future<Output = String> + Send + 'static,
) -> Result<(), Error> {
    info!(version = env!("CARGO_PKG_VERSION"), "starting");
    let interface = interface::Interface::embedded();
    if interface.is_empty() {
        warn!(
            "this build carries no web interface, so the server answers its pages with how to \
             build it; the API, the CLI and the agents work without it"
        );
    }
    let keys = Arc::new(keys::load(
        &config.keys,
        &config.data_dir,
        keys::Generation::Allowed,
    )?);
    let forge = config
        .github
        .as_ref()
        .map(forge::load)
        .transpose()?
        .map(Arc::new);
    let oidc = Arc::new(oidc::Providers::load(&config.oidc, &config.site)?);
    let store = Arc::new(store::open(&config.store).await?);
    let db = connect(&config).await?;
    bosun_db::migrate(&db).await.map_err(Error::Migrate)?;
    let sign_in = Arc::new(sign_in::Settings::new(
        config.trusted_proxies.clone(),
        config.failed_sign_ins,
        config.data_dir.clone(),
    ));
    match &config.bootstrap_admin {
        Some(login) => owner::bootstrap(&db, login).await?,
        None => owner::first_start(&db, &config.data_dir, &sign_in.hasher).await?,
    }
    let clock = clock::Clock::system();
    let replica = agents::Replica::new();
    let notifications = bosun_db::notify::Listener::connect(&config.database.url, replica.id())
        .await
        .map_err(Error::Notifications)?;
    let tools = Arc::new(tools::Tools::new(
        db.clone(),
        Arc::clone(&store),
        config.tools.clone(),
    )?);
    let actions = Arc::new(actions::Actions::new(
        db.clone(),
        Arc::clone(&store),
        forge.clone(),
        config.tools.runtimes.clone(),
    ));
    let dispatcher = queue::Dispatcher::new(
        db.clone(),
        Arc::clone(&replica),
        config.queue,
        config.site.clone(),
        Arc::clone(&keys),
        actions,
    );
    let followers = Arc::new(logs::Followers::default());
    let deliveries = Arc::new(Notify::new());
    let reports = Arc::new(Notify::new());
    let events = Arc::new(events::Hub::default());
    let parses = Arc::new(forge_events::Parses::default());
    let mut tasks = agents::start(
        db.clone(),
        Arc::clone(&replica),
        notifications,
        config.agents,
        Arc::clone(&dispatcher),
        agents::Listeners {
            followers: Arc::clone(&followers),
            deliveries: Arc::clone(&deliveries),
            outbox: Arc::clone(&reports),
            events: Arc::clone(&events),
        },
    );
    if let Some(forge) = &forge {
        tasks.spawn(hooks::work(
            db.clone(),
            Arc::clone(forge),
            deliveries,
            Arc::clone(&parses),
            config.site.clone(),
        ));
        tasks.spawn(outbox::work(
            outbox::Outbox {
                db: db.clone(),
                store: Arc::clone(&store),
                forge: Arc::clone(forge),
                site: config.site.clone(),
                clock: clock.clone(),
            },
            reports,
        ));
    }
    tasks.spawn(queue::sweep(Arc::clone(&dispatcher)));
    tasks.spawn(logs::sweep(db.clone(), Arc::clone(&store)));
    tasks.spawn(sessions::sweep(db.clone(), clock.clone()));
    let connector = agents::Connector::new(
        db.clone(),
        Arc::clone(&store),
        Arc::clone(&replica),
        config.agents,
        dispatcher,
    );
    let listen = config.listen;
    let listener = TcpListener::bind(listen)
        .await
        .map_err(|source| Error::Listen {
            address: listen,
            source,
        })?;
    let address = listener.local_addr().map_err(|source| Error::Listen {
        address: listen,
        source,
    })?;
    info!(%address, replica_id = %replica.id(), "listening");
    let (closer, shutdown) = shutdown::channel();
    let state = AppState {
        db,
        site: config.site,
        store,
        followers,
        events,
        shutdown: shutdown.clone(),
        agent_binaries: Arc::from(config.agent_binaries.as_path()),
        clock,
        sign_in,
        forge,
        parses,
        keys,
        tools,
        oidc,
    };
    axum::serve(
        listener,
        app(state, interface, connector, shutdown)
            .into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let reason = stop.await;
        info!(%reason, "shutting down");
        closer.close(format!("the server is shutting down: {reason}"));
    })
    .await
    .map_err(Error::Serve)?;
    tasks.stop().await;
    info!("stopped");
    Ok(())
}

pub async fn migrate(config: &Config) -> Result<(), Error> {
    let db = connect(config).await?;
    bosun_db::migrate(&db).await.map_err(Error::Migrate)?;
    Ok(())
}

pub async fn create_owner(config: &Config, login: &str) -> Result<String, Error> {
    let db = connect(config).await?;
    bosun_db::migrate(&db).await.map_err(Error::Migrate)?;
    owner::create(&db, login).await
}

pub async fn rewrap(config: &Config) -> Result<Rewrapped, Error> {
    let keys = keys::load(&config.keys, &config.data_dir, keys::Generation::Refused)?;
    let db = connect(config).await?;
    bosun_db::migrate(&db).await.map_err(Error::Migrate)?;
    let secrets = secrets::rewrap(&db, &keys).await?;
    Ok(Rewrapped {
        secrets,
        kek_id: keys.current().to_owned(),
    })
}

pub fn openapi() -> String {
    openapi::render(&api().document())
}

async fn connect(config: &Config) -> Result<DatabaseConnection, Error> {
    bosun_db::connect(&config.database.url, config.database.max_connections)
        .await
        .map_err(Error::Connect)
}

fn api() -> openapi::Api {
    openapi::Api::interim()
        .get("/api/v1/me", me::show)
        .merge(sign_in::routes())
        .merge(oidc::routes())
        .merge(organisations::routes())
        .merge(projects::routes())
        .merge(workflows::routes())
        .merge(lint::routes())
        .merge(commands::routes())
        .merge(runs::routes())
        .merge(logs::routes())
        .merge(summaries::routes())
        .merge(agents::routes())
        .merge(pools::routes())
        .merge(aliases::routes())
        .merge(users::routes())
        .merge(teams::routes())
        .merge(roles::routes())
        .merge(bindings::routes())
        .merge(tokens::routes())
        .merge(events::routes())
        .merge(queue::routes())
        .merge(secrets::routes())
        .merge(variables::routes())
}

fn app(
    state: AppState,
    interface: interface::Interface,
    connector: agents::Connector,
    shutdown: shutdown::Shutdown,
) -> Router {
    let http = Router::new()
        .route("/healthz", get(health::alive))
        .route("/readyz", get(health::ready))
        .merge(api().into_router(&state))
        .merge(install::routes())
        .merge(runtime::routes())
        .merge(hooks::routes())
        .fallback(move |method: Method, uri: Uri| interface::answer(interface, method, uri))
        .method_not_allowed_fallback(envelope::no_route)
        .with_state(state);
    let agents = AgentServer::new(connector).max_decoding_message_size(agents::MESSAGE_LIMIT);
    let grpc = grpc::routes(Routes::new(agents), shutdown);
    Router::new()
        .fallback_service(service_fn(move |request: Request| {
            let target = if grpc::is_grpc(&request) {
                grpc.clone()
            } else {
                http.clone()
            };
            target.oneshot(request)
        }))
        .layer(middleware::from_fn(trace::request))
}
