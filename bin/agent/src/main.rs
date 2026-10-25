mod agent;
mod backoff;
mod config;
mod connection;
mod fetch;
mod host;
mod job;
mod register;
mod spool;

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use bosun_runner::Tracking;
use clap::{Parser, Subcommand};
use tokio::signal::unix::{SignalKind, signal};
use tracing::{error, info};
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, fmt};

use crate::agent::{Agent, VERSION};
use crate::config::Config;
use crate::register::Registration;
use crate::spool::Spool;

#[derive(Parser)]
#[command(
    name = "bosun-agent",
    version,
    about = "The Bosun agent: runs the jobs of a Bosun server"
)]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Register with a server and write the configuration file of this agent")]
    Register {
        #[arg(long, value_name = "FILE", help = "The configuration file to create")]
        config: PathBuf,
        #[arg(long, value_name = "URL", help = "The address of the server")]
        url: String,
        #[arg(long, value_name = "TOKEN", help = "A registration token brt_…")]
        token: String,
        #[arg(long, value_name = "NAME", help = "The name of this agent")]
        name: String,
        #[arg(
            long,
            value_name = "DIR",
            help = "The directory of this agent, for its workspaces and its log spool"
        )]
        dir: PathBuf,
        #[arg(
            long = "label",
            value_name = "LABEL",
            help = "A label beyond the automatic ones; repeat it for more"
        )]
        labels: Vec<String>,
    },
    #[command(about = "Connect to the server and run its jobs until SIGTERM or SIGINT")]
    Run {
        #[arg(long, value_name = "FILE", help = "The configuration file")]
        config: PathBuf,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let arguments = Arguments::parse();
    logs(match arguments.command {
        Command::Register { .. } => BoxMakeWriter::new(io::stderr),
        Command::Run { .. } => BoxMakeWriter::new(io::stdout),
    });
    let outcome = match arguments.command {
        Command::Register {
            config,
            url,
            token,
            name,
            dir,
            labels,
        } => register::register(Registration {
            file: config.clone(),
            url,
            token,
            name,
            dir,
            labels,
        })
        .await
        .map(|registered| {
            println!(
                "registered the agent {} in {}",
                registered.agent_id,
                config.display()
            );
        }),
        Command::Run { config } => run(config).await,
    };
    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            error!("{failure}");
            ExitCode::FAILURE
        }
    }
}

async fn run(file: PathBuf) -> Result<(), String> {
    let config = Config::load(&file)?;
    info!(version = VERSION, config = %file.display(), agent_id = %config.agent_id, "starting");
    let spool = Spool::open(&config.dir.join("spool"), config.spool_limit()).map_err(|error| {
        format!(
            "the log spool in {} cannot be opened: {error}",
            config.dir.display()
        )
    })?;
    let tracking = Tracking::probe();
    info!(%tracking, "the agent tracks the processes of each job");
    let stop = stop().map_err(|error| format!("the agent cannot handle signals: {error}"))?;
    Agent::new(config, spool, tracking).run(stop).await;
    Ok(())
}

fn stop() -> io::Result<std::pin::Pin<Box<dyn Future<Output = String> + Send>>> {
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    Ok(Box::pin(async move {
        tokio::select! {
            _ = terminate.recv() => "received SIGTERM".to_owned(),
            _ = interrupt.recv() => "received SIGINT".to_owned(),
        }
    }))
}

fn logs(writer: BoxMakeWriter) {
    let filter = Targets::new()
        .with_default(LevelFilter::INFO)
        .with_target("h2", LevelFilter::WARN)
        .with_target("hyper", LevelFilter::WARN)
        .with_target("tower", LevelFilter::WARN);
    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(writer)
                .json()
                .flatten_event(true)
                .with_current_span(true)
                .with_span_list(false)
                .with_filter(filter),
        )
        .init();
}
