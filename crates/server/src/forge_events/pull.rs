use serde_json::Value;

pub(crate) const PULL_REQUESTS: [&str; 3] = [
    "pull_request",
    "pull_request_review",
    "pull_request_review_comment",
];

pub(crate) const TARGET: &str = "pull_request_target";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pull {
    pub(crate) number: u64,
    pub(crate) base: String,
    pub(crate) head_sha: String,
    pub(crate) merged: Option<String>,
    pub(crate) fork: bool,
}

impl Pull {
    pub(crate) fn of(payload: &Value) -> Result<Pull, String> {
        let pull = payload
            .get("pull_request")
            .filter(|pull| pull.is_object())
            .ok_or("the event names no `pull_request`")?;
        let text = |pointer: &str| {
            pull.pointer(pointer)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| {
                    format!(
                        "the pull request names no `{}`",
                        pointer[1..].replace('/', ".")
                    )
                })
        };
        let number = pull
            .get("number")
            .and_then(Value::as_u64)
            .ok_or("the pull request names no `number`")?;
        let base_repository = pull
            .pointer("/base/repo/id")
            .or_else(|| payload.pointer("/repository/id"))
            .and_then(Value::as_u64);
        let head_repository = pull.pointer("/head/repo/id").and_then(Value::as_u64);
        let merged = if pull.get("merged").and_then(Value::as_bool) == Some(true) {
            Some(text("/merge_commit_sha")?)
        } else {
            None
        };
        Ok(Pull {
            number,
            base: text("/base/ref")?,
            head_sha: text("/head/sha")?,
            merged,
            fork: head_repository.is_none() || head_repository != base_repository,
        })
    }

    pub(crate) fn base_ref(&self) -> String {
        format!("refs/heads/{}", self.base)
    }

    pub(crate) fn merge_ref(&self) -> String {
        format!("refs/pull/{}/merge", self.number)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use serde_json::{Value, json};

    use super::Pull;

    fn payload(name: &str) -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../contracts/fixtures/github")
            .join(format!("{name}.json"));
        let text =
            fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let case: Value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        case["payload"].clone()
    }

    #[test]
    fn a_pull_request_is_from_a_fork_when_its_head_lives_in_another_repository() {
        let own = Pull::of(&payload("pull-request")).expect("a pull request");
        assert_eq!(
            own,
            Pull {
                number: 2,
                base: "master".to_owned(),
                head_sha: "ec26c3e57ca3a959ca5aad62de7213c562f8c821".to_owned(),
                merged: None,
                fork: false,
            }
        );
        assert_eq!(own.merge_ref(), "refs/pull/2/merge");
        assert_eq!(own.base_ref(), "refs/heads/master");
        assert!(
            !Pull::of(&payload("pull-request-target"))
                .expect("a pull request")
                .fork
        );
        assert!(
            Pull::of(&payload("pull-request-fork"))
                .expect("a pull request")
                .fork
        );

        let mut deleted = payload("pull-request-fork");
        deleted["pull_request"]["head"]["repo"] = Value::Null;
        assert!(Pull::of(&deleted).expect("a pull request").fork);
    }

    #[test]
    fn a_merged_pull_request_names_its_merge_commit_and_a_broken_one_is_refused() {
        let mut merged = payload("pull-request-merged");
        assert_eq!(
            Pull::of(&merged).expect("a pull request").merged.as_deref(),
            Some("7638417db6d59f3c431d3e1f261cc637155684cd")
        );
        merged["pull_request"]["merge_commit_sha"] = Value::Null;
        assert_eq!(
            Pull::of(&merged),
            Err("the pull request names no `merge_commit_sha`".to_owned())
        );
        assert_eq!(
            Pull::of(&json!({ "action": "opened" })),
            Err("the event names no `pull_request`".to_owned())
        );
        let mut headless = payload("pull-request");
        headless["pull_request"]["head"]["sha"] = Value::Null;
        assert_eq!(
            Pull::of(&headless),
            Err("the pull request names no `head.sha`".to_owned())
        );
    }
}
