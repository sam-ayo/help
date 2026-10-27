//! The messages between a phone and the laptop, inside the encryption of `noise`. Each message
//! is one JSON object with a `type` field. The PWA has the same types in `pwa/src/protocol.ts`.

use serde::{Deserialize, Serialize};

/// The version of the messages. A peer with a different version gets a refusal.
pub const VERSION: u32 = 1;

/// The payload of the first handshake message of a phone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhoneHello {
    pub v: u32,
    /// A name for the list of phones on the laptop, for example "iPhone".
    pub device: String,
    /// The one-time code of the pairing link. Only the first connection of a phone has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair: Option<String>,
}

/// The payload of the handshake reply of the laptop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaptopHello {
    pub v: u32,
    pub accepted: bool,
    /// The name of the laptop, for example "Sam's MacBook Pro".
    pub name: String,
    /// Why the laptop refused the phone, if it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A message from a phone to the laptop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToLaptop {
    /// A message for the agent. It goes in the queue of the laptop. `id` is a random ID from
    /// the phone. The laptop sends it back in `Accepted`, and ignores a second prompt with the
    /// same ID, for example after a reconnection.
    Prompt { id: String, text: String },
    /// Stop the turn of the agent.
    Cancel,
    /// The answer to `Event::Permission`. `option` is `None` to reject.
    Permission { request: u64, option: Option<String> },
    /// Send the events after `after`. The phone sends it after each connection, with the last
    /// `seq` that it has, or 0.
    Sync { after: u64 },
}

/// A message from the laptop to a phone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToPhone {
    /// The state of the agent. The laptop sends it after each connection and when it changes.
    Status { agent: String, busy: bool, queued: u32 },
    /// The prompt with this ID is in the queue of the laptop.
    Accepted { id: String },
    /// One event of the conversation. `seq` goes up by 1 for each event.
    Event { seq: u64, event: Event },
    /// The laptop does not have all of the events after `Sync::after`, so the phone must clear
    /// its view. The events from `first` follow.
    Reset { first: u64 },
    Error { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pending,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionOption {
    pub id: String,
    pub name: String,
    pub allow: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanEntry {
    pub text: String,
    pub status: Status,
}

/// An event of the conversation. These follow `acp_host::Event`, without the large tool input
/// and output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A message of the user, from the laptop or from a phone.
    UserText { text: String },
    /// A part of the answer of the agent. Join the parts that follow each other.
    AgentText { text: String },
    Thought { text: String },
    /// `kind` is `"subagent"` for a subagent. `parent` is the ID of the subagent that made the
    /// tool call, if a subagent made it.
    ToolCall {
        id: String,
        title: String,
        #[serde(rename = "tool_kind")]
        kind: String,
        status: Status,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
    },
    ToolUpdate {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<Status>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    Plan { entries: Vec<PlanEntry> },
    /// The agent asks for permission. Answer with `ToLaptop::Permission`.
    Permission { request: u64, title: String, options: Vec<PermissionOption> },
    /// The permission request has an answer, from this phone or from a different place.
    PermissionClosed { request: u64 },
    TurnEnded { reason: String },
    Error { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_shape() {
        let msg = ToPhone::Event {
            seq: 7,
            event: Event::ToolCall {
                id: "t1".into(),
                title: "Read file".into(),
                kind: "read".into(),
                status: Status::Running,
                parent: None,
            },
        };
        assert_eq!(
            serde_json::to_value(&msg).unwrap(),
            json!({"type": "event", "seq": 7, "event": {
                "kind": "tool_call", "id": "t1", "title": "Read file", "tool_kind": "read", "status": "running"
            }})
        );
        let prompt: ToLaptop = serde_json::from_value(json!({"type": "prompt", "id": "a", "text": "hi"})).unwrap();
        assert_eq!(prompt, ToLaptop::Prompt { id: "a".into(), text: "hi".into() });
        let reject: ToLaptop =
            serde_json::from_value(json!({"type": "permission", "request": 3, "option": null})).unwrap();
        assert_eq!(reject, ToLaptop::Permission { request: 3, option: None });
    }
}
