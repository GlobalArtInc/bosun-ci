use std::error::Error;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use bosun_server::Config;
use clap::{Parser, Subcommand};
use tokio::signal::unix::{SignalKind, signal};
use tracing::error;
use tracing_subscriber::filter::{LevelFilter, Targets};
use tracing_subscriber::fmt::writer::BoxMakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{Layer, fmt};

const DEFAULT_CONFIG: &str = "/etc/bosun/server.toml";

#[derive(Parser)]
#[command(name = "bosun-server", version, about = "The Bosun CI/CD server")]
struct Arguments {
    #[arg(
        long,
        global = true,
        value_name = "FILE",
        help = "The configuration file [default: /etc/bosun/server.toml when it exists]"
    )]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Bring the schema up to date, then serve until SIGTERM or SIGINT")]
    Serve,
    #[command(about = "Bring the schema up to date and exit")]
    Migrate,
    #[command(subcommand, about = "Administer the installation")]
    Admin(Admin),
}

#[derive(Subcommand)]
enum Admin {
    #[command(
        about = "Bring the schema up to date, create an owner and print its first personal token"
    )]
    Create {
        #[arg(help = "The login of the owner")]
        login: String,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let arguments = Arguments::parse();
    logs(match arguments.command {
        Command::Admin(_) => BoxMakeWriter::new(io::stderr),
        Command::Serve | Command::Migrate => BoxMakeWriter::new(io::stdout),
    });
    match run(arguments).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            error!("{failure}");
            ExitCode::FAILURE
        }
    }
}

async fn run(arguments: Arguments) -> Result<(), Box<dyn Error>> {
    let file = arguments
        .config
        .or_else(|| Some(PathBuf::from(DEFAULT_CONFIG)).filter(|path| path.exists()));
    let config = Config::load(file.as_deref())?;
    match arguments.command {
        Command::Serve => bosun_server::serve(config, stop()?).await?,
        Command::Migrate => bosun_server::migrate(&config).await?,
        Command::Admin(Admin::Create { login }) => {
            let token = bosun_server::create_owner(&config, &login).await?;
            println!("{token}");
        }
    }
    Ok(())
}

fn stop() -> io::Result<impl Future<Output = String> + Send + 'static> {
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    Ok(async move {
        tokio::select! {
            _ = terminate.recv() => "received SIGTERM".to_owned(),
            _ = interrupt.recv() => "received SIGINT".to_owned(),
        }
    })
}

fn logs(writer: BoxMakeWriter) {
    let filter = Targets::new()
        .with_default(LevelFilter::INFO)
        .with_target("sqlx", LevelFilter::WARN);
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
