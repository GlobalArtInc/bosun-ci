mod agents;
mod auth;
mod commands;
mod config;
mod dispatch;
mod envelope;
mod error;
mod grpc;
mod health;
mod idempotency;
mod keys;
mod me;
mod organisations;
mod owner;
mod planner;
mod projects;
mod request;
mod shutdown;
mod slug;
#[cfg(test)]
mod tests;
mod token;
mod trace;
mod workflows;

use std::future::Future;
use std::sync::Arc;

use axum::Router;
use axum::extract::Request;
use axum::middleware;
use axum::routing::get;
use bosun_proto::agent::v1::agent_server::AgentServer;
use sea_orm::DatabaseConnection;
use tokio::net::TcpListener;
use tonic::service::Routes;
use tower::{ServiceExt, service_fn};
use tracing::info;

pub use config::Config;
pub use error::Error;

#[derive(Clone)]
struct AppState {
    db: DatabaseConnection,
}

pub async fn serve(
    config: Config,
    stop: impl Future<Output = String> + Send + 'static,
) -> Result<(), Error> {
    info!(version = env!("CARGO_PKG_VERSION"), "starting");
    keys::verify(&config.keys)?;
    let db = connect(&config).await?;
    bosun_db::migrate(&db).await.map_err(Error::Migrate)?;
    if let Some(login) = &config.bootstrap_admin {
        owner::bootstrap(&db, login).await?;
    }
    let replica = agents::Replica::new();
    let notifications = bosun_db::control::Listener::connect(&config.database.url, replica.id())
        .await
        .map_err(Error::Notifications)?;
    let tasks = agents::start(
        db.clone(),
        Arc::clone(&replica),
        notifications,
        config.agents,
    );
    let connector = agents::Connector::new(db.clone(), Arc::clone(&replica), config.agents);
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
    axum::serve(listener, app(AppState { db }, connector, shutdown))
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

async fn connect(config: &Config) -> Result<DatabaseConnection, Error> {
    bosun_db::connect(&config.database.url, config.database.max_connections)
        .await
        .map_err(Error::Connect)
}

fn app(state: AppState, connector: agents::Connector, shutdown: shutdown::Shutdown) -> Router {
    let api = Router::new()
        .route("/api/v1/me", get(me::show))
        .merge(organisations::routes())
        .merge(projects::routes())
        .merge(workflows::routes())
        .merge(commands::routes())
        .route_layer(middleware::from_fn_with_state(state.clone(), auth::owner));
    let http = Router::new()
        .route("/healthz", get(health::alive))
        .route("/readyz", get(health::ready))
        .merge(api)
        .merge(agents::routes(state.clone()))
        .fallback(envelope::no_route)
        .method_not_allowed_fallback(envelope::no_route)
        .with_state(state);
    let grpc = grpc::routes(Routes::new(AgentServer::new(connector)), shutdown);
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
