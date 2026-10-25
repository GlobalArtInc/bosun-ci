use std::process::ExitCode;

use bosun_types::api::{Capability, LabelAlias, SetLabelAliasRequest};
use clap::{Subcommand, ValueEnum};

use crate::Context;
use crate::client::Failure;
use crate::output::{self, name};

#[derive(Subcommand)]
pub(crate) enum Command {
    #[command(
        about = "Map a label of runs-on to a pool, a capability and a default image, replacing \
                 the alias the label had"
    )]
    Set {
        #[arg(help = "The label, such as ubuntu-latest")]
        label: String,
        #[arg(
            long,
            value_name = "POOL",
            help = "The pool whose agents take the jobs of the label [default: any pool open to \
                    the project]"
        )]
        pool: Option<String>,
        #[arg(
            long,
            value_enum,
            value_name = "CAPABILITY",
            help = "The capability the jobs of the label need [default: either, or podman with \
                    --image]"
        )]
        capability: Option<Executor>,
        #[arg(
            long,
            value_name = "IMAGE",
            help = "The default image of the jobs of the label, which then run in a container"
        )]
        image: Option<String>,
        #[arg(
            long,
            value_name = "ORGANISATION",
            help = "Set the alias of an organisation, which wins over the installation's there \
                    [default: the installation's]"
        )]
        organisation: Option<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum Executor {
    Shell,
    Podman,
}

pub(crate) async fn run(context: &Context, command: Command) -> Result<ExitCode, Failure> {
    let client = context.client()?;
    match command {
        Command::Set {
            label,
            pool,
            capability,
            image,
            organisation,
        } => {
            let alias: LabelAlias = client
                .post(
                    "/api/v1/label-aliases",
                    &SetLabelAliasRequest {
                        label,
                        organisation: organisation.clone(),
                        pool: pool.clone(),
                        capability: capability.map(|executor| match executor {
                            Executor::Shell => Capability::Shell,
                            Executor::Podman => Capability::Podman,
                        }),
                        image,
                    },
                )
                .await?;
            if context.json() {
                output::json(&alias)?;
                return Ok(ExitCode::SUCCESS);
            }
            let scope = match &organisation {
                Some(organisation) => format!("organisation {organisation}"),
                None => "the installation".to_owned(),
            };
            let pool = match &pool {
                Some(pool) => format!("the pool {pool}"),
                None => "any pool".to_owned(),
            };
            let capability = match &alias.capability {
                Some(capability) => name(capability),
                None => "either capability".to_owned(),
            };
            let image = match &alias.image {
                Some(image) => format!(", image {image}"),
                None => String::new(),
            };
            println!(
                "the label {} of {scope} goes to {pool} with {capability}{image}",
                alias.label
            );
            Ok(ExitCode::SUCCESS)
        }
    }
}
