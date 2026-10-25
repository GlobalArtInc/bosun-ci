use std::io::ErrorKind;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE, HOST};
use axum::http::{HeaderMap, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use bosun_types::error::ErrorCode;
use tokio::fs::File;
use tokio_util::io::ReaderStream;
use tracing::error;

use crate::AppState;
use crate::envelope::ApiError;

const SCRIPT: &str = include_str!("../../../deploy/install-agent.sh");

const SERVER: &str = "__BOSUN_SERVER__";

const VERSION: &str = "__BOSUN_VERSION__";

const PLATFORMS: [&str; 2] = ["linux-x86_64", "linux-aarch64"];

const FORWARDED_PROTO: &str = "x-forwarded-proto";

const LONGEST_HOST: usize = 255;

const CHUNK: usize = 64 * 1024;

pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/install-agent.sh", get(script))
        .route("/_runtime/agent/{platform}", get(agent))
}

async fn script(headers: HeaderMap, uri: Uri) -> Result<Response, ApiError> {
    let server = server(&headers, &uri).ok_or_else(|| {
        ApiError::new(
            ErrorCode::InvalidInput,
            "the request names no host by which the script could reach the server",
        )
    })?;
    let text = render(&server);
    Ok(([(CONTENT_TYPE, "text/x-shellscript; charset=utf-8")], text).into_response())
}

fn render(server: &str) -> String {
    SCRIPT
        .replace(SERVER, server)
        .replace(VERSION, env!("CARGO_PKG_VERSION"))
}

fn server(headers: &HeaderMap, uri: &Uri) -> Option<String> {
    let host = match headers.get(HOST) {
        Some(host) => host.to_str().ok()?,
        None => uri.authority()?.as_str(),
    };
    let reachable = !host.is_empty()
        && host.len() <= LONGEST_HOST
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-.:[]".contains(&byte));
    if !reachable {
        return None;
    }
    let secure = headers
        .get(FORWARDED_PROTO)
        .and_then(|proto| proto.to_str().ok())
        .and_then(|proto| proto.split(',').next())
        .is_some_and(|proto| proto.trim().eq_ignore_ascii_case("https"));
    let scheme = if secure { "https" } else { "http" };
    Some(format!("{scheme}://{host}"))
}

async fn agent(
    State(state): State<AppState>,
    Path(platform): Path<String>,
) -> Result<Response, ApiError> {
    if !PLATFORMS.contains(&platform.as_str()) {
        return Err(ApiError::new(
            ErrorCode::NotFound,
            format!(
                "there is no agent for {platform}; the server has agents for {}",
                PLATFORMS.join(" and ")
            ),
        ));
    }
    let path = state.agent_binaries.join(&platform);
    let file = match File::open(&path).await {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Err(ApiError::new(
                ErrorCode::NotFound,
                format!(
                    "the server has no agent for {platform}: there is no {}",
                    path.display()
                ),
            ));
        }
        Err(error) => return Err(unreadable(&path, error)),
    };
    let length = file
        .metadata()
        .await
        .map_err(|error| unreadable(&path, error))?
        .len();
    Ok((
        [
            (CONTENT_TYPE, "application/octet-stream".to_owned()),
            (CONTENT_LENGTH, length.to_string()),
        ],
        Body::from_stream(ReaderStream::with_capacity(file, CHUNK)),
    )
        .into_response())
}

fn unreadable(path: &std::path::Path, error: std::io::Error) -> ApiError {
    error!(path = %path.display(), %error, "an agent binary could not be read");
    ApiError::new(
        ErrorCode::Internal,
        format!("the agent binary {} could not be read", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, Uri};

    use super::{SCRIPT, SERVER, VERSION, render, server};

    #[test]
    fn the_script_names_the_server_and_its_version_once_each() {
        assert_eq!(SCRIPT.matches(SERVER).count(), 1);
        assert_eq!(SCRIPT.matches(VERSION).count(), 1);
        let rendered = render("https://ci.example.org");
        assert!(
            rendered.contains("server='https://ci.example.org'"),
            "{rendered}"
        );
        assert!(
            rendered.contains(&format!("version='{}'", env!("CARGO_PKG_VERSION"))),
            "{rendered}"
        );
    }

    #[test]
    fn the_server_is_the_host_of_the_request_with_the_scheme_the_proxy_forwards() {
        let headers = |pairs: &[(&'static str, &'static str)]| {
            let mut headers = HeaderMap::new();
            for (name, value) in pairs {
                headers.insert(*name, HeaderValue::from_static(value));
            }
            headers
        };
        let path = Uri::from_static("/install-agent.sh");
        for (pairs, expected) in [
            (
                vec![("host", "ci.example.org")],
                Some("http://ci.example.org"),
            ),
            (
                vec![("host", "10.0.0.5:8080")],
                Some("http://10.0.0.5:8080"),
            ),
            (vec![("host", "[::1]:8080")], Some("http://[::1]:8080")),
            (
                vec![("host", "ci.example.org"), ("x-forwarded-proto", "https")],
                Some("https://ci.example.org"),
            ),
            (
                vec![
                    ("host", "ci.example.org"),
                    ("x-forwarded-proto", "HTTPS, http"),
                ],
                Some("https://ci.example.org"),
            ),
            (
                vec![("host", "ci.example.org"), ("x-forwarded-proto", "gopher")],
                Some("http://ci.example.org"),
            ),
            (vec![("host", "ci.example.org'; rm -rf /")], None),
            (vec![("host", "")], None),
            (vec![], None),
        ] {
            assert_eq!(
                server(&headers(&pairs), &path).as_deref(),
                expected,
                "{pairs:?}"
            );
        }
        assert_eq!(
            server(
                &HeaderMap::new(),
                &Uri::from_static("http://ci.example.org:8443/install-agent.sh")
            )
            .as_deref(),
            Some("http://ci.example.org:8443")
        );
    }
}
