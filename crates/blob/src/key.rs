use std::fmt;

const MAX_KEY_BYTES: usize = 1024;
const MAX_SEGMENT_BYTES: usize = 255;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyError {
    key: String,
    reason: &'static str,
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` is not an object key: {}", self.key, self.reason)
    }
}

impl std::error::Error for KeyError {}

impl Key {
    pub fn new(key: impl Into<String>) -> Result<Self, KeyError> {
        let key = key.into();
        match problem(&key) {
            None => Ok(Self(key)),
            Some(reason) => Err(KeyError { key, reason }),
        }
    }

    pub(crate) fn segments(&self) -> std::str::Split<'_, char> {
        self.0.split('/')
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

fn problem(key: &str) -> Option<&'static str> {
    if key.is_empty() {
        return Some("it is empty");
    }
    if key.len() > MAX_KEY_BYTES {
        return Some("it is longer than 1024 bytes");
    }
    if key.chars().any(char::is_control) {
        return Some("it contains a control character");
    }
    key.split('/').find_map(segment_problem)
}

pub(crate) fn starts_some_key(prefix: &str) -> bool {
    if prefix.len() > MAX_KEY_BYTES || prefix.chars().any(char::is_control) {
        return false;
    }
    let mut segments: Vec<&str> = prefix.split('/').collect();
    let partial = segments.pop().unwrap_or_default();
    partial.len() <= MAX_SEGMENT_BYTES
        && segments
            .into_iter()
            .all(|segment| segment_problem(segment).is_none())
}

fn segment_problem(segment: &str) -> Option<&'static str> {
    if segment.is_empty() {
        Some("it has an empty segment")
    } else if segment == "." || segment == ".." {
        Some("it has a `.` or `..` segment")
    } else if segment.len() > MAX_SEGMENT_BYTES {
        Some("it has a segment longer than 255 bytes")
    } else {
        None
    }
}
