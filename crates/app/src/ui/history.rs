//! Keep the conversation in a file, so that it is there after the app starts again.
//! A new agent session gets the earlier messages with its first prompt.

use std::{
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use acp_host::{Attachment, ToolStatus};
use serde::{Deserialize, Serialize};

use super::{Chat, Item};

/// The app keeps only this number of the newest items.
const MAX_ITEMS: usize = 5_000;
/// The earlier messages that a new session gets, in characters from the end.
const MAX_CONTEXT: usize = 40_000;
/// The app compacts the conversation when the messages after the last compaction have this
/// number of characters.
pub const COMPACT_AT: usize = 30_000;
/// A new session gets at least this number of characters of the newest messages, also when the
/// summary has them, so that it knows what the user and the agent said last.
const RECENT: usize = 6_000;
/// The new messages that a compaction gets, in characters from the end.
const MAX_COMPACT: usize = 80_000;
/// The app keeps this number of characters of a tool output. The chat shows no more.
const MAX_DETAIL: usize = 4_000;

/// A chat in the file of an older version of the app, which had many chats.
#[derive(Deserialize)]
struct LegacyChat {
    updated: i64,
    items: Vec<SavedItem>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SavedItem {
    User {
        text: String,
        /// An older version of the app kept no time.
        #[serde(default)]
        at: i64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<Attachment>,
        /// The time of the turn in seconds. An older version of the app kept no time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        took: Option<i64>,
    },
    Agent { text: String },
    AgentFiles { files: Vec<Attachment> },
    Thought { text: String },
    Tool {
        title: String,
        kind: String,
        status: String,
        detail: Option<String>,
        /// An older version of the app kept no ID and no parent.
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        parent: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<Attachment>,
        /// The time of a subagent in seconds. An older version of the app kept no time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        took: Option<i64>,
    },
    Plan { entries: Vec<(String, String)> },
    Notice { text: String },
    Error { text: String },
    Compacted {},
    /// An older version of the app had topics. The app reads them and drops them.
    #[allow(dead_code)]
    Topic {},
    /// A test build of the app had a line after the subagents of a turn. The app reads it and
    /// drops it.
    #[allow(dead_code)]
    SubagentsDone {},
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
        Item::User { text, at, attachments, took } => {
            SavedItem::User { text: text.clone(), at: *at, attachments: attachments.clone(), took: *took }
        }
        Item::Agent(text) => SavedItem::Agent { text: text.clone() },
        Item::AgentFiles(files) => SavedItem::AgentFiles { files: files.clone() },
        Item::Thought(text) => SavedItem::Thought { text: text.clone() },
        Item::Tool { id, title, kind, status, detail, parent, files, took } => SavedItem::Tool {
            id: Some(id.clone()),
            parent: parent.clone(),
            files: files.clone(),
            took: *took,
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
        Item::Compacted => SavedItem::Compacted {},
    }
}

fn load_item(item: SavedItem, ix: usize) -> Option<Item> {
    Some(match item {
        SavedItem::User { text, at, attachments, took } => Item::User { text, at, attachments, took },
        SavedItem::Agent { text } => Item::Agent(text),
        SavedItem::AgentFiles { files } => Item::AgentFiles(files),
        SavedItem::Thought { text } => Item::Thought(text),
        SavedItem::Tool { title, kind, status, detail, id, parent, files, took } => Item::Tool {
            id: id.unwrap_or_else(|| format!("saved-{ix}")),
            parent,
            files,
            took,
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
        SavedItem::Compacted {} => Item::Compacted,
        SavedItem::Topic {} | SavedItem::SubagentsDone {} => return None,
    })
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
/// version of the app from `legacy`: the oldest chat comes first.
pub fn load(path: &Path, legacy: &Path) -> Vec<Item> {
    let saved: Vec<SavedItem> = if path.exists() {
        read_json(path).unwrap_or_default()
    } else {
        let mut chats: Vec<LegacyChat> = read_json(legacy).unwrap_or_default();
        chats.sort_by_key(|c| c.updated);
        chats
            .into_iter()
            .flat_map(|c| c.items)
            .collect()
    };
    saved.into_iter().enumerate().filter_map(|(ix, i)| load_item(i, ix)).collect()
}

/// The user messages and the replies of the agent, one entry for each item. The other items
/// have no entry.
fn transcript(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match item {
            Item::User { text, at, attachments, .. } => {
                let text = format!("{text}{}", files_note(attachments));
                Some(match chrono::DateTime::from_timestamp(*at, 0) {
                    Some(t) if *at > 0 => format!("User ({}): {text}\n\n", t.format("%Y-%m-%d %H:%M UTC")),
                    _ => format!("User: {text}\n\n"),
                })
            }
            Item::Agent(text) => Some(format!("Assistant: {text}\n\n")),
            Item::AgentFiles(files) => Some(format!("Assistant:{}\n\n", files_note(files))),
            _ => None,
        })
        .collect()
}

/// The paths of the files of a message, for the transcript. A new session cannot see the
/// earlier files, but it can read them at these paths.
fn files_note(files: &[Attachment]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let paths: Vec<String> = files.iter().map(|f| f.path.display().to_string()).collect();
    format!(" [Attached files: {}]", paths.join(", "))
}

/// The index of the first item after the last compaction.
fn compacted_until(items: &[Item]) -> usize {
    items.iter().rposition(|i| matches!(i, Item::Compacted)).map_or(0, |ix| ix + 1)
}

/// Keep the last `max` characters of `text`.
fn keep_end(text: String, max: usize) -> String {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("[The start of the conversation is not included.]\n\n{}", &text[start..])
}

/// The messages that the summary does not have yet.
pub fn uncompacted(chat: &Chat) -> String {
    keep_end(transcript(&chat.items[compacted_until(&chat.items)..]).concat(), MAX_COMPACT)
}

/// The conversation is long enough to compact.
pub fn needs_compaction(chat: &Chat) -> bool {
    transcript(&chat.items[compacted_until(&chat.items)..]).iter().map(String::len).sum::<usize>() >= COMPACT_AT
}

/// The first prompt of a new session: the earlier messages, then the new one. After a
/// compaction the session gets the summary in the preamble, so it gets only the messages after
/// the compaction, and the newest messages before it.
pub fn prompt_with_context(chat: &Chat, text: &str) -> String {
    let until = compacted_until(&chat.items);
    let mut before = transcript(&chat.items[..until]);
    let mut entries = transcript(&chat.items[until..]);
    let summarized = until > 0;
    let mut recent: usize = entries.iter().map(String::len).sum();
    while recent < RECENT
        && let Some(entry) = before.pop()
    {
        recent += entry.len();
        entries.insert(0, entry);
    }
    if entries.is_empty() {
        return text.to_string();
    }
    let transcript = keep_end(entries.concat(), MAX_CONTEXT);
    let intro = if summarized {
        "This message continues an earlier conversation with a different session. The summary \
         above has the start of the conversation. The newest messages of the conversation:"
    } else {
        "This message continues an earlier conversation with a different session. \
         The earlier conversation:"
    };
    format!("{intro}\n\n<conversation>\n{transcript}</conversation>\n\nThe new message:\n\n{text}")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use acp_host::AgentKind;

    use super::*;

    fn chat(items: Vec<Item>) -> Chat {
        Chat::new(AgentKind::Codex, items, PathBuf::from("/tmp/project"))
    }

    fn file() -> Attachment {
        Attachment { path: "/tmp/shot.png".into(), name: "shot.png".into(), mime: "image/png".into(), size: 3 }
    }

    #[test]
    fn the_transcript_has_the_paths_of_the_files() {
        let c = chat(vec![Item::User { text: "see".into(), at: 0, attachments: vec![file()], took: None }]);
        assert!(prompt_with_context(&c, "next").contains("User: see [Attached files: /tmp/shot.png]"));
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("help-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn save_then_load_keeps_the_conversation() {
        let dir = temp_dir("history");
        let path = dir.join("conversation.json");
        let legacy = dir.join("chats.json");
        let c = chat(vec![
            Item::User { text: "hello".into(), at: 1_700_000_000, attachments: vec![file()], took: None },
            Item::Tool {
                id: "x".into(),
                title: "ls".into(),
                kind: "execute".into(),
                status: ToolStatus::Running,
                detail: Some("out".into()),
                parent: Some("task".into()),
                files: vec![file()],
                took: Some(80),
            },
            Item::Plan(vec![("step".into(), ToolStatus::Done), ("next".into(), ToolStatus::Pending)]),
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

        assert_eq!(loaded.len(), 3);
        assert!(matches!(&loaded[0], Item::User { text, at: 1_700_000_000, attachments, .. } if text == "hello" && attachments == &[file()]));
        assert!(matches!(&loaded[1], Item::Tool { status: ToolStatus::Pending, detail: Some(d), parent: Some(p), files, took: Some(80), .. } if d == "out" && p == "task" && files == &[file()]));
        assert!(matches!(&loaded[2], Item::Plan(p) if p[0].1 == ToolStatus::Done && p[1].1 == ToolStatus::Pending));
    }

    #[test]
    fn old_chats_become_one_conversation() {
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

        assert_eq!(loaded.len(), 2);
        assert!(matches!(&loaded[0], Item::User { text, at: 0, .. } if text == "a"));
        assert!(matches!(&loaded[1], Item::User { text, .. } if text == "b"));
    }

    #[test]
    fn a_file_with_topics_loads_without_them() {
        let dir = temp_dir("topics");
        let path = dir.join("conversation.json");
        std::fs::write(&path, r#"[{"type":"user","text":"a"},{"type":"topic","title":"Old"},{"type":"user","text":"b"}]"#)
            .unwrap();
        let loaded = load(&path, &dir.join("chats.json"));
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(loaded.len(), 2);
        assert!(matches!(&loaded[1], Item::User { text, .. } if text == "b"));
    }

    #[test]
    fn context_has_the_earlier_messages() {
        let c = chat(vec![Item::User { text: "first".into(), at: 0, attachments: Vec::new(), took: None }, Item::Agent("answer".into())]);
        let prompt = prompt_with_context(&c, "second");
        assert!(prompt.contains("User: first") && prompt.contains("Assistant: answer"));
        assert!(prompt.ends_with("second"));
        assert_eq!(prompt_with_context(&chat(vec![]), "only"), "only");
    }

    #[test]
    fn compaction_keeps_the_new_messages_and_the_newest_old_ones() {
        let long = "x".repeat(COMPACT_AT);
        let mut c = chat(vec![Item::User { text: "old".into(), at: 0, attachments: Vec::new(), took: None }, Item::Agent(long.clone())]);
        assert!(needs_compaction(&c));
        assert!(uncompacted(&c).contains("User: old"));

        c.items.push(Item::Compacted);
        assert!(!needs_compaction(&c));
        assert_eq!(uncompacted(&c), "");
        // The long reply is the newest message, so a new session gets it, but not the old message.
        let prompt = prompt_with_context(&c, "next");
        assert!(prompt.contains("summary above") && prompt.contains(&long) && !prompt.contains("User: old"));

        c.items.push(Item::User { text: "new".into(), at: 1_700_000_000, attachments: Vec::new(), took: None });
        assert!(uncompacted(&c).starts_with("User (2023-11-14 22:13 UTC): new"));

        // The marker stays in the file. `write` is not used here: its order check is global, so
        // a test that runs at the same time could skip its write.
        let json = serde_json::to_string(&save_item(&Item::Compacted)).unwrap();
        assert_eq!(json, r#"{"type":"compacted"}"#);
        let saved: SavedItem = serde_json::from_str(&json).unwrap();
        assert!(matches!(load_item(saved, 0), Some(Item::Compacted)));
    }
}
