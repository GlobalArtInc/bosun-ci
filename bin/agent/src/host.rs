use std::fs;
use std::path::Path;

use bosun_proto::agent::v1::Heartbeat;

pub(crate) fn os() -> &'static str {
    match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "macOS",
        "windows" => "Windows",
        other => other,
    }
}

pub(crate) fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "X64",
        "aarch64" => "ARM64",
        "x86" => "X86",
        "arm" => "ARM",
        other => other,
    }
}

pub(crate) fn hostname() -> String {
    rustix::system::uname()
        .nodename()
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn heartbeat(dir: &Path) -> Heartbeat {
    let load = fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|text| text.split_whitespace().next()?.parse().ok())
        .unwrap_or_default();
    let free_disk_bytes = rustix::fs::statvfs(dir)
        .map(|stat| stat.f_bavail.saturating_mul(stat.f_frsize))
        .unwrap_or_default();
    Heartbeat {
        load,
        free_disk_bytes,
        image_cache_bytes: 0,
    }
}
