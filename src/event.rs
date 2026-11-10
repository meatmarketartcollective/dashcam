use serde::{Deserialize, Serialize};

/// One hop in an agent run. `payload` holds the full raw record as JSON;
/// `summary` is the human one-liner the replay UI leads with.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub session_id: i64,
    pub seq: i64,
    pub ts: String,
    pub kind: EventKind,
    pub summary: String,
    pub payload: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A model API turn: request as sent, response as received
    ModelTurn,
    /// A tool call and its result
    ToolCall,
    /// A user/system prompt entering the agent
    Prompt,
    /// A file diff applied to the working tree
    FileDiff,
}
