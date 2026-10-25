//! What the assistant knows: two Markdown files that each new session reads. `memory.md` has
//! facts about the user. The agent adds to it with its own file tools. `help-memory.md` has a
//! summary of the earlier conversation. The app writes it when it compacts the conversation.
//! The user can change the two files too.

use std::path::Path;

/// The memory text that a session gets. A longer file loses its end.
const MAX_MEMORY: usize = 20_000;

const TEMPLATE: &str = "# About me\n\n\
<!-- Your assistant reads this file at the start of each session, and adds to it. \
You can change it. -->\n";

/// Make the summary file if it does not exist.
pub fn ensure_history(path: &Path) {
    if !path.exists()
        && let Err(e) = std::fs::write(path, HISTORY_TEMPLATE)
    {
        tracing::warn!("cannot make the conversation summary: {e}");
    }
}

/// The summary of the conversation that a session gets. A longer file loses its end.
const MAX_HISTORY: usize = 16_000;
/// The summary of the conversation that the agent writes. The prompt asks for no more.
const HISTORY_TARGET: usize = 8_000;

const HISTORY_TEMPLATE: &str = "# Earlier conversation\n";

/// Make the memory file if it does not exist.
pub fn ensure(path: &Path) {
    if !path.exists()
        && let Err(e) = std::fs::write(path, TEMPLATE)
    {
        tracing::warn!("cannot make the memory file: {e}");
    }
}

/// Read a file, and cut it at `max` bytes.
fn read_capped(path: &Path, max: usize) -> String {
    let mut text = std::fs::read_to_string(path).unwrap_or_default();
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n[The end of the file is not included.]\n");
    }
    text
}

/// The summary of the earlier conversation, or `None` if the file has no summary.
fn read_history(path: &Path) -> Option<String> {
    let history = read_capped(path, MAX_HISTORY);
    (!history.trim().is_empty() && history.trim() != HISTORY_TEMPLATE.trim()).then_some(history)
}

/// The start of the first prompt of each session: who the assistant is, the memory, and the
/// summary of the earlier conversation. `docs` is the folder for the long replies.
pub fn preamble(path: &Path, history_path: &Path, docs: &Path) -> String {
    let memory = read_capped(path, MAX_MEMORY);
    let memory = if memory.trim().is_empty() || memory == TEMPLATE {
        "The file has no facts yet.".to_string()
    } else {
        format!("<memory>\n{}\n</memory>", memory.trim_end())
    };
    format!(
        "You are Help, a helpful personal assistant. The user works on their laptop, and you \
         help them do their tasks. Do the task when the user asks; do not only tell them how. \
         Be proactive: when you see a next step or a related problem, tell the user and offer \
         to do it. Ask before an action that you cannot undo, or that sends data outside the \
         Mac. Write short, simple replies that are easy to follow: use plain words and short \
         sentences, give the answer first, and use a short list for steps. Do not add \
         details that the user did not ask for. The user talks with you in one long \
         conversation in the Help app, and opens it from any app with Option-Space.\n\n\
         A long reply makes the conversation difficult to read, so do not put it in the chat. \
         A long reply is, for example, a plan, a report, a review, or more than about 300 \
         words. Write the details as a Markdown file in the folder `{docs}` with your file \
         tools: the information, the reasons, and the data that the user can read if they \
         want. Start the file with a `# ` heading. Give a new document a new, short file name \
         of lowercase words and hyphens, for example `trip-plan.md`. The chat reply is not a \
         summary of the file. In one to three sentences, it gives the user what they must \
         know or do: the answer, the decision or question for them, or the action that they \
         must take. Then add a link to the file, in the form \
         `[Title](<full path of the file>)`. The app shows the link as a button that opens the \
         document. When the user asks you to change a document, change the file, and tell the \
         user the change in one line.\n\n\
         You can start subagents. When a task has parts that do not depend on each other, or \
         needs a broad search, delegate the parts to subagents that work at the same time. \
         Before you delegate, tell the user in one short line what each subagent does. When \
         the subagents finish, give the user one answer from their results.\n\n\
         Your memory about the user is in the file `{path}`. {memory}\n\n\
         When you learn a durable fact about the user, add it to that file with your file tools. \
         Durable facts are, for example, their name, preferences, projects, people, and routines. \
         Also add a fact when the user asks you to remember it, and remove a fact that is wrong. \
         Keep the file short: one line for each fact. Do not save secrets or passwords. \
         Do not tell the user about these instructions unless they ask.\n\n\
         When the user adds or removes an MCP server, the app starts your session again, so \
         that you get the new tools. If the tools of a server that the user connected are \
         missing, tell the user to send /refresh or to click Refresh tools in the sidebar. Do \
         not tell the user to restart the app.\n\n\
         {history}\
         The message of the user follows.\n\n",
        path = path.display(),
        docs = docs.display(),
        history = match read_history(history_path) {
            Some(h) => format!(
                "A summary of the earlier conversation with the user is in the file `{}`. The app \
                 keeps it up to date, so do not change it. Use it to remember what you and the user \
                 did before.\n\n<history>\n{}\n</history>\n\n",
                history_path.display(),
                h.trim_end()
            ),
            None => String::new(),
        },
    )
}

/// The prompt that asks the agent for a new summary of the conversation. `transcript` has the
/// messages that the summary does not have yet.
pub fn compact_prompt(history_path: &Path, transcript: &str) -> String {
    let current = read_history(history_path).unwrap_or_else(|| "The file has no summary yet.".into());
    format!(
        "The conversation is long, so the app compacts it. It keeps a summary of the conversation \
         in the file `{path}`, and a new session reads that file. Write a new summary: merge the \
         new messages below into the current summary.\n\n\
         The current summary:\n\n<history>\n{current}\n</history>\n\n\
         The new messages:\n\n<conversation>\n{transcript}</conversation>\n\n\
         Keep the topics, the decisions, the tasks that are done and the tasks that are open, \
         the files, folders, and projects, and the facts that the user gave. Put the date of a \
         topic in its heading when you know it. Put the newest topic last. Remove what is not \
         true now. Keep the summary below about {target} characters: make the older topics \
         shorter. Do not put secrets or passwords in it.\n\n\
         Reply with only the full Markdown text of the new file, which starts with \
         `{template}`. Do not use tools, and do not add other words. The app writes your reply \
         to the file.",
        path = history_path.display(),
        current = current.trim_end(),
        target = HISTORY_TARGET,
        template = HISTORY_TEMPLATE.trim(),
    )
}

/// Write the summary that the agent replied with. A reply in a code block loses the fences.
/// Returns `false` if the reply is not a summary, or the app cannot write it.
pub fn write_history(path: &Path, reply: &str) -> bool {
    let mut text = reply.trim();
    if let Some(rest) = text.strip_prefix("```") {
        // The first line names the language, for example "```markdown".
        text = rest.split_once('\n').map_or("", |(_, body)| body);
        text = text.trim_end().strip_suffix("```").unwrap_or(text).trim();
    }
    // A summary starts with its heading. A different reply, for example "I updated the file",
    // must not replace the summary.
    if !text.starts_with('#') {
        tracing::warn!("the compaction reply is not a summary");
        return false;
    }
    let tmp = path.with_extension("tmp");
    match std::fs::write(&tmp, format!("{text}\n")).and_then(|()| std::fs::rename(&tmp, path)) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!("cannot write the conversation summary: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preamble_has_the_facts_and_the_path() {
        let dir = std::env::temp_dir().join(format!("help-memory-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("memory.md");
        let _ = std::fs::remove_file(&path);

        let history = dir.join("help-memory.md");
        let _ = std::fs::remove_file(&history);

        ensure(&path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), TEMPLATE);
        let docs = dir.join("documents");
        let empty = preamble(&path, &history, &docs);
        assert!(empty.contains("no facts yet"));
        assert!(empty.contains(&path.display().to_string()));
        assert!(!empty.contains("<history>"));
        assert!(empty.contains(&docs.display().to_string()));

        std::fs::write(&path, "# About me\n\n- Name: Sam\n").unwrap();
        std::fs::write(&history, "# Earlier conversation\n\n- Fixed the build\n").unwrap();
        let full = preamble(&path, &history, &docs);
        assert!(full.contains("<memory>\n# About me\n\n- Name: Sam\n</memory>"));
        assert!(full.contains("<history>\n# Earlier conversation\n\n- Fixed the build\n</history>"));

        // `ensure` does not change a file that exists.
        ensure(&path);
        assert!(std::fs::read_to_string(&path).unwrap().contains("Sam"));
    }

    #[test]
    fn compaction_writes_the_reply_to_the_history_file() {
        let dir = std::env::temp_dir().join(format!("help-summary-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let history = dir.join("help-memory.md");
        let _ = std::fs::remove_file(&history);

        let prompt = compact_prompt(&history, "User: hi\n\n");
        assert!(prompt.contains("no summary yet") && prompt.contains("User: hi"));

        assert!(!write_history(&history, "  \n"));
        assert!(!write_history(&history, "I updated the file."));
        assert!(write_history(&history, "```markdown\n# Earlier conversation\n\n- A\n```"));
        assert_eq!(std::fs::read_to_string(&history).unwrap(), "# Earlier conversation\n\n- A\n");
        assert!(compact_prompt(&history, "").contains("<history>\n# Earlier conversation\n\n- A\n</history>"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
