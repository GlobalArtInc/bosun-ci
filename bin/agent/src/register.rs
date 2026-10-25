use std::path::{Path, PathBuf};
use std::time::Duration;

use bosun_types::api::{RegisterAgentRequest, RegisterAgentResponse};
use bosun_types::error::ErrorEnvelope;
use reqwest::StatusCode;

use crate::config::{self, Config};

const TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) struct Registration {
    pub(crate) file: PathBuf,
    pub(crate) url: String,
    pub(crate) token: String,
    pub(crate) name: String,
    pub(crate) dir: PathBuf,
    pub(crate) labels: Vec<String>,
}

pub(crate) async fn register(registration: Registration) -> Result<Config, String> {
    let Registration {
        file,
        url,
        token,
        name,
        dir,
        labels,
    } = registration;
    if file.exists() {
        return Err(format!(
            "{} exists, so this agent is registered already; remove the file to register it again",
            file.display()
        ));
    }
    let dir = std::path::absolute(&dir)
        .map_err(|error| format!("{} has no absolute form: {error}", dir.display()))?;
    let endpoint = config::server(&url)?
        .join("api/v1/agents/register")
        .map_err(|error| format!("`{url}` has no registration address: {error}"))?;
    let response = reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|error| format!("the agent has no HTTP client: {error}"))?
        .post(endpoint.clone())
        .json(&RegisterAgentRequest {
            token,
            name,
            labels: labels.clone(),
        })
        .send()
        .await
        .map_err(|error| format!("{endpoint} could not be reached: {error}"))?;
    let status = response.status();
    if status != StatusCode::CREATED {
        return Err(refusal(status, response).await);
    }
    let registered: RegisterAgentResponse = response
        .json()
        .await
        .map_err(|error| format!("the server answered the registration with no agent: {error}"))?;
    let config = Config {
        url,
        dir,
        agent_id: registered.agent_id,
        credential: registered.credential,
        labels,
        http_proxy: None,
        https_proxy: None,
        no_proxy: None,
        spool_max_bytes: None,
    };
    config.create(Path::new(&file))?;
    Ok(config)
}

async fn refusal(status: StatusCode, response: reqwest::Response) -> String {
    let text = response.text().await.unwrap_or_default();
    let Ok(envelope) = serde_json::from_str::<ErrorEnvelope>(&text) else {
        return format!("the server refused the registration with {status}: {text}");
    };
    let mut message = format!(
        "the server refused the registration with {status}: {}",
        envelope.error.message
    );
    for field in &envelope.error.fields {
        message.push_str(&format!("; {}: {}", field.field, field.message));
    }
    message
}
