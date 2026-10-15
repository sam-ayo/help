//! Keep the chats in a file, so that they are there after the app starts again.
//! A loaded chat has no agent process. When the user sends a message in it, a new agent
//! session starts and gets the earlier messages as context.

use std::{
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use acp_host::{AgentKind, ToolStatus};
use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use super::{Chat, Item, agent_from_key, agent_key};

/// The app keeps only this number of the newest chats.
const MAX_CHATS: usize = 200;
/// The earlier messages that a new session gets, in characters from the end.
const MAX_CONTEXT: usize = 20_000;
/// The app keeps this number of characters of a tool output. The chat shows no more.
const MAX_DETAIL: usize = 4_000;

#[derive(Serialize, Deserialize)]
struct SavedChat {
    agent: String,
    title: String,
    updated: i64,
    #[serde(default)]
    unread: bool,
    items: Vec<SavedItem>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SavedItem {
    User { text: String },
    Agent { text: String },
    Thought { text: String },
    Tool { title: String, kind: String, status: String, detail: Option<String> },
    Plan { entries: Vec<(String, String)> },
    Notice { text: String },
    Error { text: String },
}

fn status_key(s: ToolStatus) -> &'static str {
    match s {
        ToolStatus::Pending => "pending",
        ToolStatus::Running => "running",
        ToolStatus::Done => "done",
        ToolStatus::Failed => "failed",
    }
}

/// A tool or plan step that was running when the app stopped shows as pending, because no
/// agent runs it now.
fn status_from_key(s: &str) -> ToolStatus {
    match s {
        "done" => ToolStatus::Done,
        "failed" => ToolStatus::Failed,
        _ => ToolStatus::Pending,
    }
}

fn save_item(item: &Item) -> SavedItem {
    match item {
        Item::User(text) => SavedItem::User { text: text.clone() },
        Item::Agent(text) => SavedItem::Agent { text: text.clone() },
        Item::Thought(text) => SavedItem::Thought { text: text.clone() },
        Item::Tool { title, kind, status, detail, .. } => SavedItem::Tool {
            title: title.clone(),
            kind: kind.clone(),
            status: status_key(*status).into(),
            detail: detail.as_ref().map(|d| d.chars().take(MAX_DETAIL).collect()),
        },
        Item::Plan(entries) => SavedItem::Plan {
            entries: entries.iter().map(|(t, s)| (t.clone(), status_key(*s).into())).collect(),
        },
        Item::Notice(text) => SavedItem::Notice { text: text.clone() },
        Item::Error(text) => SavedItem::Error { text: text.clone() },
    }
}

fn load_item(item: SavedItem, ix: usize) -> Item {
    match item {
        SavedItem::User { text } => Item::User(text),
        SavedItem::Agent { text } => Item::Agent(text),
        SavedItem::Thought { text } => Item::Thought(text),
        SavedItem::Tool { title, kind, status, detail } => Item::Tool {
            id: format!("saved-{ix}"),
            title,
            kind,
            status: status_from_key(&status),
            detail,
        },
        SavedItem::Plan { entries } => {
            Item::Plan(entries.into_iter().map(|(t, s)| (t, status_from_key(&s))).collect())
        }
        SavedItem::Notice { text } => Item::Notice(text),
        SavedItem::Error { text } => Item::Error(text),
    }
}

/// The chats to write, made on the UI thread. `write` does the slow part on a different thread.
pub struct Snapshot {
    seq: u64,
    chats: Vec<SavedChat>,
}

/// Each snapshot gets a number, so that an older snapshot cannot replace a newer one.
static NEXT_SEQ: AtomicU64 = AtomicU64::new(1);
static WRITTEN_SEQ: Mutex<u64> = Mutex::new(0);

/// Take the chats to keep. A chat in which the user sent no message is not kept.
pub fn snapshot(chats: &[Chat]) -> Snapshot {
    let mut saved: Vec<&Chat> = chats
        .iter()
        .filter(|c| c.items.iter().any(|i| matches!(i, Item::User(_))))
        .collect();
    saved.sort_by_key(|c| std::cmp::Reverse(c.updated));
    saved.truncate(MAX_CHATS);
    Snapshot {
        seq: NEXT_SEQ.fetch_add(1, Ordering::Relaxed),
        chats: saved
            .into_iter()
            .map(|c| SavedChat {
                agent: agent_key(c.kind).into(),
                title: c.title.clone(),
                updated: c.updated.timestamp(),
                unread: c.unread,
                items: c.items.iter().map(save_item).collect(),
            })
            .collect(),
    }
}

/// Write a snapshot to `path`, unless a newer snapshot is there already.
pub fn write(path: &Path, snapshot: Snapshot) {
    let mut written = WRITTEN_SEQ.lock().unwrap_or_else(|e| e.into_inner());
    if snapshot.seq < *written {
        return;
    }
    let result = serde_json::to_vec(&snapshot.chats)
        .map_err(anyhow::Error::from)
        .and_then(|json| {
            // Write a new file, then rename it, so that a crash cannot leave half a file.
            let tmp = path.with_extension("tmp");
            std::fs::write(&tmp, json)?;
            std::fs::rename(&tmp, path)?;
            Ok(())
        });
    match result {
        Ok(()) => *written = snapshot.seq,
        Err(e) => tracing::warn!("cannot save the chats: {e:#}"),
    }
}

/// Read the chats from `path`. The first chat gets the id `first_id`.
pub fn load(path: &Path, first_id: u64) -> Vec<Chat> {
    let Ok(json) = std::fs::read(path) else { return vec![] };
    let saved: Vec<SavedChat> = match serde_json::from_slice(&json) {
        Ok(saved) => saved,
        Err(e) => {
            // Keep the file, so that the next save does not write over the history.
            let bad = path.with_extension("json.bad");
            tracing::warn!("cannot read the chats in {}: {e}. Moved it to {}.", path.display(), bad.display());
            let _ = std::fs::rename(path, &bad);
            return vec![];
        }
    };
    saved
        .into_iter()
        .zip(first_id..)
        .map(|(c, id)| {
            let kind = agent_from_key(&c.agent).unwrap_or(AgentKind::Claude);
            let mut chat = Chat::new(id, kind, c.title);
            chat.items = c.items.into_iter().enumerate().map(|(ix, i)| load_item(i, ix)).collect();
            chat.unread = c.unread;
            if let Some(t) = DateTime::from_timestamp(c.updated, 0) {
                chat.updated = t.with_timezone(&Local);
            }
            chat
        })
        .collect()
}

/// The first prompt of a new session in an earlier chat: the earlier messages, then the new one.
pub fn prompt_with_context(chat: &Chat, text: &str) -> String {
    let mut transcript = String::new();
    for item in &chat.items {
        match item {
            Item::User(t) => transcript.push_str(&format!("User: {t}\n\n")),
            Item::Agent(t) => transcript.push_str(&format!("Assistant: {t}\n\n")),
            _ => {}
        }
    }
    if transcript.is_empty() {
        return text.to_string();
    }
    if transcript.len() > MAX_CONTEXT {
        let mut start = transcript.len() - MAX_CONTEXT;
        while !transcript.is_char_boundary(start) {
            start += 1;
        }
        transcript = format!("[The start of the conversation is not included.]\n\n{}", &transcript[start..]);
    }
    format!(
        "This message continues an earlier conversation with a different session. \
         The earlier conversation:\n\n<conversation>\n{}</conversation>\n\nThe new message:\n\n{text}",
        transcript
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(items: Vec<Item>) -> Chat {
        let mut chat = Chat::new(1, AgentKind::Codex, "Test");
        chat.items = items;
        chat
    }

    #[test]
    fn save_then_load_keeps_the_chat() {
        let dir = std::env::temp_dir().join(format!("agent-desk-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("chats.json");
        let chats = vec![
            chat(vec![
                Item::User("hello".into()),
                Item::Tool {
                    id: "x".into(),
                    title: "ls".into(),
                    kind: "execute".into(),
                    status: ToolStatus::Running,
                    detail: Some("out".into()),
                },
                Item::Plan(vec![("step".into(), ToolStatus::Done), ("next".into(), ToolStatus::Pending)]),
            ]),
            chat(vec![]),
            chat(vec![Item::Error("no npx".into())]),
        ];
        write(&path, snapshot(&chats));
        // An older snapshot does not replace a newer one.
        let old = snapshot(&[]);
        write(&path, snapshot(&chats));
        write(&path, old);
        let loaded = load(&path, 7);

        // A file that the app cannot read is moved away, not written over.
        std::fs::write(&path, "not json").unwrap();
        assert!(load(&path, 1).is_empty());
        assert!(!path.exists() && path.with_extension("json.bad").exists());
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(loaded.len(), 1, "a chat with no user message is not kept");
        let c = &loaded[0];
        assert_eq!((c.id, c.kind, c.title.as_str()), (7, AgentKind::Codex, "Test"));
        assert!(matches!(&c.items[0], Item::User(t) if t == "hello"));
        assert!(matches!(&c.items[1], Item::Tool { status: ToolStatus::Pending, detail: Some(d), .. } if d == "out"));
        assert!(matches!(&c.items[2], Item::Plan(p) if p[0].1 == ToolStatus::Done && p[1].1 == ToolStatus::Pending));
    }

    #[test]
    fn context_has_the_earlier_messages() {
        let c = chat(vec![Item::User("first".into()), Item::Agent("answer".into())]);
        let prompt = prompt_with_context(&c, "second");
        assert!(prompt.contains("User: first") && prompt.contains("Assistant: answer"));
        assert!(prompt.ends_with("second"));
        assert_eq!(prompt_with_context(&chat(vec![]), "only"), "only");
    }
}
