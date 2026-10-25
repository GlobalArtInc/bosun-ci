use bosun_types::ir::ActionReference;

pub(crate) enum Refusal {
    Later(&'static str),
    Invalid(String),
}

fn owner(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn repository(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

pub(crate) fn reference(text: &str) -> Result<ActionReference, Refusal> {
    if text.starts_with("./") {
        return Err(Refusal::Later("a local action"));
    }
    if text.starts_with("docker://") {
        return Err(Refusal::Later("a container image"));
    }
    let Some((name, version)) = text.split_once('@') else {
        return Err(Refusal::Invalid(format!(
            "names no ref; write it as `{text}@<ref>`"
        )));
    };
    if version.is_empty() || version.chars().any(char::is_whitespace) {
        return Err(Refusal::Invalid(
            "has a ref that is empty or holds whitespace".to_string(),
        ));
    }
    let mut parts = name.split('/');
    let (Some(owner_name), Some(repository_name)) = (parts.next(), parts.next()) else {
        return Err(Refusal::Invalid(
            "is neither `owner/repo@ref` nor `owner/repo/path@ref`".to_string(),
        ));
    };
    if !owner(owner_name) || !repository(repository_name) {
        return Err(Refusal::Invalid(
            "is neither `owner/repo@ref` nor `owner/repo/path@ref`".to_string(),
        ));
    }
    let path: Vec<&str> = parts.collect();
    if path
        .iter()
        .any(|segment| segment.is_empty() || matches!(*segment, "." | ".."))
    {
        return Err(Refusal::Invalid(
            "has a path with an empty, `.` or `..` part".to_string(),
        ));
    }
    Ok(ActionReference {
        owner: owner_name.to_string(),
        repository: repository_name.to_string(),
        path: (!path.is_empty()).then(|| path.join("/")),
        r#ref: version.to_string(),
    })
}
