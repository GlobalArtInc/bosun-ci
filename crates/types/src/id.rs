use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdError {
    value: String,
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` is not a version 7 UUID", self.value)
    }
}

impl std::error::Error for IdError {}

fn check(value: Uuid) -> Result<Uuid, IdError> {
    if value.get_version_num() == 7 && value.get_variant() == Variant::RFC4122 {
        Ok(value)
    } else {
        Err(IdError {
            value: value.to_string(),
        })
    }
}

macro_rules! ids {
    ($($name:ident),* $(,)?) => {
        $(
            #[derive(
                Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
            )]
            #[serde(try_from = "Uuid", into = "Uuid")]
            pub struct $name(Uuid);

            impl $name {
                pub fn as_uuid(&self) -> Uuid {
                    self.0
                }
            }

            impl TryFrom<Uuid> for $name {
                type Error = IdError;

                fn try_from(value: Uuid) -> Result<Self, IdError> {
                    check(value).map(Self)
                }
            }

            impl From<$name> for Uuid {
                fn from(id: $name) -> Uuid {
                    id.0
                }
            }

            impl fmt::Display for $name {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    self.0.fmt(f)
                }
            }

            impl FromStr for $name {
                type Err = IdError;

                fn from_str(value: &str) -> Result<Self, IdError> {
                    let uuid = Uuid::parse_str(value).map_err(|_| IdError {
                        value: value.to_string(),
                    })?;
                    Self::try_from(uuid)
                }
            }

            impl JsonSchema for $name {
                fn inline_schema() -> bool {
                    true
                }

                fn schema_name() -> Cow<'static, str> {
                    stringify!($name).into()
                }

                fn json_schema(_: &mut SchemaGenerator) -> Schema {
                    json_schema!({ "type": "string", "format": "uuid" })
                }
            }
        )*
    };
}

ids!(
    OrganisationId,
    ProjectId,
    UserId,
    TokenId,
    WorkflowId,
    RevisionId,
    RunId,
    AttemptId,
    JobId,
    ExecutionId,
    StepId,
    PoolId,
    AgentId,
    RegistrationTokenId,
    SessionId,
    TeamId,
    RoleId,
    RoleBindingId,
    LabelAliasId,
    SecretId,
    VariableId,
    ScheduleId,
);
