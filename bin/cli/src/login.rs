use std::io::{self, BufRead, IsTerminal, Stdin};
use std::os::fd::AsFd;
use std::process::ExitCode;

use bosun_types::api::Me;
use rustix::termios::{LocalModes, OptionalActions, tcgetattr, tcsetattr};

use crate::client::{Client, Failure};
use crate::config::{self, Profile};
use crate::{Context, output};

pub(crate) async fn login(context: &Context, server: Option<String>) -> Result<ExitCode, Failure> {
    let path = config::path()?;
    let server = match server {
        Some(server) => config::server(&server)?,
        None => config::stored(&path, &context.profile)?
            .map(|profile| profile.server)
            .ok_or_else(|| {
                Failure::new(format!(
                    "the profile `{}` has no server yet; name it with --server <url>",
                    context.profile
                ))
            })?,
    };
    let token = token()?;
    let profile = Profile { server, token };
    let client = Client::new(profile.clone())?;
    let me: Me = client.get("/api/v1/me", &[]).await?;
    config::save(&path, &context.profile, profile)?;
    if context.json() {
        output::json(&me)?;
    } else {
        println!(
            "signed in to {} as {}, kept in the profile `{}` of {}",
            client.server(),
            me.user.login,
            context.profile,
            path.display()
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn token() -> Result<String, Failure> {
    let stdin = io::stdin();
    let line = if stdin.is_terminal() {
        eprint!("Personal token: ");
        hidden(&stdin)
    } else {
        let mut line = String::new();
        stdin.lock().read_line(&mut line).map(|_| line)
    }
    .map_err(|error| Failure::new(format!("the token could not be read: {error}")))?;
    let token = line.trim().to_owned();
    if token.is_empty() {
        return Err(Failure::new(
            "no token arrived on standard input; pass a personal token bst_… on its first line",
        ));
    }
    Ok(token)
}

pub(crate) fn hidden(stdin: &Stdin) -> io::Result<String> {
    let saved = tcgetattr(stdin.as_fd())?;
    let mut quiet = saved.clone();
    quiet.local_modes.remove(LocalModes::ECHO);
    tcsetattr(stdin.as_fd(), OptionalActions::Now, &quiet)?;
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);
    let restored = tcsetattr(stdin.as_fd(), OptionalActions::Now, &saved);
    eprintln!();
    read?;
    restored?;
    Ok(line)
}
