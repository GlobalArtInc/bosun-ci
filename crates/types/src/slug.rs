pub const SLUG_RULE: &str =
    "1 to 64 lowercase letters, digits, `-`, `_` or `.`, starting with a letter or a digit";

const SLUG_LENGTH: usize = 64;

pub fn is_slug(text: &str) -> bool {
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
