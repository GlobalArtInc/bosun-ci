use std::fs;
use std::path::{Component, Path};

use super::options::{Policy, unreadable};

const VOLUME_OPTIONS: &[&str] = &[
    "ro", "rw", "z", "Z", "copy", "nocopy", "nosuid", "suid", "nodev", "dev", "noexec", "exec",
    "private", "rprivate", "slave", "rslave", "bind", "rbind",
];

const MOUNT_SWITCHES: &[&str] = &[
    "ro",
    "rw",
    "readonly",
    "bind-nonrecursive",
    "tmpcopyup",
    "notmpcopyup",
    "nosuid",
    "suid",
    "nodev",
    "dev",
    "noexec",
    "exec",
    "copy",
    "nocopy",
];

const MOUNT_VALUES: &[&str] = &["size", "tmpfs-size", "mode", "tmpfs-mode"];

const PROPAGATION: &[&str] = &["private", "rprivate", "slave", "rslave"];

pub(crate) fn malformed(value: &str, mount: bool) -> Option<String> {
    if let Some(found) = unreadable(value) {
        return Some(format!("holds {found}"));
    }
    if let Some(found) = value.chars().find(|c| matches!(c, '"' | '\'' | '\\')) {
        return Some(format!("holds `{found}`"));
    }
    if !mount {
        return None;
    }
    let mut seen: Vec<&str> = Vec::new();
    for field in value.split(',') {
        let key = field.split_once('=').map_or(field, |(key, _)| key);
        let key = match key {
            "source" => "src",
            "destination" | "target" => "dst",
            "readonly" => "ro",
            other => other,
        };
        if seen.contains(&key) {
            return Some(format!("sets `{key}` more than once in a mount"));
        }
        seen.push(key);
    }
    None
}

pub(crate) fn volume(value: &str, policy: &Policy<'_>) -> Result<(), String> {
    let parts: Vec<&str> = value.split(':').collect();
    let (source, destination, options) = match parts.as_slice() {
        [destination] => (None, *destination, None),
        [source, destination] if destination.starts_with('/') => {
            (Some(*source), *destination, None)
        }
        [source, destination, options] => (Some(*source), *destination, Some(*options)),
        _ => return Err(format!("is a volume `{value}` that the agent cannot read")),
    };
    destination_of(destination)?;
    if let Some(options) = options {
        for option in options.split(',') {
            if !VOLUME_OPTIONS.contains(&option) {
                return Err(format!("sets the volume option `{option}`"));
            }
        }
    }
    match source {
        None => Ok(()),
        Some(source) => source_of(source, policy),
    }
}

pub(crate) fn mount(value: &str, policy: &Policy<'_>) -> Result<(), String> {
    let mut kind = "volume";
    let mut source = None;
    let mut destination = None;
    for field in value.split(',') {
        let (key, text) = match field.split_once('=') {
            Some((key, text)) => (key, Some(text)),
            None => (field, None),
        };
        match (key, text) {
            ("type", Some(text)) => kind = text,
            ("src" | "source", Some(text)) => source = Some(text),
            ("dst" | "destination" | "target", Some(text)) => destination = Some(text),
            ("bind-propagation", Some(text)) if PROPAGATION.contains(&text) => {}
            ("relabel", Some("shared" | "private")) => {}
            (key, Some(text)) if MOUNT_VALUES.contains(&key) && !text.is_empty() => {}
            (key, None | Some("true" | "false")) if MOUNT_SWITCHES.contains(&key) => {}
            _ => {
                return Err(format!(
                    "sets `{field}` in a mount, which the agent does not allow"
                ));
            }
        }
    }
    let Some(destination) = destination else {
        return Err("is a mount without a destination".to_owned());
    };
    destination_of(destination)?;
    match kind {
        "bind" => match source {
            Some(source) => host_path(source, policy),
            None => Err("is a bind mount without a source".to_owned()),
        },
        "volume" => match source {
            Some(source) => named(source, policy),
            None => Ok(()),
        },
        "tmpfs" => match source {
            None => Ok(()),
            Some(_) => Err("is a tmpfs mount with a source".to_owned()),
        },
        other => Err(format!(
            "mounts the type `{other}`, which the agent does not allow"
        )),
    }
}

fn destination_of(destination: &str) -> Result<(), String> {
    if destination.starts_with('/') {
        Ok(())
    } else {
        Err(format!(
            "mounts into `{destination}`, which is not an absolute path of the container"
        ))
    }
}

fn source_of(source: &str, policy: &Policy<'_>) -> Result<(), String> {
    if source.is_empty() {
        Err("has an empty source".to_owned())
    } else if source.starts_with(['/', '.', '~']) {
        host_path(source, policy)
    } else {
        named(source, policy)
    }
}

fn named(name: &str, policy: &Policy<'_>) -> Result<(), String> {
    let mut characters = name.chars();
    let valid = characters
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && characters.all(|next| next.is_ascii_alphanumeric() || matches!(next, '_' | '.' | '-'));
    if !valid {
        return Err(format!(
            "names the volume `{name}`, which is neither a name nor an absolute path"
        ));
    }
    if policy.untrusted {
        return Err(format!(
            "names the volume `{name}`, which would keep data between jobs"
        ));
    }
    Ok(())
}

fn host_path(path: &str, policy: &Policy<'_>) -> Result<(), String> {
    if !path.starts_with('/') {
        return Err(format!(
            "mounts `{path}`, a relative path of the host that cannot be checked against the workspace"
        ));
    }
    if Path::new(path)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(format!("mounts `{path}`, which contains `..`"));
    }
    let resolved = fs::canonicalize(path).map_err(|error| {
        format!("mounts `{path}`, which cannot be checked against the workspace: {error}")
    })?;
    if resolved.starts_with(policy.workspace) {
        Ok(())
    } else {
        Err(format!("mounts `{path}`, which is outside the workspace"))
    }
}
