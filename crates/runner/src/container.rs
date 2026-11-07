mod dirty;
pub(crate) mod engine;
pub(crate) mod launch;
mod lifecycle;
mod options;
mod volumes;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub use dirty::{is_marked, recover};
pub use engine::{Call, Engine, Outcome, Pending, Podman};
pub use lifecycle::sweep;
pub(crate) use lifecycle::{Container, Request};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullPolicy {
    #[default]
    Missing,
    Never,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    pub username: String,
    pub password: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("username", &self.username)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Libc {
    Glibc,
    Musl,
}

pub trait Provision: Send + Sync + fmt::Debug {
    fn nodes(&self, libc: Libc) -> Pending<'_, Result<Vec<PathBuf>, String>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mounts {
    pub work: PathBuf,
    pub externals: PathBuf,
    pub tools: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ContainerJob {
    pub image: String,
    pub credentials: Option<Credentials>,
    pub allow_privileged: bool,
    pub untrusted: bool,
    pub pull_policy: PullPolicy,
    pub engine: Arc<dyn Engine>,
    pub mounts: Mounts,
    pub state: PathBuf,
    pub provision: Option<Arc<dyn Provision>>,
}

#[derive(Debug, Clone)]
pub struct Recovery {
    pub engine: Arc<dyn Engine>,
    pub state: PathBuf,
}

pub(crate) const WORK: &str = "/__w";
pub(crate) const TEMP: &str = "/__w/_temp";
pub(crate) const EXTERNALS: &str = "/__e";
pub(crate) const TOOLS: &str = "/__t";
pub(crate) const HOME: &str = "/github/home";
pub(crate) const WORKFLOW: &str = "/github/workflow";

#[derive(Debug, Clone)]
pub(crate) struct Layout {
    pairs: Vec<(PathBuf, &'static str, bool)>,
}

impl Layout {
    pub(crate) fn new(mounts: &Mounts, temp: &Path) -> Layout {
        Layout {
            pairs: vec![
                (mounts.work.clone(), WORK, false),
                (temp.to_path_buf(), TEMP, false),
                (mounts.externals.clone(), EXTERNALS, true),
                (mounts.tools.clone(), TOOLS, false),
                (temp.join("_github_home"), HOME, false),
                (temp.join("_github_workflow"), WORKFLOW, false),
            ],
        }
    }

    pub(crate) fn pairs(&self) -> &[(PathBuf, &'static str, bool)] {
        &self.pairs
    }

    pub(crate) fn to_container(&self, host: &Path) -> Option<String> {
        let mut best: Option<(usize, String)> = None;
        for (root, container, _) in &self.pairs {
            if let Ok(rest) = host.strip_prefix(root) {
                let depth = root.components().count();
                if best.as_ref().is_none_or(|(known, _)| depth > *known) {
                    let rest = rest.to_string_lossy();
                    let path = if rest.is_empty() {
                        (*container).to_owned()
                    } else {
                        format!("{container}/{rest}")
                    };
                    best = Some((depth, path));
                }
            }
        }
        best.map(|(_, path)| path)
    }

    pub(crate) fn to_host(&self, container: &str) -> Option<PathBuf> {
        let mut best: Option<(usize, PathBuf)> = None;
        for (root, mounted, _) in &self.pairs {
            let Some(rest) = container.strip_prefix(mounted) else {
                continue;
            };
            let rest = match rest {
                "" => "",
                rest if rest.starts_with('/') => rest.trim_start_matches('/'),
                _ => continue,
            };
            if best
                .as_ref()
                .is_none_or(|(known, _)| mounted.len() > *known)
            {
                let path = if rest.is_empty() {
                    root.clone()
                } else {
                    root.join(rest)
                };
                best = Some((mounted.len(), path));
            }
        }
        best.map(|(_, path)| path)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Layout, Mounts};

    fn layout() -> Layout {
        Layout::new(
            &Mounts {
                work: PathBuf::from("/agent/work"),
                externals: PathBuf::from("/agent/externals"),
                tools: PathBuf::from("/agent/tools"),
            },
            Path::new("/agent/work/_temp"),
        )
    }

    #[test]
    fn host_paths_map_to_the_table_of_mounts_and_back_choosing_the_longest_match() {
        let layout = layout();
        for (host, container) in [
            ("/agent/work", "/__w"),
            ("/agent/work/_temp", "/__w/_temp"),
            (
                "/agent/work/globalart.keel/keel",
                "/__w/globalart.keel/keel",
            ),
            ("/agent/work/_temp/step-1.sh", "/__w/_temp/step-1.sh"),
            ("/agent/externals/node24/bin/node", "/__e/node24/bin/node"),
            ("/agent/tools", "/__t"),
            ("/agent/work/_temp/_github_home", "/github/home"),
            (
                "/agent/work/_temp/_github_workflow/event.json",
                "/github/workflow/event.json",
            ),
        ] {
            assert_eq!(
                layout.to_container(Path::new(host)).as_deref(),
                Some(container),
                "{host}"
            );
            assert_eq!(
                layout.to_host(container),
                Some(PathBuf::from(host)),
                "{container}"
            );
        }
        assert_eq!(layout.to_container(Path::new("/agent/workdir")), None);
        assert_eq!(layout.to_container(Path::new("/etc/passwd")), None);
        assert_eq!(layout.to_host("/__wx/escape"), None);
        assert_eq!(layout.to_host("/usr/bin"), None);
    }
}
