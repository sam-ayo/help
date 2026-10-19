//! Keep the conversation in a file, so that it is there after the app starts again.
//! A new agent session gets the earlier messages of the current topic with its first prompt.

use std::{
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use acp_host::ToolStatus;
use serde::{Deserialize, Serialize};

use super::{Chat, Item};

/// The app keeps only this number of the newest items.
const MAX_ITEMS: usize = 5_000;
/// The earlier messages that a new session gets, in characters from the end.
const MAX_CONTEXT: usize = 20_000;
/// The app keeps this number of characters of a tool output. The chat shows no more.
const MAX_DETAIL: usize = 4_000;

/// A chat in the file of an older version of the app, which had many chats.
#[derive(Deserialize)]
struct LegacyChat {
    title: String,
    updated: i64,
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
    Topic { title: String },
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
        Item::Topic(title) => SavedItem::Topic { title: title.clone() },
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
        SavedItem::Topic { title } => Item::Topic(title),
    }
}

/// The items to write, made on the UI thread. `write` does the slow part on a different thread.
pub struct Snapshot {
    seq: u64,
    items: Vec<SavedItem>,
}

/// Each snapshot gets a number, so that an older snapshot cannot replace a newer one.
static NEXT_SEQ: AtomicU64 = AtomicU64::new(1);
static WRITTEN_SEQ: Mutex<u64> = Mutex::new(0);

/// Take the items of the conversation to keep.
pub fn snapshot(chat: &Chat) -> Snapshot {
    let skip = chat.items.len().saturating_sub(MAX_ITEMS);
    Snapshot {
        seq: NEXT_SEQ.fetch_add(1, Ordering::Relaxed),
        items: chat.items[skip..].iter().map(save_item).collect(),
    }
}

/// Write a snapshot to `path`, unless a newer snapshot is there already.
pub fn write(path: &Path, snapshot: Snapshot) {
    let mut written = WRITTEN_SEQ.lock().unwrap_or_else(|e| e.into_inner());
    if snapshot.seq < *written {
        return;
    }
    let result = serde_json::to_vec(&snapshot.items)
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
        Err(e) => tracing::warn!("cannot save the conversation: {e:#}"),
    }
}

/// Read JSON from `path`. A file that the app cannot read moves to `<name>.bad`, so that the
/// next save does not write over it.
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let json = std::fs::read(path).ok()?;
    match serde_json::from_slice(&json) {
        Ok(value) => Some(value),
        Err(e) => {
            let mut bad = path.as_os_str().to_owned();
            bad.push(".bad");
            tracing::warn!("cannot read {}: {e}. Moved it to {}.", path.display(), bad.to_string_lossy());
            let _ = std::fs::rename(path, &bad);
            None
        }
    }
}

/// Read the conversation from `path`. If that file is not there, read the chats of an older
/// version of the app from `legacy`: the oldest chat comes first, and each chat starts a topic
/// with its title.
pub fn load(path: &Path, legacy: &Path) -> Vec<Item> {
    let saved: Vec<SavedItem> = if path.exists() {
        read_json(path).unwrap_or_default()
    } else {
        let mut chats: Vec<LegacyChat> = read_json(legacy).unwrap_or_default();
        chats.sort_by_key(|c| c.updated);
        chats
            .into_iter()
            .flat_map(|c| std::iter::once(SavedItem::Topic { title: c.title }).chain(c.items))
            .collect()
    };
    saved.into_iter().enumerate().map(|(ix, i)| load_item(i, ix)).collect()
}

/// The first prompt of a new session: the earlier messages of the topic, then the new one.
pub fn prompt_with_context(chat: &Chat, text: &str) -> String {
    let mut transcript = String::new();
    for item in chat.topic_items() {
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
    use std::path::PathBuf;

    use acp_host::AgentKind;

    use super::*;

    fn chat(items: Vec<Item>) -> Chat {
        Chat::new(AgentKind::Codex, items, PathBuf::from("/tmp/project"))
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agent-desk-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_then_load_keeps_the_conversation() {
        let dir = temp_dir("history");
        let path = dir.join("conversation.json");
        let legacy = dir.join("chats.json");
        let c = chat(vec![
            Item::User("hello".into()),
            Item::Tool {
                id: "x".into(),
                title: "ls".into(),
                kind: "execute".into(),
                status: ToolStatus::Running,
                detail: Some("out".into()),
            },
            Item::Plan(vec![("step".into(), ToolStatus::Done), ("next".into(), ToolStatus::Pending)]),
            Item::Topic(String::new()),
        ]);
        write(&path, snapshot(&c));
        // An older snapshot does not replace a newer one.
        let old = snapshot(&chat(vec![]));
        write(&path, snapshot(&c));
        write(&path, old);
        let loaded = load(&path, &legacy);

        // A file that the app cannot read is moved away, not written over.
        std::fs::write(&path, "not json").unwrap();
        assert!(load(&path, &legacy).is_empty());
        assert!(!path.exists() && dir.join("conversation.json.bad").exists());
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(loaded.len(), 4);
        assert!(matches!(&loaded[0], Item::User(t) if t == "hello"));
        assert!(matches!(&loaded[1], Item::Tool { status: ToolStatus::Pending, detail: Some(d), .. } if d == "out"));
        assert!(matches!(&loaded[2], Item::Plan(p) if p[0].1 == ToolStatus::Done && p[1].1 == ToolStatus::Pending));
        assert!(matches!(&loaded[3], Item::Topic(t) if t.is_empty()));
    }

    #[test]
    fn old_chats_become_topics() {
        let dir = temp_dir("legacy");
        let path = dir.join("conversation.json");
        let legacy = dir.join("chats.json");
        std::fs::write(
            &legacy,
            r#"[{"agent":"claude","title":"Newer","updated":20,"items":[{"type":"user","text":"b"}]},
                {"agent":"codex","title":"Older","updated":10,"unread":true,"items":[{"type":"user","text":"a"}]}]"#,
        )
        .unwrap();
        let loaded = load(&path, &legacy);
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(loaded.len(), 4);
        assert!(matches!(&loaded[0], Item::Topic(t) if t == "Older"));
        assert!(matches!(&loaded[1], Item::User(t) if t == "a"));
        assert!(matches!(&loaded[2], Item::Topic(t) if t == "Newer"));
        assert!(matches!(&loaded[3], Item::User(t) if t == "b"));
    }

    #[test]
    fn context_has_the_earlier_messages_of_the_topic() {
        let c = chat(vec![
            Item::User("old".into()),
            Item::Topic(String::new()),
            Item::User("first".into()),
            Item::Agent("answer".into()),
        ]);
        let prompt = prompt_with_context(&c, "second");
        assert!(prompt.contains("User: first") && prompt.contains("Assistant: answer"));
        assert!(!prompt.contains("old"));
        assert!(prompt.ends_with("second"));
        assert_eq!(prompt_with_context(&chat(vec![]), "only"), "only");
    }
}
