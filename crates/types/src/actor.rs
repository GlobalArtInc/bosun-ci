use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::id::{AgentId, TokenId, UserId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Actor {
    User {
        id: UserId,
        login: String,
    },
    Token {
        id: TokenId,
        user_id: UserId,
        login: String,
    },
    Agent {
        id: AgentId,
        name: String,
    },
    System,
}
