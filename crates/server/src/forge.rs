use std::fs;
use std::path::Path;

use bosun_forge::{App, Forge as _, GitHubApp};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::sync::OnceCell;

use crate::Error;
use crate::config::GitHub;

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

    pub(crate) fn signed(&self, signature: &[u8], body: &[u8]) -> bool {
        let Some(digits) = signature.strip_prefix(SIGNATURE_PREFIX.as_bytes()) else {
            return false;
        };
        let Some(expected) = unhex(digits) else {
            return false;
        };
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(&self.secret) else {
            return false;
        };
        mac.update(body);
        mac.verify_slice(&expected).is_ok()
    }
}

fn unhex(digits: &[u8]) -> Option<Vec<u8>> {
    if !digits.len().is_multiple_of(2) {
        return None;
    }
    digits
        .chunks(2)
        .map(|pair| {
            let high = char::from(pair[0]).to_digit(16)?;
            let low = char::from(pair[1]).to_digit(16)?;
            u8::try_from(high * 16 + low).ok()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::unhex;

    #[test]
    fn a_signature_is_read_as_hexadecimal_digits() {
        assert_eq!(unhex(b"00ff7a"), Some(vec![0x00, 0xff, 0x7a]));
        assert_eq!(unhex(b"00FF7A"), Some(vec![0x00, 0xff, 0x7a]));
        assert_eq!(unhex(b""), Some(Vec::new()));
        for refused in [&b"0"[..], b"0g", b"zz", b"0 1f"] {
            assert_eq!(unhex(refused), None, "{refused:?}");
        }
    }
}
