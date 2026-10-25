mod agent;
mod alias;
mod client;
mod config;
mod follow;
mod job;
mod login;
mod output;
mod pool;
mod project;
mod run;
mod token;
mod workflow;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::client::{Client, Failure};

#[derive(Parser)]
#[command(name = "bosun", version, about = "The command line of Bosun")]
struct Arguments {
    #[arg(
        long,
        global = true,
        value_name = "NAME",
        default_value = config::DEFAULT_PROFILE,
        help = "The profile of the configuration file to use"
    )]
    profile: String,
    #[arg(long, global = true, help = "Print JSON instead of text")]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(
        about = "Sign in to a server with a personal token read from standard input, and keep it \
                 in the profile"
    )]
    Login {
        #[arg(
            long,
            value_name = "URL",
            help = "The address of the server [default: the server of the profile]"
        )]
        server: Option<String>,
    },
    #[command(subcommand, about = "Read and change projects")]
    Project(project::Command),
    #[command(subcommand, about = "Push, dispatch and inventory workflows")]
    Workflow(workflow::Command),
    #[command(subcommand, about = "Act on runs")]
    Run(run::Command),
    #[command(subcommand, about = "Read the logs of jobs")]
    Job(job::Command),
    #[command(subcommand, about = "List, drain and delete agents")]
    Agent(agent::Command),
    #[command(
        subcommand,
        about = "List and create pools and their registration tokens"
    )]
    Pool(pool::Command),
    #[command(
        subcommand,
        about = "Map labels of runs-on to pools, capabilities and images"
    )]
    Alias(alias::Command),
    #[command(subcommand, about = "Create personal tokens")]
    Token(token::Command),
}

pub(crate) struct Context {
    profile: String,
    json: bool,
}

impl Context {
    pub(crate) fn json(&self) -> bool {
        self.json
    }

    pub(crate) fn client(&self) -> Result<Client, Failure> {
        let profile = config::load(&config::path()?, &self.profile)?;
        Client::new(profile)
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let arguments = Arguments::parse();
    let context = Context {
        profile: arguments.profile,
        json: arguments.json,
    };
    let outcome = match arguments.command {
        Command::Login { server } => login::login(&context, server).await,
        Command::Project(command) => project::run(&context, command).await,
        Command::Workflow(command) => workflow::run(&context, command).await,
        Command::Run(command) => run::run(&context, command).await,
        Command::Job(command) => job::run(&context, command).await,
        Command::Agent(command) => agent::run(&context, command).await,
        Command::Pool(command) => pool::run(&context, command).await,
        Command::Alias(command) => alias::run(&context, command).await,
        Command::Token(command) => token::run(&context, command).await,
    };
    match outcome {
        Ok(code) => code,
        Err(failure) => {
            output::failure(&failure);
            ExitCode::FAILURE
        }
    }
}
