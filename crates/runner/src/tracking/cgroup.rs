use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rustix::process::{Pid, Signal, kill_process};
use tokio::process::Command;
use tokio::time::{Instant, sleep};

const MOUNT: &str = "/sys/fs/cgroup";

const OWN: &str = "/proc/self/cgroup";

const PREFIX: &str = "bosun-job-";

const EMPTYING: Duration = Duration::from_secs(10);

const POLL: Duration = Duration::from_millis(10);

pub(crate) fn probe() -> Result<PathBuf, String> {
    static PROBES: AtomicU64 = AtomicU64::new(0);
    let text =
        fs::read_to_string(OWN).map_err(|error| format!("{OWN} is not readable: {error}"))?;
    let own = text
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or_else(|| format!("{OWN} names no cgroup of the unified hierarchy"))?;
    let mount = Path::new(MOUNT);
    if !mount.join("cgroup.controllers").is_file() {
        return Err(format!("{MOUNT} is not a cgroup v2 hierarchy"));
    }
    let root = mount.join(own.trim().trim_start_matches('/'));
    let number = PROBES.fetch_add(1, Ordering::Relaxed);
    let probe = root.join(format!("bosun-probe-{}-{number}", std::process::id()));
    fs::create_dir(&probe).map_err(|error| {
        format!(
            "the cgroup {} is not delegated to this process: {error}",
            root.display()
        )
    })?;
    let joinable = [root.join("cgroup.procs"), probe.join("cgroup.procs")]
        .iter()
        .try_for_each(|procs| OpenOptions::new().write(true).open(procs).map(drop));
    let _ = fs::remove_dir(&probe);
    joinable.map_err(|error| {
        format!(
            "processes cannot move into cgroups under {}: {error}",
            root.display()
        )
    })?;
    Ok(root)
}

pub(crate) struct Job {
    path: PathBuf,
    procs: Arc<File>,
}

impl Job {
    pub(crate) fn create(root: &Path, name: &str) -> io::Result<Job> {
        let name: String = name
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || character == '-' {
                    character
                } else {
                    '_'
                }
            })
            .collect();
        let path = root.join(format!("{PREFIX}{name}"));
        match fs::create_dir(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let procs = OpenOptions::new()
            .write(true)
            .open(path.join("cgroup.procs"))?;
        Ok(Job {
            path,
            procs: Arc::new(procs),
        })
    }

    pub(crate) fn join_on_exec(&self, command: &mut Command) {
        let procs = Arc::clone(&self.procs);
        unsafe {
            command.pre_exec(move || {
                rustix::io::write(&*procs, b"0")?;
                Ok(())
            });
        }
    }

    pub(crate) async fn kill(self) {
        let deadline = Instant::now() + EMPTYING;
        let killed = fs::write(self.path.join("cgroup.kill"), "1").is_ok();
        loop {
            if !killed {
                for pid in self.members() {
                    let _ = kill_process(pid, Signal::KILL);
                }
            }
            match fs::remove_dir(&self.path) {
                Ok(()) => return,
                Err(error) if error.kind() == ErrorKind::NotFound => return,
                Err(_) if Instant::now() < deadline => sleep(POLL).await,
                Err(_) => return,
            }
        }
    }

    fn members(&self) -> Vec<Pid> {
        fs::read_to_string(self.path.join("cgroup.procs"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .filter_map(Pid::from_raw)
            .collect()
    }
}
