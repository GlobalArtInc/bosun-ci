use std::fs;
use std::path::Path;

use bosun_forge::{App, Forge as _, GitHubApp};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::sync::OnceCell;

use crate::Error;
use crate::config::GitHub;
use crate::digest::unhex;

const SIGNATURE_PREFIX: &str = "sha256=";

pub(crate) struct Forge {
    pub(crate) app: GitHubApp,
    secret: Vec<u8>,
    identity: OnceCell<App>,
}

pub(crate) fn load(config: &GitHub) -> Result<Forge, Error> {
    let key = read(&config.private_key_file, "private key")?;
    let app = GitHubApp::new(config.app_id, &key, &config.api_url).map_err(Error::GitHub)?;
    let secret = read(&config.webhook_secret_file, "webhook secret")?;
    let secret = secret.trim_ascii().to_vec();
    if secret.is_empty() {
        return Err(Error::EmptyWebhookSecret {
            path: config.webhook_secret_file.clone(),
        });
    }
    Ok(Forge {
        app,
        secret,
        identity: OnceCell::new(),
    })
}

fn read(path: &Path, what: &'static str) -> Result<Vec<u8>, Error> {
    fs::read(path).map_err(|source| Error::ReadGitHubFile {
        what,
        path: path.to_owned(),
        source,
    })
}

impl Forge {
    pub(crate) async fn bot(&self) -> Result<u64, bosun_forge::Error> {
        let identity = self.identity.get_or_try_init(|| self.app.app()).await?;
        Ok(identity.bot.id)
    }

    #[cfg(test)]
    pub(crate) fn of(app: GitHubApp) -> Forge {
        Forge {
            app,
            secret: Vec::new(),
            identity: OnceCell::new(),
        }
    }

    pub(crate) fn signed(&self, signature: &[u8], body: &[u8]) -> bool {
        let Some(digits) = signature.strip_prefix(SIGNATURE_PREFIX.as_bytes()) else {
            return false;
        };
        let Some(expected) = std::str::from_utf8(digits).ok().and_then(unhex) else {
            return false;
        };
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(&self.secret) else {
            return false;
        };
        mac.update(body);
        mac.verify_slice(&expected).is_ok()
    }
}
