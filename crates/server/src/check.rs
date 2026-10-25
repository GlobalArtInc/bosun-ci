use std::error::Error as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use reqwest::StatusCode;
use serde::Deserialize;

use crate::{Config, Error};

const TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
struct Unready {
    reason: String,
}

pub async fn check(config: &Config) -> Result<SocketAddr, Error> {
    if config.listen.port() == 0 {
        return Err(Error::CheckAnyPort);
    }
    let address = SocketAddr::new(
        match config.listen.ip() {
            IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
            ip => ip,
        },
        config.listen.port(),
    );
    let unanswered = |error: reqwest::Error| {
        let mut reason = error.to_string();
        let mut source = error.source();
        while let Some(cause) = source {
            reason.push_str(&format!(": {cause}"));
            source = cause.source();
        }
        Error::Unanswered { address, reason }
    };
    let response = reqwest::Client::builder()
        .no_proxy()
        .timeout(TIMEOUT)
        .build()
        .map_err(unanswered)?
        .get(format!("http://{address}/readyz"))
        .send()
        .await
        .map_err(unanswered)?;
    let status = response.status();
    if status == StatusCode::OK {
        return Ok(address);
    }
    let reason = match response.json::<Unready>().await {
        Ok(unready) => unready.reason,
        Err(_) => format!("it answers /readyz with {status}"),
    };
    Err(Error::Unready { address, reason })
}
