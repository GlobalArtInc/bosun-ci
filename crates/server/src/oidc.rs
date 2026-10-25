mod admission;
mod http;
mod state;
mod token;

use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use tokio::sync::Mutex;
use tracing::warn;
use url::Url;
use url::form_urlencoded::byte_serialize;

use crate::Error;
use crate::config::Oidc;
use crate::site::Site;

pub(crate) use http::routes;

use self::token::{Expected, IdToken, Jwks};

const DISCOVERY: [&str; 2] = [".well-known", "openid-configuration"];

const METADATA_LIFETIME: Duration = Duration::from_secs(3600);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

const READ_TIMEOUT: Duration = Duration::from_secs(30);

const MESSAGE_LIMIT: usize = 200;

#[derive(Default)]
pub(crate) struct Providers {
    providers: Vec<Provider>,
}

struct Provider {
    config: Oidc,
    secret: String,
    redirect_uri: String,
    home: String,
    http: reqwest::Client,
    metadata: Mutex<Option<(Instant, Arc<Metadata>)>>,
    keys: Mutex<Option<Arc<Jwks>>>,
}

#[derive(Deserialize)]
struct Metadata {
    issuer: String,
    authorization_endpoint: Url,
    token_endpoint: Url,
    jwks_uri: Url,
}

#[derive(Deserialize)]
struct Tokens {
    id_token: Option<String>,
}

#[derive(Deserialize)]
struct Denial {
    error: String,
    error_description: Option<String>,
}

enum Failure {
    Unreachable(String),
    Refused(String),
}

impl Providers {
    pub(crate) fn load(configs: &[Oidc], site: &Site) -> Result<Providers, Error> {
        if configs.is_empty() {
            return Ok(Providers::default());
        }
        let http = reqwest::Client::builder()
            .user_agent(concat!("bosun/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .read_timeout(READ_TIMEOUT)
            .build()
            .map_err(|error| {
                Error::IdentityProviders(format!("the HTTP client cannot start: {error}"))
            })?;
        let providers = configs
            .iter()
            .map(|config| {
                let path = &config.client_secret_file;
                let secret =
                    fs::read_to_string(path).map_err(|source| Error::ReadClientSecret {
                        provider: config.name.clone(),
                        path: path.clone(),
                        source,
                    })?;
                let secret = secret.trim().to_owned();
                if secret.is_empty() {
                    return Err(Error::EmptyClientSecret {
                        provider: config.name.clone(),
                        path: path.clone(),
                    });
                }
                Ok(Provider {
                    redirect_uri: format!(
                        "{}/api/v1/auth/oidc/{}/callback",
                        site.url(),
                        config.name
                    ),
                    home: format!("{}/", site.url()),
                    config: config.clone(),
                    secret,
                    http: http.clone(),
                    metadata: Mutex::new(None),
                    keys: Mutex::new(None),
                })
            })
            .collect::<Result<Vec<Provider>, Error>>()?;
        Ok(Providers { providers })
    }

    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.providers
            .iter()
            .map(|provider| provider.config.name.as_str())
    }

    fn find(&self, name: &str) -> Option<&Provider> {
        self.providers
            .iter()
            .find(|provider| provider.config.name == name)
    }
}

impl Provider {
    fn name(&self) -> &str {
        &self.config.name
    }

    async fn metadata(&self) -> Result<Arc<Metadata>, Failure> {
        let mut cached = self.metadata.lock().await;
        if let Some((fetched, metadata)) = cached.as_ref()
            && fetched.elapsed() < METADATA_LIFETIME
        {
            return Ok(Arc::clone(metadata));
        }
        let mut url = Url::parse(&self.config.issuer).map_err(|error| {
            Failure::Unreachable(format!("the issuer is not an address: {error}"))
        })?;
        if let Ok(mut segments) = url.path_segments_mut() {
            segments.pop_if_empty().extend(DISCOVERY);
        }
        let metadata: Metadata = self.fetch(&url, "the discovery document").await?;
        if metadata.issuer != self.config.issuer {
            return Err(Failure::Unreachable(format!(
                "the discovery document at {url} names the issuer `{}`, and the configuration \
                 names `{}`",
                metadata.issuer, self.config.issuer
            )));
        }
        let metadata = Arc::new(metadata);
        *cached = Some((Instant::now(), Arc::clone(&metadata)));
        Ok(metadata)
    }

    async fn keys(&self, metadata: &Metadata, kid: Option<&str>) -> Result<Arc<Jwks>, Failure> {
        let mut cached = self.keys.lock().await;
        if let Some(keys) = cached.as_ref()
            && keys.offers(kid)
        {
            return Ok(Arc::clone(keys));
        }
        let keys: Arc<Jwks> = Arc::new(self.fetch(&metadata.jwks_uri, "the keys").await?);
        *cached = Some(Arc::clone(&keys));
        Ok(keys)
    }

    async fn fetch<T: DeserializeOwned>(&self, url: &Url, what: &str) -> Result<T, Failure> {
        let unreachable =
            |problem: String| Failure::Unreachable(format!("{what} at {url} {problem}"));
        let response = self
            .http
            .get(url.clone())
            .send()
            .await
            .map_err(|error| unreachable(format!("could not be read: {error}")))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|error| unreachable(format!("could not be read: {error}")))?;
        if !status.is_success() {
            return Err(unreachable(format!("answered {status}")));
        }
        serde_json::from_slice(&body)
            .map_err(|error| unreachable(format!("is not what it should be: {error}")))
    }

    fn authorization(&self, metadata: &Metadata, state: &str, nonce: &str, challenge: &str) -> Url {
        let mut url = metadata.authorization_endpoint.clone();
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &self.config.client_id)
            .append_pair("redirect_uri", &self.redirect_uri)
            .append_pair("scope", &self.config.scopes.join(" "))
            .append_pair("state", state)
            .append_pair("nonce", nonce)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256");
        url
    }

    async fn exchange(
        &self,
        metadata: &Metadata,
        code: &str,
        verifier: &str,
    ) -> Result<String, Failure> {
        let response = self
            .http
            .post(metadata.token_endpoint.clone())
            .basic_auth(
                form_encoded(&self.config.client_id),
                Some(form_encoded(&self.secret)),
            )
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", &self.redirect_uri),
                ("code_verifier", verifier),
            ])
            .send()
            .await
            .map_err(|error| {
                Failure::Unreachable(format!(
                    "the token endpoint {} could not be reached: {error}",
                    metadata.token_endpoint
                ))
            })?;
        let status = response.status();
        let body = response.bytes().await.map_err(|error| {
            Failure::Unreachable(format!(
                "the answer of the token endpoint {} could not be read: {error}",
                metadata.token_endpoint
            ))
        })?;
        if !status.is_success() {
            return Err(match serde_json::from_slice::<Denial>(&body) {
                Ok(denial) => Failure::Refused(format!(
                    "the identity provider `{}` did not exchange the code: {}",
                    self.name(),
                    described(&denial.error, denial.error_description.as_deref())
                )),
                Err(_) => Failure::Unreachable(format!(
                    "the token endpoint {} answered {status}",
                    metadata.token_endpoint
                )),
            });
        }
        let tokens: Tokens = serde_json::from_slice(&body).map_err(|error| {
            Failure::Unreachable(format!(
                "the token endpoint {} answered what is not a token response: {error}",
                metadata.token_endpoint
            ))
        })?;
        tokens.id_token.ok_or_else(|| {
            Failure::Refused(format!(
                "the identity provider `{}` exchanged the code without an ID token",
                self.name()
            ))
        })
    }

    async fn verify(
        &self,
        metadata: &Metadata,
        token: &str,
        expected: &Expected<'_>,
    ) -> Result<serde_json::Map<String, serde_json::Value>, Failure> {
        let refused = |problem: String| {
            Failure::Refused(format!(
                "the ID token of `{}` is refused: it {problem}",
                self.name()
            ))
        };
        let token = IdToken::parse(token).map_err(refused)?;
        let keys = self.keys(metadata, token.kid()).await?;
        token.verify(&keys, expected).map_err(refused)
    }
}

impl Failure {
    fn log(&self, provider: &str) {
        if let Failure::Unreachable(problem) = self {
            warn!(provider, %problem, "an identity provider cannot be used");
        }
    }
}

fn form_encoded(text: &str) -> String {
    byte_serialize(text.as_bytes()).collect()
}

fn described(error: &str, description: Option<&str>) -> String {
    let mut text = match description {
        Some(description) => format!("{error}: {description}"),
        None => error.to_owned(),
    };
    if let Some((cut, _)) = text.char_indices().nth(MESSAGE_LIMIT) {
        text.truncate(cut);
        text.push('…');
    }
    text
}
