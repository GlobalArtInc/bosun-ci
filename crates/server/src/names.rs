use crate::request::Faults;

const NAME_LENGTH: usize = 255;

const VALUE_BYTES: usize = 48 * 1024;

const RESERVED: &str = "GITHUB_";

pub(crate) fn check_name(faults: &mut Faults, field: &str, text: &str) -> Option<String> {
    let problem = if text.is_empty() {
        "is empty".to_owned()
    } else if text.len() > NAME_LENGTH {
        format!("is longer than {NAME_LENGTH} characters")
    } else if let Some(character) = text
        .chars()
        .find(|character| !character.is_ascii_alphanumeric() && *character != '_')
    {
        format!("holds `{character}`, and a name holds only letters, digits and `_`")
    } else if text.starts_with(|first: char| first.is_ascii_digit()) {
        "starts with a digit, which a name may not".to_owned()
    } else if text.to_ascii_uppercase().starts_with(RESERVED) {
        format!("starts with `{RESERVED}`, which only the forge's own names do")
    } else {
        return Some(text.to_ascii_uppercase());
    };
    faults.add(field, format!("`{text}` {problem}"));
    None
}

pub(crate) fn check_value(faults: &mut Faults, field: &str, text: &str) {
    if text.is_empty() {
        faults.add(field, "is empty");
    } else if text.len() > VALUE_BYTES {
        faults.add(
            field,
            format!(
                "holds {} bytes, and a value holds at most {VALUE_BYTES} (48 KiB)",
                text.len()
            ),
        );
    }
}
