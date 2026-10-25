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

pub(crate) fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    if os == "linux" && musl(Path::new("/")) {
        format!("{os}-{arch}-musl")
    } else {
        format!("{os}-{arch}")
    }
}

fn musl(root: &Path) -> bool {
    let loaders: Vec<String> = ["lib", "lib64"]
        .iter()
        .filter_map(|directory| fs::read_dir(root.join(directory)).ok())
        .flatten()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    let has = |prefix: &str| loaders.iter().any(|name| name.starts_with(prefix));
    has("ld-musl-") && !has("ld-linux")
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::musl;

    #[test]
    fn a_host_is_musl_when_it_has_the_loader_of_musl_and_none_of_glibc() {
        for (files, expected) in [
            (&["lib/ld-musl-x86_64.so.1"][..], true),
            (
                &["lib/ld-musl-aarch64.so.1", "lib/libc.musl-aarch64.so.1"],
                true,
            ),
            (&["lib64/ld-linux-x86-64.so.2"], false),
            (&["lib/ld-linux-aarch64.so.1"], false),
            (
                &["lib/ld-musl-x86_64.so.1", "lib64/ld-linux-x86-64.so.2"],
                false,
            ),
            (&[], false),
        ] {
            let root = tempfile::tempdir().expect("a directory");
            for file in files {
                let path = root.path().join(file);
                fs::create_dir_all(path.parent().expect("a parent")).expect("a directory");
                fs::write(&path, b"").expect("a file");
            }
            assert_eq!(musl(root.path()), expected, "{files:?}");
        }
    }
}
