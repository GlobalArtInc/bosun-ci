use bosun_expr::Key;
use bosun_types::diagnostic::PathSegment;
use bosun_types::ir::{CacheMode, CacheVolume, Priority, Span, VolumeMode, VolumeScope};

use crate::Code;
use crate::lower::{Entry, Lowering, child, index, optional};
use crate::surface::CACHE_VOLUME;
use crate::yaml::{Node, Value};

const SCOPES: &[(&str, VolumeScope)] = &[
    ("project", VolumeScope::Project),
    ("organisation", VolumeScope::Organisation),
];

const MODES: &[(&str, VolumeMode)] = &[
    ("exclusive", VolumeMode::Exclusive),
    ("shared", VolumeMode::Shared),
];

fn volume_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("is empty");
    }
    if name.starts_with('.') {
        return Err("starts with a `.`");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("holds a character other than letters, digits, `-`, `_` and `.`");
    }
    Ok(())
}

fn volume_path(path: &str) -> Result<(), &'static str> {
    if path.trim().is_empty() {
        return Err("is empty");
    }
    if path.contains('\0') {
        return Err("holds a NUL character");
    }
    if path.split('/').any(|part| part == "..") {
        return Err("has a `..` part");
    }
    Ok(())
}

fn locate(item: &Node, path: &[PathSegment], key: &str) -> (Span, Vec<PathSegment>) {
    if let Value::Mapping(entries) = &item.value {
        let found = entries
            .iter()
            .find(|(name, _)| matches!(&name.value, Value::String(text) if text == key));
        if let Some((_, value)) = found {
            return (value.span, child(path, key));
        }
    }
    (item.span, path.to_vec())
}

impl Lowering<'_> {
    pub(crate) fn cache_mode(&mut self, entry: &Entry<'_>, owner: &str) -> Option<CacheMode> {
        let options: Vec<(&str, CacheMode)> = CacheMode::ALL
            .iter()
            .map(|mode| (mode.name(), *mode))
            .collect();
        self.choice(entry, &format!("`cache-mode` of {owner}"), &options)
    }

    pub(crate) fn priority(&mut self, entry: &Entry<'_>, owner: &str) -> Option<Priority> {
        let options: Vec<(&str, Priority)> = Priority::ALL
            .iter()
            .map(|priority| (priority.name(), *priority))
            .collect();
        self.choice(entry, &format!("`priority` of {owner}"), &options)
    }

    pub(crate) fn cache_volumes(
        &mut self,
        entry: &Entry<'_>,
        owner: &str,
    ) -> Option<Vec<CacheVolume>> {
        let what = format!("`cache-volumes` of {owner}");
        let Value::Sequence(items) = &entry.value.value else {
            self.error(
                Code::InvalidType,
                entry.value.span,
                &entry.path,
                format!("{what} must be a list"),
            );
            return None;
        };
        let mut volumes: Vec<CacheVolume> = Vec::new();
        let mut complete = true;
        for (position, item) in items.iter().enumerate() {
            let path = index(&entry.path, position);
            let place = format!("entry {} of {what}", position + 1);
            let Some(volume) = self.cache_volume(item, &path, &place) else {
                complete = false;
                continue;
            };
            let same_name = volumes.iter().any(|seen| seen.name == volume.name);
            if same_name {
                complete = false;
                let (span, at) = locate(item, &path, "name");
                self.error(
                    Code::InvalidValue,
                    span,
                    &at,
                    format!(
                        "{place} repeats the name `{}` of an earlier entry",
                        volume.name
                    ),
                );
            }
            if volumes.iter().any(|seen| seen.path == volume.path) && !same_name {
                complete = false;
                let (span, at) = locate(item, &path, "path");
                self.error(
                    Code::InvalidValue,
                    span,
                    &at,
                    format!(
                        "{place} repeats the path `{}` of an earlier entry",
                        volume.path
                    ),
                );
            }
            volumes.push(volume);
        }
        complete.then_some(volumes)
    }

    fn volume_text(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        what: &str,
        check: fn(&str) -> Result<(), &'static str>,
    ) -> Option<String> {
        let text = self.string(node, path, what)?;
        let text = self.literal(text, node, path, what)?;
        match check(&text) {
            Ok(()) => Some(text),
            Err(reason) => {
                self.error(
                    Code::InvalidValue,
                    node.span,
                    path,
                    format!("{what} is `{text}`, which {reason}"),
                );
                None
            }
        }
    }

    fn cache_volume(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        place: &str,
    ) -> Option<CacheVolume> {
        if let Value::Mapping(_) = node.value {
            return self.volume_mapping(node, path, place);
        }
        let name = self.volume_text(node, path, &format!("the name in {place}"), volume_name)?;
        Some(CacheVolume {
            path: name.clone(),
            name,
            scope: VolumeScope::Project,
            key: None,
            mode: VolumeMode::Exclusive,
        })
    }

    fn volume_mapping(
        &mut self,
        node: &Node,
        path: &[PathSegment],
        place: &str,
    ) -> Option<CacheVolume> {
        let fields = self.fields(node, &CACHE_VOLUME, place, path, node.span)?;
        let what = |key: &str| format!("`{key}` of {place}");
        let name = fields.get("name").and_then(|field| {
            self.volume_text(field.value, &field.path, &what("name"), volume_name)
        });
        let volume_path_text = optional(fields.get("path"), |field| {
            self.volume_text(field.value, &field.path, &what("path"), volume_path)
        });
        let scope = optional(fields.get("scope"), |field| {
            self.choice(field, &what("scope"), SCOPES)
        });
        let key = optional(fields.get("key"), |field| {
            self.template(
                field.value,
                &field.path,
                &what("key"),
                Key::JobCacheVolumesKey,
            )
        });
        let mode = optional(fields.get("mode"), |field| {
            self.choice(field, &what("mode"), MODES)
        });
        if !fields.complete {
            return None;
        }
        let name = name?;
        Some(CacheVolume {
            path: volume_path_text?.unwrap_or_else(|| name.clone()),
            name,
            scope: scope?.unwrap_or(VolumeScope::Project),
            key: key?,
            mode: mode?.unwrap_or(VolumeMode::Exclusive),
        })
    }
}
