use uuid::Uuid;

use crate::request::Faults;

const SLUG_RULE: &str =
    "1 to 64 lowercase letters, digits, `-`, `_` or `.`, starting with a letter or a digit";

const SLUG_LENGTH: usize = 64;

const NAME_LENGTH: usize = 255;

pub(crate) fn is_slug(text: &str) -> bool {
    let mut bytes = text.bytes();
    let starts_well = bytes
        .next()
        .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit());
    starts_well
        && text.len() <= SLUG_LENGTH
        && bytes.all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        })
}

pub(crate) fn reads_as_id(text: &str) -> bool {
    Uuid::try_parse(text).is_ok()
}

pub(crate) fn check_slug(faults: &mut Faults, field: &str, what: &str, text: &str) -> bool {
    let valid = is_slug(text);
    if !valid {
        faults.add(
            field,
            format!("`{text}` is not {what}: it must be {SLUG_RULE}"),
        );
    }
    valid
}

pub(crate) fn check_name(faults: &mut Faults, field: &str, text: &str) {
    let length = text.chars().count();
    if text.trim().is_empty() || length > NAME_LENGTH || text.chars().any(char::is_control) {
        faults.add(
            field,
            format!(
                "a name is 1 to {NAME_LENGTH} characters without control characters, not all of \
                 them white space"
            ),
        );
    }
}
