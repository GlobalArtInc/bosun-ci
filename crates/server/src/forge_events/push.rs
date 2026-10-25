use bosun_forge::Comparison;
use serde_json::Value;

const BRANCHES: &str = "refs/heads/";

const TAGS: &str = "refs/tags/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Push {
    pub(crate) git_ref: String,
    pub(crate) read: Read,
    pub(crate) changes: Pending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Read {
    After(String),
    DefaultBranch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pending {
    Compare { base: String, head: String },
    Known(Changes),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Changes {
    Listed(Vec<String>),
    Unknown,
    Untouched,
}

impl Changes {
    pub(crate) fn files(&self) -> Option<&[String]> {
        match self {
            Changes::Listed(files) => Some(files),
            Changes::Unknown | Changes::Untouched => None,
        }
    }

    pub(crate) fn touches(&self, path: &str) -> bool {
        match self {
            Changes::Listed(files) => files.iter().any(|file| file == path),
            Changes::Unknown => true,
            Changes::Untouched => false,
        }
    }
}

pub(crate) fn plan(payload: &Value, default_branch: &str) -> Result<Push, String> {
    let text = |field: &str| {
        payload
            .get(field)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("the push names no `{field}`"))
    };
    let flag = |field: &str| payload.get(field).and_then(Value::as_bool) == Some(true);
    let git_ref = text("ref")?;
    if flag("deleted") {
        return Ok(Push {
            git_ref,
            read: Read::DefaultBranch,
            changes: Pending::Known(Changes::Listed(Vec::new())),
        });
    }
    let after = text("after")?;
    let changes = if git_ref.starts_with(TAGS) {
        Pending::Known(Changes::Untouched)
    } else if flag("created") {
        if git_ref.strip_prefix(BRANCHES) == Some(default_branch) {
            Pending::Known(Changes::Unknown)
        } else {
            Pending::Compare {
                base: default_branch.to_owned(),
                head: after.clone(),
            }
        }
    } else {
        Pending::Compare {
            base: text("before")?,
            head: after.clone(),
        }
    };
    Ok(Push {
        git_ref,
        read: Read::After(after),
        changes,
    })
}

pub(crate) fn compared(comparison: Option<Comparison>) -> Changes {
    match comparison {
        Some(comparison) if !comparison.truncated => Changes::Listed(comparison.files),
        Some(_) | None => Changes::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use bosun_forge::Comparison;
    use serde_json::Value;

    use super::{Changes, Pending, Read, compared, plan};

    fn cases() -> Vec<Value> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../contracts/fixtures/pushes/cases.json");
        let text =
            fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
    }

    fn text(value: &Value, field: &str) -> String {
        value[field]
            .as_str()
            .unwrap_or_else(|| panic!("no `{field}` in {value}"))
            .to_owned()
    }

    fn files(value: &Value) -> Vec<String> {
        value
            .as_array()
            .unwrap_or_else(|| panic!("{value} is not a list of files"))
            .iter()
            .map(|file| file.as_str().unwrap_or_default().to_owned())
            .collect()
    }

    fn changes(value: &Value) -> Changes {
        match value {
            Value::String(name) if name == "unknown" => Changes::Unknown,
            Value::String(name) if name == "untouched" => Changes::Untouched,
            Value::Object(object) if object.contains_key("listed") => {
                Changes::Listed(files(&object["listed"]))
            }
            other => panic!("{other} is not a description of changes"),
        }
    }

    #[test]
    fn pushes_read_their_workflows_and_their_changes_as_the_committed_cases_say() {
        let cases = cases();
        assert!(!cases.is_empty());
        for case in cases {
            let description = text(&case, "description");
            let push = plan(&case["push"], &text(&case, "default_branch"))
                .unwrap_or_else(|error| panic!("{description}: {error}"));
            let read = match case["read"].as_str() {
                Some("default_branch") => Read::DefaultBranch,
                Some("after") => Read::After(text(&case["push"], "after")),
                other => panic!("{description}: `{other:?}` is not where a push reads"),
            };
            assert_eq!(push.read, read, "{description}");
            assert_eq!(push.git_ref, text(&case["push"], "ref"), "{description}");
            let found = match (&push.changes, &case["compare"]) {
                (Pending::Compare { base, head }, Value::Object(expected)) => {
                    assert_eq!(
                        (base.as_str(), head.as_str()),
                        (
                            expected["base"].as_str().unwrap_or_default(),
                            expected["head"].as_str().unwrap_or_default()
                        ),
                        "{description}"
                    );
                    let answer = &case["comparison"];
                    compared((!answer.is_null()).then(|| Comparison {
                        files: files(&answer["files"]),
                        truncated: answer["truncated"].as_bool().unwrap_or(false),
                    }))
                }
                (Pending::Known(changes), Value::Null) => changes.clone(),
                (pending, expected) => {
                    panic!("{description}: the push compares {pending:?}, not {expected}")
                }
            };
            assert_eq!(found, changes(&case["changes"]), "{description}");
        }
    }

    #[test]
    fn a_change_is_known_by_its_list_unless_the_list_is_unknown_or_absent() {
        let listed = Changes::Listed(vec![".bosun/workflows/ci.yml".to_owned()]);
        assert!(listed.touches(".bosun/workflows/ci.yml"));
        assert!(!listed.touches(".bosun/workflows/docs.yml"));
        assert_eq!(listed.files().map(<[String]>::len), Some(1));
        assert!(Changes::Unknown.touches(".bosun/workflows/ci.yml"));
        assert_eq!(Changes::Unknown.files(), None);
        assert!(!Changes::Untouched.touches(".bosun/workflows/ci.yml"));
        assert_eq!(Changes::Untouched.files(), None);
    }
}
