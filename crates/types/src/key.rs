use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyError {
    kind: &'static str,
    value: String,
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "`{}` is not a valid {}: it must start with a letter or `_` and contain only letters, digits, `_` and `-`",
            self.value, self.kind
        )
    }
}

impl std::error::Error for KeyError {}

fn is_valid(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

macro_rules! keys {
    ($($name:ident => $kind:literal),* $(,)?) => {
        $(
            #[derive(
                Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
            )]
            #[serde(try_from = "String", into = "String")]
            pub struct $name(String);

            impl $name {
                pub fn as_str(&self) -> &str {
                    &self.0
                }
            }

            impl TryFrom<String> for $name {
                type Error = KeyError;

                fn try_from(value: String) -> Result<Self, KeyError> {
                    if is_valid(&value) {
                        Ok(Self(value))
                    } else {
                        Err(KeyError { kind: $kind, value })
                    }
                }
            }

            impl From<$name> for String {
                fn from(key: $name) -> String {
                    key.0
                }
            }

            impl fmt::Display for $name {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    f.write_str(&self.0)
                }
            }

            impl FromStr for $name {
                type Err = KeyError;

                fn from_str(value: &str) -> Result<Self, KeyError> {
                    Self::try_from(value.to_string())
                }
            }
        )*
    };
}

keys!(JobKey => "job key", StepKey => "step key");

impl JsonSchema for JobKey {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "JobKey".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string", "pattern": "^[A-Za-z_][A-Za-z0-9_-]*$" })
    }
}
