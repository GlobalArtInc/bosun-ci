use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::de::{Error as _, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::ir::ActionReference;

pub const MOST_PATTERNS: usize = 1000;

pub const LONGEST_PATTERN: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActionPolicy {
    All {},
    Organisation {},
    Listed {
        #[serde(deserialize_with = "bounded")]
        #[schemars(length(max = MOST_PATTERNS), inner(length(max = LONGEST_PATTERN)))]
        patterns: Vec<ActionPattern>,
    },
}

fn bounded<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<ActionPattern>, D::Error> {
    struct Patterns;

    impl<'de> Visitor<'de> for Patterns {
        type Value = Vec<ActionPattern>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "a list of at most {MOST_PATTERNS} patterns")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<ActionPattern>, A::Error> {
            let mut patterns = Vec::new();
            while let Some(pattern) = seq.next_element()? {
                if patterns.len() == MOST_PATTERNS {
                    return Err(A::Error::custom(format!(
                        "a policy lists at most {MOST_PATTERNS} patterns"
                    )));
                }
                patterns.push(pattern);
            }
            Ok(patterns)
        }
    }

    deserializer.deserialize_seq(Patterns)
}

impl Default for ActionPolicy {
    fn default() -> ActionPolicy {
        ActionPolicy::All {}
    }
}

impl ActionPolicy {
    pub fn permits(&self, account: Option<&str>, action: &ActionReference) -> bool {
        match self {
            ActionPolicy::All {} => true,
            ActionPolicy::Organisation {} => {
                account.is_some_and(|account| account.eq_ignore_ascii_case(&action.owner))
            }
            ActionPolicy::Listed { patterns } => {
                patterns.iter().any(|pattern| pattern.matches(action))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
pub struct ActionPattern {
    owner: String,
    target: Target,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Owner,
    Repository {
        repository: String,
        path: Option<String>,
        git_ref: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternError(String);

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PatternError {}

const FORMS: &str = "is none of `owner/*`, `owner/repo@<ref>`, `owner/repo@*`, \
                     `owner/repo/path@<ref>` and `owner/repo/path@*`";

fn refuse(text: &str, problem: &str) -> PatternError {
    PatternError(format!("the pattern `{text}` {problem}"))
}

fn owner_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

fn repository_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

impl FromStr for ActionPattern {
    type Err = PatternError;

    fn from_str(text: &str) -> Result<ActionPattern, PatternError> {
        if text.len() > LONGEST_PATTERN {
            return Err(PatternError(format!(
                "a pattern is at most {LONGEST_PATTERN} bytes, and this one has {}",
                text.len()
            )));
        }
        if let Some(owner) = text.strip_suffix("/*") {
            if !owner_name(owner) {
                return Err(refuse(text, FORMS));
            }
            return Ok(ActionPattern {
                owner: owner.to_ascii_lowercase(),
                target: Target::Owner,
            });
        }
        let Some((name, git_ref)) = text.split_once('@') else {
            return Err(refuse(text, FORMS));
        };
        if git_ref.is_empty() || git_ref.chars().any(char::is_whitespace) {
            return Err(refuse(text, "has a ref that is empty or holds whitespace"));
        }
        if git_ref != "*" && git_ref.contains('*') {
            return Err(refuse(
                text,
                "has a ref with a `*` in it; `*` stands for every ref and nothing else",
            ));
        }
        let mut parts = name.split('/');
        let (Some(owner), Some(repository)) = (parts.next(), parts.next()) else {
            return Err(refuse(text, FORMS));
        };
        if !owner_name(owner) || !repository_name(repository) {
            return Err(refuse(text, FORMS));
        }
        let path: Vec<&str> = parts.collect();
        if path.iter().any(|segment| {
            segment.is_empty() || matches!(*segment, "." | "..") || segment.contains('*')
        }) {
            return Err(refuse(
                text,
                "has a path with an empty, `.`, `..` or `*` part",
            ));
        }
        Ok(ActionPattern {
            owner: owner.to_ascii_lowercase(),
            target: Target::Repository {
                repository: repository.to_ascii_lowercase(),
                path: (!path.is_empty()).then(|| path.join("/")),
                git_ref: (git_ref != "*").then(|| git_ref.to_owned()),
            },
        })
    }
}

impl fmt::Display for ActionPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.target {
            Target::Owner => write!(f, "{}/*", self.owner),
            Target::Repository {
                repository,
                path,
                git_ref,
            } => {
                write!(f, "{}/{repository}", self.owner)?;
                if let Some(path) = path {
                    write!(f, "/{path}")?;
                }
                write!(f, "@{}", git_ref.as_deref().unwrap_or("*"))
            }
        }
    }
}

impl TryFrom<String> for ActionPattern {
    type Error = PatternError;

    fn try_from(text: String) -> Result<ActionPattern, PatternError> {
        text.parse()
    }
}

impl From<ActionPattern> for String {
    fn from(pattern: ActionPattern) -> String {
        pattern.to_string()
    }
}

impl ActionPattern {
    pub fn matches(&self, action: &ActionReference) -> bool {
        if !self.owner.eq_ignore_ascii_case(&action.owner) {
            return false;
        }
        match &self.target {
            Target::Owner => true,
            Target::Repository {
                repository,
                path,
                git_ref,
            } => {
                repository.eq_ignore_ascii_case(&action.repository)
                    && path
                        .as_ref()
                        .is_none_or(|path| action.path.as_ref() == Some(path))
                    && git_ref
                        .as_ref()
                        .is_none_or(|git_ref| &action.r#ref == git_ref)
            }
        }
    }
}
