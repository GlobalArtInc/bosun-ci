use std::ffi::OsStr;
use std::fmt;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::process::Command;
use tokio::time::timeout;

const PROGRAM: &str = "podman";

const ANSWER_WITHIN: Duration = Duration::from_secs(60);

const OLDEST: (u64, u64) = (4, 4);

const STDERR_TAIL: usize = 400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detection {
    Usable { version: String },
    Lacking(Vec<String>),
}

impl fmt::Display for Detection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Detection::Usable { version } => write!(
                f,
                "podman {version} with cgroups v2 and netavark, so the agent holds podman"
            ),
            Detection::Lacking(lacks) => write!(
                f,
                "the agent runs without the podman capability, which needs {}",
                lacks.join("; ")
            ),
        }
    }
}

pub async fn detect() -> Detection {
    detect_with(OsStr::new(PROGRAM), ANSWER_WITHIN).await
}

pub async fn detect_with(program: &OsStr, limit: Duration) -> Detection {
    let mut command = Command::new(program);
    command
        .args(["info", "--format", "json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return lacking(format!(
                "podman 4.4 or newer, and {} is not on the PATH",
                program.to_string_lossy()
            ));
        }
        Err(error) => {
            return lacking(format!(
                "a podman that starts, and {} could not be run: {error}",
                program.to_string_lossy()
            ));
        }
    };
    let output = match timeout(limit, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return lacking(format!(
                "a podman that answers, and `podman info` failed: {error}"
            ));
        }
        Err(_) => {
            return lacking(format!(
                "a podman that answers `podman info` within {} seconds",
                limit.as_secs()
            ));
        }
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return lacking(format!(
            "a working podman, and `podman info` exited with {}: {}",
            output.status,
            tail(stderr.trim())
        ));
    }
    assess(&String::from_utf8_lossy(&output.stdout))
}

pub fn assess(info: &str) -> Detection {
    let info: Info = match serde_json::from_str(info) {
        Ok(info) => info,
        Err(error) => {
            return lacking(format!(
                "a `podman info --format json` that can be read, and it is not valid: {error}"
            ));
        }
    };
    let mut lacks = Vec::new();
    let version = info.version.version.unwrap_or_default();
    match release(&version) {
        Some(found) if found >= OLDEST => {}
        Some(_) => lacks.push(format!(
            "podman 4.4 or newer, where it finds podman {version}"
        )),
        None => lacks.push(format!(
            "podman 4.4 or newer, where it finds the version '{version}'"
        )),
    }
    let cgroups = info.host.cgroup_version.unwrap_or_default();
    if cgroups != "v2" {
        lacks.push(if cgroups.is_empty() {
            "cgroups v2, which podman does not report".to_owned()
        } else {
            format!("cgroups v2, where podman reports {cgroups}")
        });
    }
    if info.host.service_is_remote {
        lacks.push(
            "a local podman, where podman reports serviceIsRemote: its paths are those of another \
             machine"
                .to_owned(),
        );
    }
    let network = info.host.network_backend.unwrap_or_default();
    if network != "netavark" {
        lacks.push(if network.is_empty() {
            "netavark, which podman does not report as its network backend".to_owned()
        } else {
            format!("netavark, where podman uses {network}")
        });
    }
    if lacks.is_empty() {
        Detection::Usable { version }
    } else {
        Detection::Lacking(lacks)
    }
}

fn lacking(reason: String) -> Detection {
    Detection::Lacking(vec![reason])
}

fn release(version: &str) -> Option<(u64, u64)> {
    let mut parts = version.trim().splitn(3, '.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts
        .next()?
        .split(|character: char| !character.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((major, minor))
}

fn tail(text: &str) -> &str {
    let mut start = text.len().saturating_sub(STDERR_TAIL);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

#[derive(Deserialize)]
struct Info {
    #[serde(default)]
    host: Host,
    #[serde(default)]
    version: Version,
}

#[derive(Default, Deserialize)]
struct Host {
    #[serde(default, rename = "cgroupVersion")]
    cgroup_version: Option<String>,
    #[serde(default, rename = "networkBackend")]
    network_backend: Option<String>,
    #[serde(default, rename = "serviceIsRemote")]
    service_is_remote: bool,
}

#[derive(Default, Deserialize)]
struct Version {
    #[serde(default, rename = "Version")]
    version: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{Detection, assess, release};

    fn info(version: &str, cgroups: &str, network: &str) -> String {
        format!(
            r#"{{"host": {{"cgroupVersion": "{cgroups}", "networkBackend": "{network}",
            "security": {{"rootless": true}}}}, "version": {{"APIVersion": "{version}",
            "Version": "{version}", "GoVersion": "go1.21.1"}}}}"#
        )
    }

    #[test]
    fn a_podman_of_4_4_with_cgroups_v2_and_netavark_is_usable() {
        for version in ["4.4.0", "4.9.3", "5.0.0-dev", "4.4.1-rc1", "10.0.0"] {
            assert_eq!(
                assess(&info(version, "v2", "netavark")),
                Detection::Usable {
                    version: version.to_owned()
                },
                "{version}"
            );
        }
    }

    #[test]
    fn every_missing_prerequisite_is_named_at_once() {
        assert_eq!(
            assess(&info("4.3.1", "v1", "cni")),
            Detection::Lacking(vec![
                "podman 4.4 or newer, where it finds podman 4.3.1".to_owned(),
                "cgroups v2, where podman reports v1".to_owned(),
                "netavark, where podman uses cni".to_owned(),
            ])
        );
        let message = assess(&info("3.4.4", "v2", "netavark")).to_string();
        assert_eq!(
            message,
            "the agent runs without the podman capability, which needs podman 4.4 or newer, \
             where it finds podman 3.4.4"
        );
    }

    #[test]
    fn what_podman_leaves_out_or_garbles_counts_as_missing() {
        assert_eq!(
            assess(r#"{"host": {}, "version": {}}"#),
            Detection::Lacking(vec![
                "podman 4.4 or newer, where it finds the version ''".to_owned(),
                "cgroups v2, which podman does not report".to_owned(),
                "netavark, which podman does not report as its network backend".to_owned(),
            ])
        );
        let garbled = assess(&info("four", "v2", "netavark"));
        assert_eq!(
            garbled,
            Detection::Lacking(vec![
                "podman 4.4 or newer, where it finds the version 'four'".to_owned()
            ])
        );
        let Detection::Lacking(lacks) = assess("not json") else {
            panic!("a podman that prints no JSON was taken for usable");
        };
        assert!(lacks[0].starts_with("a `podman info --format json` that can be read"));
    }

    #[test]
    fn a_remote_podman_is_not_usable_whatever_else_it_reports() {
        let remote = info("5.2.0", "v2", "netavark")
            .replace(r#""security""#, r#""serviceIsRemote": true, "security""#);
        assert_eq!(
            assess(&remote),
            Detection::Lacking(vec![
                "a local podman, where podman reports serviceIsRemote: its paths are those of \
                 another machine"
                    .to_owned()
            ])
        );
    }

    #[test]
    fn a_release_is_its_major_and_minor_whatever_follows() {
        for (version, expected) in [
            ("4.4.0", Some((4, 4))),
            ("4.10.2", Some((4, 10))),
            ("5.0.0-dev", Some((5, 0))),
            ("4.4-rc1", Some((4, 4))),
            ("4", None),
            ("", None),
            ("v4.4.0", None),
        ] {
            assert_eq!(release(version), expected, "{version}");
        }
    }
}
