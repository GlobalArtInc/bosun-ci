use bosun_types::slug::SLUG_RULE;
pub(crate) use bosun_types::slug::is_slug;
use uuid::Uuid;

use crate::request::Faults;

const NAME_LENGTH: usize = 255;

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
