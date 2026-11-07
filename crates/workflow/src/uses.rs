use bosun_types::ir::ActionReference;

pub(crate) enum Refusal {
    Invalid(String),
}

pub(crate) enum Target {
    Remote(ActionReference),
    Local(String),
    Docker(String),
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

fn local(path: &str) -> Result<String, Refusal> {
    let parts: Vec<&str> = path.split('/').collect();
    let last = parts.len().saturating_sub(1);
    let clean = parts.iter().enumerate().all(|(position, part)| {
        *part != ".."
            && !part.chars().any(char::is_whitespace)
            && (!part.is_empty() || position == last)
    });
    if !clean {
        return Err(Refusal::Invalid(
            "has a path with an empty part, a `..` part or whitespace".to_string(),
        ));
    }
    Ok(path.to_string())
}

fn image(image: &str) -> Result<String, Refusal> {
    if image.is_empty() || image.chars().any(char::is_whitespace) {
        return Err(Refusal::Invalid(
            "names no image, or an image with whitespace".to_string(),
        ));
    }
    Ok(image.to_string())
}

pub(crate) fn target(text: &str) -> Result<Target, Refusal> {
    if let Some(path) = text.strip_prefix("./") {
        return local(path).map(Target::Local);
    }
    if let Some(name) = text.strip_prefix("docker://") {
        return image(name).map(Target::Docker);
    }
    reference(text).map(Target::Remote)
}

pub(crate) fn reference(text: &str) -> Result<ActionReference, Refusal> {
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
