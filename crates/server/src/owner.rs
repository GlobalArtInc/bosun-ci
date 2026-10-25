use std::fs::DirBuilder;
use std::io::{self, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

use bosun_db::tokens::NewToken;
use chrono::{TimeDelta, Utc};
use sea_orm::DatabaseConnection;
use tempfile::NamedTempFile;
use tracing::info;

use crate::token::{PERSONAL, Token};
use crate::{Error, slug};

pub(crate) const LOGIN_RULE: &str = "a login is 1 to 64 lowercase letters, digits, `-`, `_` or \
                                     `.`, and starts with a letter or a digit";

const TOKEN_NAME: &str = "bootstrap";

const TOKEN_LIFETIME: TimeDelta = TimeDelta::days(365);

const FIRST_OWNER: &str = "admin";

const TOKEN_FILE: &str = "admin.token";

pub(crate) fn is_login(text: &str) -> bool {
    slug::is_slug(text)
}

pub(crate) async fn create(db: &DatabaseConnection, login: &str) -> Result<String, Error> {
    if !is_login(login) {
        return Err(Error::InvalidLogin {
            login: login.to_owned(),
        });
    }
    let (token, first) = first_token()?;
    let expires_at = first.expires_at;
    bosun_db::users::create_owner(db, login, first)
        .await
        .map_err(Error::CreateOwner)?;
    info!(login, %expires_at, "created the owner and its first personal token");
    Ok(token.into_string())
}

pub(crate) async fn bootstrap(db: &DatabaseConnection, login: &str) -> Result<(), Error> {
    let (token, first) = first_token()?;
    let expires_at = first.expires_at;
    match bosun_db::users::bootstrap_owner(db, login, first)
        .await
        .map_err(Error::Bootstrap)?
    {
        Some(_) => info!(
            login,
            token = token.into_string().as_str(),
            %expires_at,
            "created the owner from bootstrap.admin; its token is shown only this once"
        ),
        None => info!(
            login,
            "the database has users already, so bootstrap.admin creates no owner"
        ),
    }
    Ok(())
}

pub(crate) async fn first_start(db: &DatabaseConnection, data_dir: &Path) -> Result<(), Error> {
    let (token, first) = first_token()?;
    let expires_at = first.expires_at;
    if bosun_db::users::bootstrap_owner(db, FIRST_OWNER, first)
        .await
        .map_err(Error::FirstOwner)?
        .is_none()
    {
        return Ok(());
    }
    let path = data_dir.join(TOKEN_FILE);
    write_token(data_dir, &path, token).map_err(|source| Error::TokenFile {
        path: path.clone(),
        source,
    })?;
    info!(
        login = FIRST_OWNER,
        path = %path.display(),
        %expires_at,
        "created the owner admin on the first start and wrote its token to the data directory"
    );
    Ok(())
}

fn write_token(data_dir: &Path, path: &Path, token: Token) -> io::Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(data_dir)?;
    let mut file = NamedTempFile::new_in(data_dir)?;
    writeln!(file, "{}", token.into_string())?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn first_token() -> Result<(Token, NewToken), Error> {
    let token = Token::generate(PERSONAL)?;
    let stored = NewToken {
        name: TOKEN_NAME.to_owned(),
        sha256: token.sha256(),
        expires_at: Utc::now() + TOKEN_LIFETIME,
    };
    Ok((token, stored))
}

#[cfg(test)]
mod tests {
    use super::is_login;

    #[test]
    fn a_login_is_short_lowercase_and_starts_with_a_letter_or_a_digit() {
        for login in [
            "roman",
            "r",
            "0",
            "ci-bot",
            "first.last",
            "a_b",
            &"a".repeat(64),
        ] {
            assert!(is_login(login), "`{login}` was refused");
        }
        for text in [
            "",
            "Roman",
            "roman x",
            "-roman",
            ".roman",
            "_roman",
            "user:roman",
            "rómán",
            &"a".repeat(65),
        ] {
            assert!(!is_login(text), "`{text}` was accepted");
        }
    }
}
