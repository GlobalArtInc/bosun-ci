#[cfg(target_os = "linux")]
mod cgroup;

use std::fmt;
use std::io;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

use rustix::process::{Pid, Signal, kill_process_group};
use tokio::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tracking {
    #[cfg(target_os = "linux")]
    Cgroups(PathBuf),
    ProcessGroups(String),
}

impl Tracking {
    pub fn probe() -> Tracking {
        #[cfg(target_os = "linux")]
        {
            match cgroup::probe() {
                Ok(root) => Tracking::Cgroups(root),
                Err(reason) => Tracking::ProcessGroups(reason),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            Tracking::ProcessGroups("cgroups exist only on Linux".to_owned())
        }
    }

    pub(crate) fn start(&self, job: &str) -> io::Result<Processes> {
        match self {
            #[cfg(target_os = "linux")]
            Tracking::Cgroups(root) => Ok(Processes {
                groups: Vec::new(),
                cgroup: Some(cgroup::Job::create(root, job)?),
            }),
            Tracking::ProcessGroups(_) => {
                let _ = job;
                Ok(Processes {
                    groups: Vec::new(),
                    #[cfg(target_os = "linux")]
                    cgroup: None,
                })
            }
        }
    }
}

impl fmt::Display for Tracking {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            #[cfg(target_os = "linux")]
            Tracking::Cgroups(root) => write!(f, "a cgroup per job under {}", root.display()),
            Tracking::ProcessGroups(reason) => {
                write!(f, "a process group per step, without cgroups: {reason}")
            }
        }
    }
}

pub(crate) struct Processes {
    groups: Vec<Pid>,
    #[cfg(target_os = "linux")]
    cgroup: Option<cgroup::Job>,
}

impl Processes {
    pub(crate) fn prepare(&self, command: &mut Command) {
        command.process_group(0);
        #[cfg(target_os = "linux")]
        if let Some(cgroup) = &self.cgroup {
            cgroup.join_on_exec(command);
        }
    }

    pub(crate) fn started(&mut self, group: Pid) {
        self.groups.push(group);
    }

    pub(crate) async fn kill(&mut self) {
        for group in self.groups.drain(..) {
            signal(group, Signal::KILL);
        }
        #[cfg(target_os = "linux")]
        if let Some(cgroup) = self.cgroup.take() {
            cgroup.kill().await;
        }
    }
}

pub(crate) fn signal(group: Pid, signal: Signal) {
    let _ = kill_process_group(group, signal);
}
