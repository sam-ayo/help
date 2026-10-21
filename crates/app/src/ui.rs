//! The main window: one conversation with the assistant, as in a messages app, and a sidebar.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

use acp_host::{AgentKind, Command, Event, McpConfigWatch, PermissionOptionView, SUBAGENT_KIND, Session, ToolStatus};
use gpui::{
    AppContext as _, Context, Div, Entity, Focusable as _, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Render, SharedString, StyleRefinement, Stateful, StatefulInteractiveElement as _, Styled as _,
    Anchor, Animation, AnimationExt as _, Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::{
    ActiveTheme as _, Icon, InteractiveElementExt as _, Sizable as _, StyledExt as _, Theme, h_flex,
    button::{Button, ButtonRounded, ButtonVariants as _},
    input::{InputEvent, Textarea, TextareaState},
    menu::{DropdownMenu as _, PopupMenuItem},
    message_scroller::{MessageScroller, MessageScrollerState},
    spinner::Spinner,
    text::TextView,
    v_flex,
};
use gpui_kit_assets::IconName as KitIcon;
use tokio::sync::{mpsc, oneshot};

use crate::{Env, theme};

mod avatar;
mod demo;
mod dock;
mod history;
mod memory;

gpui::actions!(help, [StopAgent, ToggleSidebar]);

/// The app looks for a change to the MCP servers of the agent at this interval.
const MCP_POLL: Duration = Duration::from_secs(3);

enum Item {
    /// `at` is the time that the user sent the message, in seconds since 1970, or 0 if the
    /// time is not known.
    User { text: String, at: i64 },
    Agent(String),
    Thought(String),
    Tool {
        id: String,
        title: String,
        kind: String,
        status: ToolStatus,
        detail: Option<String>,
        /// The ID of the subagent tool call that made this tool call. The card of that
        /// subagent shows this tool call, so the conversation does not show it again.
        parent: Option<String>,
    },
    Plan(Vec<(String, ToolStatus)>),
    Notice(String),
    Error(String),
    /// The app compacted the conversation here. The summary in `help-memory.md` has the
    /// messages before this item.
    Compacted,
}

struct PendingPermission {
    title: String,
    options: Vec<PermissionOptionView>,
    reply: oneshot::Sender<Option<String>>,
}

/// The one conversation with the assistant.
struct Chat {
    kind: AgentKind,
    items: Vec<Item>,
    session: Option<Session>,
    /// The ID of the agent session. A refresh of the tools continues this session.
    session_id: Option<String>,
    /// The tools refresh, and the new session is not ready yet. It has the names of the new
    /// MCP servers, for the notice.
    refreshing: Option<Vec<String>>,
    /// The tools must refresh when the turn ends. It has the names of the new MCP servers.
    refresh_due: Option<Vec<String>>,
    /// Each new session gets a new number. The events of an older session do not apply.
    generation: u64,
    /// The session got the earlier messages, or the conversation had none.
    primed: bool,
    busy: bool,
    closed: bool,
    permission: Option<PendingPermission>,
    /// The folder that the agent works in.
    cwd: PathBuf,
    /// The text of the summary that the agent writes in a compaction turn, or `None` when the
    /// turn is a usual turn. See `AppView::compact`.
    compaction: Option<String>,
}

impl Chat {
    /// A conversation with no agent session.
    fn new(kind: AgentKind, items: Vec<Item>, cwd: PathBuf) -> Self {
        Self {
            kind,
            items,
            session: None,
            session_id: None,
            refreshing: None,
            refresh_due: None,
            generation: 0,
            primed: false,
            busy: false,
            closed: false,
            permission: None,
            cwd,
            compaction: None,
        }
    }
}

pub struct AppView {
    env: Env,
    chat: Chat,
    composer: Entity<TextareaState>,
    /// The rows that the conversation shows. See `build_rows`.
    rows: Vec<Row>,
    /// The scroll state of the conversation. It has one entry for each row.
    scroller: Entity<MessageScrollerState>,
    /// The mouse went down on a toolbar. A move then drags the window.
    drag_pending: bool,
    /// The sidebar shows. The user can hide it to give the conversation more room.
    sidebar_open: bool,
    /// The tool cards that the user opened or closed, by tool id. A card for a failed tool
    /// starts open. The other cards start closed.
    toggled_tools: HashSet<String>,
    /// The turns that show their steps, by the index of the user message of the turn.
    open_steps: HashSet<usize>,
    /// The files where the agent reads its MCP servers. See `poll_mcp_config`.
    mcp_watch: Option<McpConfigWatch>,
    _subs: Vec<gpui::Subscription>,
}

impl AppView {
    pub fn new(env: Env, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Message")
                .auto_grow(1, 10)
                .submit_on_enter(true)
        });
        let subs = vec![cx.subscribe_in(&composer, window, |this, _state, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { shift: false, .. } = ev {
                this.send(window, cx);
            }
        })];

        let chat = if demo::enabled() {
            demo::chat()
        } else {
            let cwd = std::fs::read_to_string(&env.folder_file)
                .ok()
                .map(|s| PathBuf::from(s.trim()))
                .filter(|p| p.is_dir())
                .unwrap_or_else(|| env.workspace.clone());
            let items = history::load(&env.conversation_file, &env.chats_file);
            Chat::new(load_agent(&env.agent_file), items, cwd)
        };
        let rows = build_rows(&chat, &HashSet::new());
        let count = rows.len();
        let scroller = cx.new(|cx| MessageScrollerState::new(count, cx));
        let mut view = Self {
            env,
            chat,
            composer,
            rows,
            scroller,
            drag_pending: false,
            sidebar_open: true,
            toggled_tools: HashSet::new(),
            open_steps: HashSet::new(),
            mcp_watch: None,
            _subs: subs,
        };
        // Render the frames of the avatar in the colors that the user picked, then show them.
        avatar::load(&view.env.avatar_file, cx);
        // The Dock icon moves while the agent works, as the "working" row does. This is not in
        // `render`, because a hidden window does not render.
        view._subs.push(cx.observe_self(|this, cx| {
            dock::set_moving(this.chat.busy && this.chat.permission.is_none(), cx)
        }));
        // The agent starts now, so that it is ready for the first message.
        view.poll_mcp_config(cx);
        view.connect(None, cx);
        // The agent reads its MCP servers only when its session starts. When the user adds or
        // removes a server, the session starts again, so that the agent gets the new tools.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(MCP_POLL).await;
                if this.update(cx, |this, cx| this.poll_mcp_config(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        // The keyboard shortcuts work only when an element in the window has the focus.
        window.focus(&view.composer.focus_handle(cx), cx);
        // Keep the conversation when the app quits, and when the window closes before that.
        cx.on_app_quit(|this, _| {
            this.save_now();
            async {}
        })
        .detach();
        cx.on_release(|this, _| this.save_now()).detach();
        view
    }

    /// Start a new agent session, and show its events in the conversation. Dropping the old
    /// session stops its agent. With `resume`, the new agent process continues that session.
    fn connect(&mut self, resume: Option<String>, cx: &mut Context<Self>) {
        // The demo starts no agents. See `demo`.
        if demo::enabled() {
            return;
        }
        let chat = &mut self.chat;
        chat.generation += 1;
        let generation = chat.generation;
        let (tx, mut rx) = mpsc::unbounded_channel();
        // A continued session has the earlier messages. If the agent cannot continue it, the
        // `Ready` event tells, and the next message gets them. See `apply`.
        chat.primed = resume.is_some();
        chat.session_id = resume.clone();
        chat.session = Some(acp_host::start(
            &self.env.runtime,
            chat.kind,
            chat.cwd.clone(),
            self.env.bridge.clone(),
            resume,
            tx,
        ));
        // A new agent process reads the MCP servers again.
        chat.refresh_due = None;
        chat.closed = false;
        chat.busy = false;
        chat.compaction = None;
        if let Some(p) = chat.permission.take() {
            let _ = p.reply.send(None);
        }
        cx.spawn(async move |this, cx| {
            while let Some(event) = rx.recv().await {
                let ok = this.update(cx, |this, cx| {
                    if this.chat.generation != generation {
                        return;
                    }
                    if this.chat.compaction.is_some() && !matches!(event, Event::Ready { .. }) {
                        this.apply_compaction(event, cx);
                    } else {
                        // Keep the conversation at the end of each turn, and when the agent stops.
                        let ended = matches!(event, Event::TurnEnded(_));
                        let save = ended || matches!(event, Event::Closed);
                        apply(&mut this.chat, event);
                        if save {
                            this.save(cx);
                        }
                        // A long conversation gets a summary, and the agent starts again with it.
                        if ended && history::needs_compaction(&this.chat) {
                            this.compact(cx);
                        }
                        // A change to the MCP servers during the turn refreshes the tools now.
                        if ended && let Some(servers) = this.chat.refresh_due.take() {
                            this.refresh_tools(servers, cx);
                        }
                    }
                    // The conversation follows the new text only when it shows the end. The user
                    // can scroll up to read while the agent works.
                    this.rows_changed(cx);
                    cx.notify();
                });
                if ok.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Keep the conversation in a file. The write runs off the UI thread. The demo is not kept.
    fn save(&self, cx: &mut Context<Self>) {
        if demo::enabled() {
            return;
        }
        let snapshot = history::snapshot(&self.chat);
        let path = self.env.conversation_file.clone();
        cx.background_spawn(async move { history::write(&path, snapshot) }).detach();
    }

    /// Keep the conversation in a file before the app stops.
    fn save_now(&self) {
        if !demo::enabled() {
            history::write(&self.env.conversation_file, history::snapshot(&self.chat));
        }
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        if text.trim().is_empty() || self.chat.busy {
            return;
        }
        // The app does the compaction, so that the summary goes to `help-memory.md`. The agent
        // does not get this command.
        if text.trim() == "/compact" && !demo::enabled() {
            self.composer.update(cx, |s, cx| s.set_value("", window, cx));
            self.compact(cx);
            self.scroll_to_end(cx);
            return;
        }
        // The app cannot see each change to the MCP servers, for example a completed login.
        if text.trim() == "/refresh" && !demo::enabled() {
            self.composer.update(cx, |s, cx| s.set_value("", window, cx));
            self.refresh_tools(Vec::new(), cx);
            self.scroll_to_end(cx);
            return;
        }
        if demo::live() {
            self.chat.items.push(Item::User { text, at: chrono::Utc::now().timestamp() });
            self.chat.busy = true;
            self.composer.update(cx, |s, cx| s.set_value("", window, cx));
            self.scroll_to_end(cx);
            cx.notify();
            cx.spawn(async move |this, cx| {
                for (delay, event) in demo::delegation() {
                    cx.background_executor().timer(delay).await;
                    let ok = this.update(cx, |this, cx| {
                        apply(&mut this.chat, event);
                        this.rows_changed(cx);
                        cx.notify();
                    });
                    if ok.is_err() {
                        break;
                    }
                }
            })
            .detach();
            return;
        }
        if demo::enabled() {
            self.chat.items.push(Item::Notice("The demo has no agent.".into()));
            self.scroll_to_end(cx);
            cx.notify();
            return;
        }
        // An agent that stopped gets a new session.
        if self.chat.session.is_none() {
            self.connect(None, cx);
        }
        let chat = &mut self.chat;
        // The first prompt of a session has the earlier messages of the conversation.
        // It also has the memory about the user.
        let prompt = if chat.primed {
            text.clone()
        } else {
            memory::preamble(&self.env.memory_file, &self.env.history_file)
                + &history::prompt_with_context(chat, &text)
        };
        let Some(session) = &chat.session else { return };
        session.send(Command::Prompt(prompt));
        chat.primed = true;
        chat.items.push(Item::User { text, at: chrono::Utc::now().timestamp() });
        chat.busy = true;
        self.composer.update(cx, |s, cx| s.set_value("", window, cx));
        self.scroll_to_end(cx);
        self.save(cx);
        cx.notify();
    }

    /// Ask the agent for a summary of the messages after the last compaction, and write it to
    /// `help-memory.md`. Then a new session starts, with the summary and the newest messages.
    /// The chat does not show the turn. See `apply_compaction`.
    fn compact(&mut self, cx: &mut Context<Self>) {
        if self.chat.busy {
            return;
        }
        let transcript = history::uncompacted(&self.chat);
        if transcript.is_empty() {
            self.chat.items.push(Item::Notice("The conversation has no new messages to compact.".into()));
            self.rows_changed(cx);
            cx.notify();
            return;
        }
        if self.chat.session.is_none() {
            self.connect(None, cx);
        }
        let Some(session) = &self.chat.session else { return };
        session.send(Command::Prompt(memory::compact_prompt(&self.env.history_file, &transcript)));
        self.chat.compaction = Some(String::new());
        self.chat.busy = true;
        self.rows_changed(cx);
        cx.notify();
    }

    /// An event of a compaction turn. The reply of the agent is the summary. The steps of the
    /// turn do not show.
    fn apply_compaction(&mut self, event: Event, cx: &mut Context<Self>) {
        let chat = &mut self.chat;
        match event {
            Event::AgentText(t) => {
                if let Some(summary) = &mut chat.compaction {
                    summary.push_str(&t);
                }
            }
            Event::TurnEnded(reason) => {
                let summary = chat.compaction.take().unwrap_or_default();
                chat.busy = false;
                if reason == "EndTurn" && memory::write_history(&self.env.history_file, &summary) {
                    self.chat.items.push(Item::Compacted);
                    self.save(cx);
                    // The new session has a small context: the summary and the newest messages.
                    self.connect(None, cx);
                } else if reason != "Cancelled" {
                    self.chat.items.push(Item::Error(format!(
                        "Cannot compact the conversation ({reason}). Send /compact to try again."
                    )));
                }
            }
            Event::Permission { reply, .. } => {
                let _ = reply.send(None);
            }
            // The turn ends after the error, and shows its own error.
            Event::Error(e) => tracing::warn!("the compaction turn failed: {e}"),
            Event::Closed => {
                chat.compaction = None;
                apply(chat, event);
            }
            _ => {}
        }
    }

    /// Start the agent again in the same session, so that it reads its MCP servers again and
    /// gets their tools. The conversation stays. During a turn, the refresh waits for the end
    /// of the turn. `servers` has the names of the new MCP servers.
    fn refresh_tools(&mut self, servers: Vec<String>, cx: &mut Context<Self>) {
        if demo::enabled() {
            return;
        }
        let chat = &mut self.chat;
        if chat.busy {
            chat.refresh_due.get_or_insert_default().extend(servers);
            return;
        }
        // An agent that does not run reads the MCP servers when its next session starts.
        if chat.session.is_none() {
            return;
        }
        let resume = chat.session_id.clone();
        chat.refreshing = Some(servers);
        self.connect(resume, cx);
        self.rows_changed(cx);
        cx.notify();
    }

    /// Refresh the tools when the MCP servers of the agent change. A different agent or folder
    /// gets a new watch: its session reads the MCP servers when it starts.
    fn poll_mcp_config(&mut self, cx: &mut Context<Self>) {
        if demo::enabled() {
            return;
        }
        let chat = &self.chat;
        match &mut self.mcp_watch {
            Some(watch) if watch.is_for(chat.kind, &chat.cwd) => {
                if let Some(servers) = watch.poll() {
                    self.refresh_tools(servers, cx);
                }
            }
            _ => self.mcp_watch = Some(McpConfigWatch::new(chat.kind, chat.cwd.clone())),
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.chat.permission.take() {
            let _ = p.reply.send(None);
        }
        if let Some(s) = &self.chat.session {
            s.send(Command::Cancel);
        }
        self.rows_changed(cx);
        cx.notify();
    }

    fn answer_permission(&mut self, option: Option<String>, cx: &mut Context<Self>) {
        if let Some(p) = self.chat.permission.take() {
            let _ = p.reply.send(option);
        }
        self.rows_changed(cx);
        cx.notify();
    }

    /// Make the rows again, and tell the scroller what changed. The rows of the current turn
    /// can change in place, so the scroller measures them again.
    fn rows_changed(&mut self, cx: &mut Context<Self>) {
        let rows = build_rows(&self.chat, &self.open_steps);
        let old = std::mem::replace(&mut self.rows, rows);
        let rows = &self.rows;
        let same = old.iter().zip(rows).take_while(|(a, b)| a == b).count();
        let turn = self.chat.items.iter().rposition(|i| matches!(i, Item::User { .. })).unwrap_or(0);
        let from = rows.iter().position(|r| matches!(r, Row::Item(ix) if *ix >= turn)).unwrap_or(rows.len());
        self.scroller.update(cx, |s, cx| {
            s.splice(same..old.len(), rows.len() - same, cx);
            let from = from.min(same);
            if from < rows.len() {
                s.remeasure_items(from..rows.len(), cx);
            }
        });
    }

    /// Show or hide the steps of the turn that starts at the user message `turn`.
    fn toggle_steps(&mut self, turn: usize, cx: &mut Context<Self>) {
        if !self.open_steps.remove(&turn) {
            self.open_steps.insert(turn);
        }
        self.rows_changed(cx);
        cx.notify();
    }

    /// Show the end of the conversation, and follow the new text again.
    fn scroll_to_end(&mut self, cx: &mut Context<Self>) {
        self.rows_changed(cx);
        self.scroller.update(cx, |s, cx| s.scroll_to_end(cx));
    }

    /// The conversation has a message from the user.
    fn started(&self) -> bool {
        self.chat.items.iter().any(|i| matches!(i, Item::User { .. }))
    }

    /// Use a different agent. The new agent gets the earlier messages with the next message.
    fn pick_agent(&mut self, kind: AgentKind, cx: &mut Context<Self>) {
        if kind == self.chat.kind || self.chat.busy {
            return;
        }
        save_agent(&self.env.agent_file, kind);
        self.chat.kind = kind;
        if self.started() {
            self.chat.items.push(Item::Notice(format!("{} continues the conversation.", agent_name(kind))));
        }
        self.connect(None, cx);
        self.scroll_to_end(cx);
        cx.notify();
    }

    /// Ask for the folder that the agent works in. The agent starts again in the new folder,
    /// and gets the earlier messages with the next message.
    fn pick_folder(&mut self, cx: &mut Context<Self>) {
        if self.chat.busy {
            return;
        }
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = paths.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |this, cx| {
                if path == this.chat.cwd || this.chat.busy {
                    return;
                }
                if let Err(e) = std::fs::write(&this.env.folder_file, path.to_string_lossy().as_bytes()) {
                    tracing::warn!("cannot save the folder: {e}");
                }
                if !this.chat.items.is_empty() {
                    this.chat.items.push(Item::Notice(format!("The agent works in {} now.", display_path(&path))));
                }
                this.chat.cwd = path;
                this.connect(None, cx);
                this.scroll_to_end(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Put the keyboard focus in the composer.
    pub(crate) fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.composer.focus_handle(cx), cx);
    }

    /// Open the memory file in the default text editor.
    fn open_memory(&self) {
        memory::ensure(&self.env.memory_file);
        if let Err(e) = std::process::Command::new("open").arg("-t").arg(&self.env.memory_file).spawn() {
            tracing::warn!("cannot open the memory file: {e}");
        }
    }

    /// Open the summary of the earlier conversation in the default text editor.
    fn open_history(&self) {
        memory::ensure_history(&self.env.history_file);
        if let Err(e) = std::process::Command::new("open").arg("-t").arg(&self.env.history_file).spawn() {
            tracing::warn!("cannot open the conversation summary: {e}");
        }
    }

    /// Put a suggested prompt in the composer, for the user to change or send.
    fn suggest(&mut self, text: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |s, cx| s.set_value(text, window, cx));
        window.focus(&self.composer.focus_handle(cx), cx);
    }
}

/// A row of the conversation.
#[derive(Clone, Copy, PartialEq)]
enum Row {
    /// The item at this index.
    Item(usize),
    /// The final reply of a turn that has steps. The reply has the button that shows or hides
    /// the steps. `turn` is the index of the user message of the turn.
    Reply { ix: usize, turn: usize, open: bool },
    /// The line at the end of a turn that has steps but no reply, for example a turn that
    /// the user stopped.
    Steps { turn: usize, count: usize, open: bool },
    /// The agent works. A permission request shows instead of this line. `steps` has the turn
    /// and the open state, if the turn has steps.
    Working { steps: Option<(usize, bool)> },
}

/// A message shows the time above it when no message before it has a time, or when the
/// message before it is at least this number of seconds older.
const TIME_GAP: i64 = 60 * 60;

/// The message at `at` shows the time above it. `prev` is the time of the message before it.
fn show_time(prev: Option<i64>, at: i64) -> bool {
    at > 0 && prev.is_none_or(|prev| at - prev >= TIME_GAP)
}

/// The time above a message, as in Messages, in two parts: the day, and the time.
/// For example: "Today" "9:41 AM", "Monday" "9:41 AM", "Mon, Oct 12" "at 9:41 AM",
/// "Oct 12, 2025" "at 9:41 AM".
fn time_label(at: i64, now: chrono::DateTime<chrono::Local>) -> Option<(String, String)> {
    use chrono::Datelike as _;
    let time = chrono::DateTime::from_timestamp(at, 0)?.with_timezone(&chrono::Local);
    let clock = time.format("%-I:%M %p").to_string();
    let days = (now.date_naive() - time.date_naive()).num_days();
    Some(match days {
        0 => ("Today".into(), clock),
        1 => ("Yesterday".into(), clock),
        2..7 => (time.format("%A").to_string(), clock),
        _ if time.year() == now.year() => (time.format("%a, %b %-d").to_string(), format!("at {clock}")),
        _ => (time.format("%b %-d, %Y").to_string(), format!("at {clock}")),
    })
}

/// Count `n` of a thing: "1 step", "2 steps".
fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

/// A turn shows the user message, the final reply of the agent, and the errors and notices.
/// The other items of the turn are steps: thoughts, tools, plans, and the text of the agent
/// before the final reply. The steps show only when the user opens them. The button that opens
/// them shows only when the pointer is on the reply. While the agent works, all of its text is
/// a step.
fn build_rows(chat: &Chat, open_steps: &HashSet<usize>) -> Vec<Row> {
    let items = &chat.items;
    let mut rows = Vec::new();
    let mut working_steps = None;
    let mut ix = 0;
    while ix < items.len() {
        if !matches!(items[ix], Item::User { .. }) {
            rows.push(Row::Item(ix));
            ix += 1;
            continue;
        }
        let turn = ix;
        let end = items[turn + 1..]
            .iter()
            .position(|i| matches!(i, Item::User { .. }))
            .map_or(items.len(), |n| turn + 1 + n);
        let working = end == items.len() && chat.busy;
        let reply = if working {
            None
        } else {
            (turn + 1..end).rev().find(|&i| matches!(items[i], Item::Agent(_)))
        };
        // The card of a subagent shows the tool calls of the subagent, so they get no rows.
        let subagents: HashSet<&str> = items[turn + 1..end]
            .iter()
            .filter_map(|i| match i {
                Item::Tool { id, kind, .. } if kind == SUBAGENT_KIND => Some(id.as_str()),
                _ => None,
            })
            .collect();
        let in_card = |i: usize| matches!(&items[i], Item::Tool { parent: Some(p), .. } if subagents.contains(p.as_str()));
        // The subagent cards always show, so the user sees that the agent delegated work.
        let is_step = |i: usize| {
            Some(i) != reply
                && !in_card(i)
                && !matches!(&items[i], Item::Tool { kind, .. } if kind == SUBAGENT_KIND)
                && !matches!(items[i], Item::Notice(_) | Item::Error(_) | Item::Compacted)
        };
        let count = (turn + 1..end).filter(|&i| is_step(i)).count();
        let open = open_steps.contains(&turn);
        rows.push(Row::Item(turn));
        for i in turn + 1..end {
            if in_card(i) {
                continue;
            }
            if count > 0 && Some(i) == reply {
                rows.push(Row::Reply { ix: i, turn, open });
            } else if open || !is_step(i) {
                rows.push(Row::Item(i));
            }
        }
        if count > 0 {
            if working {
                working_steps = Some((turn, open));
            } else if reply.is_none() {
                rows.push(Row::Steps { turn, count, open });
            }
        }
        ix = end;
    }
    if chat.busy && chat.permission.is_none() {
        rows.push(Row::Working { steps: working_steps });
    }
    rows
}

fn apply(chat: &mut Chat, event: Event) {
    match event {
        Event::Ready { session_id, resumed, .. } => {
            chat.session_id = Some(session_id);
            // A prompt that the user sent before this event did not get the earlier messages.
            if !resumed && !chat.busy {
                chat.primed = false;
            }
            if let Some(servers) = chat.refreshing.take() {
                let mut notice = if servers.is_empty() {
                    "Tools refreshed.".to_string()
                } else {
                    format!("Tools refreshed: {}.", servers.join(", "))
                };
                if !resumed {
                    notice.push_str(" The agent gets the earlier messages with your next message.");
                }
                chat.items.push(Item::Notice(notice));
            }
        }
        Event::AgentText(t) => match chat.items.last_mut() {
            Some(Item::Agent(s)) => s.push_str(&t),
            _ => chat.items.push(Item::Agent(t)),
        },
        Event::Thought(t) => match chat.items.last_mut() {
            Some(Item::Thought(s)) => s.push_str(&t),
            _ => chat.items.push(Item::Thought(t)),
        },
        Event::ToolCall { id, title, kind, status, parent } => chat.items.push(Item::Tool {
            id,
            title,
            kind,
            status,
            detail: None,
            parent,
        }),
        Event::ToolUpdate {
            id,
            title: new_title,
            status: new_status,
            detail: new_detail,
        } => {
            let found = chat.items.iter_mut().rev().find_map(|i| match i {
                Item::Tool {
                    id: tid,
                    title,
                    status,
                    detail,
                    ..
                } if *tid == id => Some((title, status, detail)),
                _ => None,
            });
            if let Some((title, status, detail)) = found {
                if let Some(t) = new_title {
                    *title = t;
                }
                if let Some(s) = new_status {
                    *status = s;
                }
                if new_detail.is_some() {
                    *detail = new_detail;
                }
            }
        }
        Event::Plan(entries) => {
            if let Some(Item::Plan(p)) = chat.items.iter_mut().rev().find(|i| matches!(i, Item::Plan(_))) {
                *p = entries;
            } else {
                chat.items.push(Item::Plan(entries));
            }
        }
        Event::Permission { title, options, reply } => {
            if let Some(old) = chat.permission.replace(PendingPermission { title, options, reply }) {
                let _ = old.reply.send(None);
            }
        }
        Event::TurnEnded(reason) => {
            chat.busy = false;
            // The user stops a turn on purpose, so a "Cancelled" notice tells them nothing new.
            if reason != "EndTurn" && reason != "Cancelled" {
                chat.items.push(Item::Notice(format!("Turn ended: {reason}")));
            }
        }
        Event::Error(e) => chat.items.push(Item::Error(e)),
        Event::Closed => {
            chat.closed = true;
            chat.busy = false;
            chat.session = None;
            chat.refreshing = None;
            chat.items.push(Item::Notice("The agent process stopped.".into()));
        }
    }
}

/// The width of the content column, as in a mail message.
const COLUMN: f32 = 760.;
/// The name of the assistant, for the user. The agent picker shows the agent kind.
const ASSISTANT_NAME: &str = "help";
/// The height of the strip at the top of the conversation, with the avatar of the assistant.
const HEADER: f32 = 84.;
/// The height of the toolbars. The traffic lights sit in the sidebar toolbar.
const TOOLBAR: f32 = 52.;
/// The height of one line in the message field: the 20 px line of the text field, with its
/// padding of 8 px above and below.
const COMPOSER_LINE: f32 = 36.;
/// The size of the send button, and the space around it in the message field.
const SEND: f32 = 28.;
const SEND_INSET: f32 = (COMPOSER_LINE - SEND) / 2.;
/// The width of the sidebar panel.
const SIDEBAR: f32 = 232.;
/// The space between the sidebar panel and the window edges.
const INSET: f32 = 8.;

fn agent_key(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "claude",
        AgentKind::Codex => "codex",
    }
}

fn agent_from_key(key: &str) -> Option<AgentKind> {
    AgentKind::ALL.into_iter().find(|&k| agent_key(k) == key.trim())
}

fn load_agent(path: &std::path::Path) -> AgentKind {
    agent_from_key(&std::fs::read_to_string(path).unwrap_or_default()).unwrap_or(AgentKind::Claude)
}

fn save_agent(path: &std::path::Path, kind: AgentKind) {
    if let Err(e) = std::fs::write(path, agent_key(kind)) {
        tracing::warn!("cannot save the agent: {e}");
    }
}

fn agent_color(kind: AgentKind) -> Hsla {
    match kind {
        AgentKind::Claude => rgb(0xD97757).into(),
        AgentKind::Codex => rgb(0x10A37F).into(),
    }
}

/// The accent of the theme, for the selected items, the links, and the send button.
fn accent(theme: &Theme) -> Hsla {
    theme.primary
}


/// The gray bubble with three dots that shows while the agent works, as the typing
/// indicator in Messages. Each dot becomes dark in turn, and the tail is two small circles.
fn typing_bubble(theme: &Theme) -> gpui::AnyElement {
    const DOT: f32 = 8.;
    let bubble = agent_bubble(theme);
    let dot = theme.muted_foreground;
    let dots = (0..3usize).map(move |i| {
        div().size(px(DOT)).rounded_full().bg(dot).with_animation(
            ("typing-dot", i),
            // All dots use the same clock, so the offsets stay in step.
            Animation::new(Duration::from_millis(1200)).repeat_synced(),
            move |el, delta| {
                // Each dot starts 0.2 of the cycle after the dot before it, and stays dark
                // for 0.5 of the cycle.
                let t = (delta - i as f32 * 0.2).rem_euclid(1.);
                let peak = if t < 0.5 { (t / 0.5 * std::f32::consts::PI).sin() } else { 0. };
                el.bg(dot.opacity(0.35 + 0.65 * peak))
            },
        )
    });
    div()
        .relative()
        .flex_none()
        .mb(px(4.))
        .child(h_flex().gap(px(4.)).px(px(14.)).py(px(12.)).rounded(px(18.)).bg(bubble).children(dots))
        .child(div().absolute().left(px(-2.)).bottom(px(-2.)).size(px(11.)).rounded_full().bg(bubble))
        .child(div().absolute().left(px(-7.)).bottom(px(-7.)).size(px(5.)).rounded_full().bg(bubble))
        .into_any_element()
}

/// The gray of a received message in Messages. It is opaque, so that the tail can be on the
/// bubble without a darker area.
fn agent_bubble(theme: &Theme) -> Hsla {
    theme.background.blend(theme.foreground.opacity(if theme.is_dark() { 0.14 } else { 0.08 }))
}

fn agent_name(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "Claude",
        AgentKind::Codex => "Codex",
    }
}

/// The space at the left of the messages of the agent, for the tail of the bubble. The
/// thoughts, tools, and plans start at the same line as the bubble.
const TAIL_INSET: f32 = 10.;

/// The round picture of the assistant, as the sender photo in Mail. It is the same for each
/// agent kind. The agent picker shows the kind. See `avatar`.
fn avatar(size: f32) -> gpui::AnyElement {
    avatar_image(None, size)
}

/// The avatar with the moving gradient, for the places that show the assistant alone.
fn live_avatar(id: &'static str, size: f32) -> gpui::AnyElement {
    avatar_image(Some(id), size)
}

/// A menu item with the three colors of an avatar palette before the label.
fn avatar_item(palette: &'static avatar::Palette) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        let [background, blob, overlay] = palette.colors();
        h_flex()
            .gap_2()
            .child(
                h_flex()
                    .child(div().size(px(12.)).rounded_full().bg(rgb(background)))
                    .child(div().ml(px(-4.)).size(px(12.)).rounded_full().bg(rgb(blob)))
                    .child(div().ml(px(-4.)).size(px(12.)).rounded_full().bg(rgb(overlay))),
            )
            .child(palette.title)
    })
}

fn avatar_image(id: Option<&'static str>, size: f32) -> gpui::AnyElement {
    let Some(images) = avatar::images() else {
        // The frames render at startup. Until then, a circle of the background color.
        let color = avatar::hex(avatar::selected().background);
        return div().flex_shrink_0().size(px(size)).rounded_full().bg(rgb(color)).into_any_element();
    };
    let source = if id.is_some() { images.moving.clone() } else { images.still.clone() };
    let image = gpui::img(source).flex_shrink_0().size(px(size));
    // GPUI plays the frames only for an image with an id.
    match id {
        Some(id) => image.id(id).into_any_element(),
        None => image.into_any_element(),
    }
}

fn status_icon(s: ToolStatus, theme: &Theme) -> gpui::AnyElement {
    match s {
        ToolStatus::Pending => Icon::new(KitIcon::Circle)
            .size(px(14.))
            .text_color(theme.muted_foreground)
            .into_any_element(),
        ToolStatus::Running => Spinner::new()
            .with_size(px(14.))
            .color(accent(theme))
            .into_any_element(),
        ToolStatus::Done => Icon::new(KitIcon::CircleCheck)
            .size(px(14.))
            .text_color(theme.success)
            .into_any_element(),
        ToolStatus::Failed => Icon::new(KitIcon::CircleX)
            .size(px(14.))
            .text_color(theme.danger)
            .into_any_element(),
    }
}

fn tool_icon(kind: &str) -> KitIcon {
    match kind {
        "read" => KitIcon::FileText,
        "edit" | "delete" | "move" => KitIcon::Pencil,
        "search" => KitIcon::Search,
        "execute" => KitIcon::Terminal,
        "fetch" => KitIcon::Globe,
        "think" => KitIcon::Brain,
        SUBAGENT_KIND => KitIcon::Bot,
        _ => KitIcon::Wrench,
    }
}

/// The button that hides or shows the sidebar.
fn sidebar_toggle(id: &'static str, tooltip: &'static str, cx: &mut Context<AppView>) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .icon(Icon::new(KitIcon::PanelLeft))
        .tooltip_with_action(tooltip, &ToggleSidebar, None)
        .on_click(cx.listener(|this, _, _, cx| {
            this.sidebar_open = !this.sidebar_open;
            cx.notify();
        }))
}

/// A menu item with a round color swatch before the label.
fn swatch_item(title: &'static str, color: Hsla) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        h_flex()
            .gap_2()
            .child(div().size(px(12.)).rounded_full().bg(color))
            .child(title)
    })
}

/// A menu item with two overlapping swatches, the base tint and the accent, before the label.
fn preset_item(title: &'static str, tint: Hsla, color: Hsla) -> PopupMenuItem {
    PopupMenuItem::element(move |_, _| {
        h_flex()
            .gap_2()
            .child(
                h_flex()
                    .child(div().size(px(12.)).rounded_full().bg(tint))
                    .child(div().ml(px(-4.)).size(px(12.)).rounded_full().bg(color)),
            )
            .child(title)
    })
}

/// A path for the user to read: the home folder shows as `~`. The folder picker gives paths
/// with links resolved, so the home folder with links resolved shows as `~` too.
fn display_path(path: &Path) -> String {
    let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from) else {
        return path.display().to_string();
    };
    let resolved = std::fs::canonicalize(&home).unwrap_or_else(|_| home.clone());
    for home in [home, resolved] {
        if let Ok(rest) = path.strip_prefix(&home) {
            return if rest.as_os_str().is_empty() { "~".into() } else { format!("~/{}", rest.display()) };
        }
    }
    path.display().to_string()
}

impl AppView {
    /// A toolbar strip that moves the window, as the title bar does.
    fn toolbar(&self, id: &'static str, cx: &mut Context<Self>) -> Stateful<Div> {
        h_flex()
            .id(id)
            .h(px(TOOLBAR))
            .flex_shrink_0()
            .items_center()
            .on_double_click(|_, window, _| window.titlebar_double_click())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.drag_pending = true),
            )
            .on_mouse_up(MouseButton::Left, cx.listener(|this, _, _, _| this.drag_pending = false))
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.drag_pending {
                    this.drag_pending = false;
                    window.start_window_move();
                }
            }))
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let accent = accent(&theme);
        // The avatar in the colors that the user picked, as the logo of a mail app.
        let brand = h_flex()
            .gap_2()
            .child(avatar(22.))
            .child(
                h_flex()
                    .text_base()
                    .font_semibold()
                    .child("help")
                    .child(div().text_color(accent).child(".")),
            );

        // The file of facts about the user that each session reads. See `memory`.
        let memory = h_flex()
            .id("memory")
            .mx_2()
            .px_2()
            .h(px(32.))
            .gap_2p5()
            .rounded(px(8.))
            .text_sm()
            .font_medium()
            .cursor_pointer()
            .hover(|el| el.bg(accent.opacity(0.08)))
            .on_click(cx.listener(|this, _, _, _| this.open_memory()))
            .child(Icon::new(KitIcon::Brain).size(px(16.)).text_color(theme.muted_foreground))
            .child(div().flex_1().child("Memory"));
        // The summary of the earlier conversation. See `AppView::compact`.
        let history = h_flex()
            .id("history")
            .mx_2()
            .px_2()
            .h(px(32.))
            .gap_2p5()
            .rounded(px(8.))
            .text_sm()
            .font_medium()
            .cursor_pointer()
            .hover(|el| el.bg(accent.opacity(0.08)))
            .on_click(cx.listener(|this, _, _, _| this.open_history()))
            .child(Icon::new(KitIcon::BookOpen).size(px(16.)).text_color(theme.muted_foreground))
            .child(div().flex_1().child("History"));
        // Start the agent again in the same session, so that it gets new MCP tools. The app
        // does this itself when it sees a change. See `AppView::refresh_tools`.
        let refresh = h_flex()
            .id("refresh-tools")
            .mx_2()
            .px_2()
            .h(px(32.))
            .gap_2p5()
            .rounded(px(8.))
            .text_sm()
            .font_medium()
            .cursor_pointer()
            .hover(|el| el.bg(accent.opacity(0.08)))
            .on_click(cx.listener(|this, _, _, cx| this.refresh_tools(Vec::new(), cx)))
            .child(Icon::new(KitIcon::RefreshCw).size(px(16.)).text_color(theme.muted_foreground))
            .child(div().flex_1().child("Refresh tools"));

        // A floating panel inset from the window edges, with a rose tint. The blurred window
        // background shows through a little.
        let panel = v_flex()
            .size_full()
            .rounded(px(16.))
            .overflow_hidden()
            .bg(theme.sidebar.opacity(0.82))
            .border_1()
            .border_color(theme.sidebar_border)
            .child(
                self.toolbar("sidebar-toolbar", cx)
                    // The panel starts below the window top. Keep the traffic lights in the middle.
                    .h(px(TOOLBAR - INSET * 2.))
                    // Leave room for the traffic lights.
                    .pl(px(76.))
                    .pr_1p5()
                    .child(div().flex_1().child(brand))
                    .child(sidebar_toggle("hide-sidebar", "Hide sidebar", cx)),
            )
            .child(div().pt_2().child(memory).child(history).child(refresh))
            .child(div().flex_1())
            .child(self.render_theme_footer(cx));
        div()
            .w(px(SIDEBAR + INSET))
            .h_full()
            .flex_shrink_0()
            .pl(px(INSET))
            .py(px(INSET))
            .child(panel)
            .into_any_element()
    }

    /// The bottom of the sidebar: a menu to pick the theme, and a button for light or dark.
    fn render_theme_footer(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let dark = theme.is_dark();
        let label = cx.global::<theme::ThemeChoice>().preset().map_or("Custom theme", |p| p.title);
        let avatar_file = self.env.avatar_file.clone();
        let view = cx.entity().downgrade();
        let (kind, busy) = (self.chat.kind, self.chat.busy);
        let menu = Button::new("theme-menu")
            .ghost()
            .small()
            .icon(Icon::new(KitIcon::Palette))
            .label(label)
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, window, cx| {
                let avatar_file = avatar_file.clone();
                let view = view.clone();
                let choice = cx.global::<theme::ThemeChoice>().clone();
                let base = choice.base;
                let accent = choice.accent;
                let appearance = choice.appearance;
                // The agent that answers. It cannot change while it works.
                let menu = menu
                    .submenu("Agent", window, cx, move |menu, _, _| {
                        AgentKind::ALL.iter().fold(menu, |menu, &k| {
                            let view = view.clone();
                            menu.item(
                                // No icon: with an icon, the menu shows no check mark.
                                PopupMenuItem::new(k.label())
                                    .checked(k == kind)
                                    .disabled(busy)
                                    .on_click(move |_, _, cx| {
                                        let _ = view.update(cx, |this, cx| this.pick_agent(k, cx));
                                    }),
                            )
                        })
                    })
                    .separator();
                // The presets first. The base and accent submenus make a custom theme.
                let menu = theme::PRESETS.iter().fold(menu.label("Themes"), |menu, p| {
                    let (tint, color) = p.swatches();
                    menu.item(
                        preset_item(p.title, tint, color)
                            .checked(p.base == base && p.accent == accent)
                            .on_click(move |_, window, cx| {
                                theme::set(Some(window), cx, |c| {
                                    c.base = p.base;
                                    c.accent = p.accent;
                                })
                            }),
                    )
                });
                menu.separator()
                    .label("Custom")
                    .submenu("Base color", window, cx, move |menu, _, _| {
                        // The base colors differ in their tint, which the muted text shows best.
                        theme::bases().fold(menu, |menu, t| {
                            menu.item(
                                swatch_item(t.title, t.swatch("muted-foreground"))
                                    .checked(t.key == base)
                                    .on_click(move |_, window, cx| theme::set(Some(window), cx, |c| c.base = t.key)),
                            )
                        })
                    })
                    .submenu("Accent color", window, cx, move |menu, _, _| {
                        let menu = menu.item(
                            PopupMenuItem::new("From the base color")
                                .checked(accent.is_none())
                                .on_click(|_, window, cx| theme::set(Some(window), cx, |c| c.accent = None)),
                        );
                        theme::accents().fold(menu.separator(), |menu, t| {
                            menu.item(
                                swatch_item(t.title, t.swatch("primary"))
                                    .checked(accent == Some(t.key))
                                    .on_click(move |_, window, cx| {
                                        theme::set(Some(window), cx, |c| c.accent = Some(t.key))
                                    }),
                            )
                        })
                    })
                    .submenu("Avatar color", window, cx, move |menu, _, _| {
                        let selected = avatar::selected().key;
                        avatar::PALETTES.iter().enumerate().fold(menu, |menu, (i, p)| {
                            let file = avatar_file.clone();
                            menu.item(
                                avatar_item(p)
                                    .checked(p.key == selected)
                                    .on_click(move |_, _, cx| avatar::select(i, file.clone(), cx)),
                            )
                        })
                    })
                    .separator()
                    .submenu("Appearance", window, cx, move |menu, _, _| {
                        theme::Appearance::ALL.into_iter().fold(menu, |menu, a| {
                            menu.item(
                                PopupMenuItem::new(a.title())
                                    .checked(a == appearance)
                                    .on_click(move |_, window, cx| theme::set(Some(window), cx, |c| c.appearance = a)),
                            )
                        })
                    })
            });
        let toggle = Button::new("appearance-toggle")
            .ghost()
            .small()
            .icon(Icon::new(if dark { KitIcon::Sun } else { KitIcon::Moon }))
            .tooltip(if dark { "Use light appearance" } else { "Use dark appearance" })
            .on_click(move |_, window, cx| {
                let next = if dark { theme::Appearance::Light } else { theme::Appearance::Dark };
                theme::set(Some(window), cx, |c| c.appearance = next)
            });
        h_flex()
            .px_2()
            .py_2()
            .gap_1()
            .border_t_1()
            .border_color(theme.sidebar_border)
            .child(menu)
            .child(div().flex_1())
            .child(toggle)
            .into_any_element()
    }

    fn render_row(&self, row: usize, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme();
        match self.rows.get(row) {
            Some(&Row::Item(ix)) => match self.chat.items.get(ix) {
                Some(item) => self.render_item(ix, item, None, cx),
                None => div().into_any_element(),
            },
            Some(&Row::Reply { ix, turn, open }) => match self.chat.items.get(ix) {
                Some(item) => self.render_item(ix, item, Some((turn, open)), cx),
                None => div().into_any_element(),
            },
            Some(&Row::Steps { turn, count, open }) => h_flex()
                .id(("steps", turn))
                .ml(px(TAIL_INSET))
                .mt(px(-8.))
                .gap_1p5()
                .flex_none()
                .self_start()
                .text_xs()
                .text_color(theme.muted_foreground)
                .cursor_pointer()
                .child(Icon::new(KitIcon::Layers).size(px(12.)))
                .child(if open { "Hide the steps".to_string() } else { plural(count, "step", "steps") })
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_steps(turn, cx)))
                .into_any_element(),
            Some(&Row::Working { steps }) => h_flex()
                .group("working")
                .items_end()
                .gap_3()
                .pl(px(TAIL_INSET))
                .child(typing_bubble(theme))
                .when_some(self.delegated(), |el, n| {
                    el.child(
                        div()
                            .pb_2()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(format!("Delegated to {}…", plural(n, "subagent", "subagents"))),
                    )
                })
                .when(self.chat.compaction.is_some(), |el| {
                    el.child(
                        div()
                            .pb_2()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("Compacting the conversation…"),
                    )
                })
                .when_some(steps, |el, (turn, open)| el.child(self.steps_button(turn, open, "working", cx)))
                .into_any_element(),
            None => div().into_any_element(),
        }
    }

    /// The number of subagents that work in the current turn, if one or more work.
    fn delegated(&self) -> Option<usize> {
        let items = &self.chat.items;
        let turn = items.iter().rposition(|i| matches!(i, Item::User { .. })).unwrap_or(0);
        let n = items[turn..]
            .iter()
            .filter(|i| {
                matches!(i, Item::Tool { kind, status: ToolStatus::Pending | ToolStatus::Running, .. }
                    if kind == SUBAGENT_KIND)
            })
            .count();
        (n > 0).then_some(n)
    }

    /// The line below the title of a subagent card: the number of steps of the subagent, and
    /// while it works, its newest step.
    fn subagent_line(&self, ix: usize, id: &str, status: ToolStatus, theme: &Theme) -> Div {
        let mut steps = 0;
        let mut newest = None;
        for item in &self.chat.items[ix + 1..] {
            if let Item::Tool { title, parent: Some(p), .. } = item
                && p == id
            {
                steps += 1;
                newest = Some(title.clone());
            }
        }
        let working = matches!(status, ToolStatus::Pending | ToolStatus::Running);
        let mut text = format!("Subagent · {}", plural(steps, "step", "steps"));
        if let Some(newest) = newest.filter(|_| working) {
            text.push_str(" · ");
            text.push_str(&newest);
        }
        h_flex()
            .px_3()
            .pb_2()
            .mt(px(-4.))
            .pl(px(34.))
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().flex_1().min_w_0().truncate().child(text))
    }

    /// The button that shows or hides the steps of a turn. It shows when the pointer is on the
    /// `group` element, and always while the steps show.
    fn steps_button(&self, turn: usize, open: bool, group: impl Into<SharedString>, cx: &mut Context<Self>) -> gpui::Div {
        let theme = cx.theme();
        div()
            .flex_shrink_0()
            .when(!open, |el| el.invisible().group_hover(group, |s| s.visible()))
            .child(
                Button::new(("steps", turn))
                    .ghost()
                    .xsmall()
                    .icon(Icon::new(KitIcon::Layers).text_color(theme.muted_foreground))
                    .tooltip(if open { "Hide the steps" } else { "Show the steps" })
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_steps(turn, cx))),
            )
    }

    /// `steps` has the turn and the open state, if the item is the final reply of a turn
    /// that has steps.
    fn render_item(
        &self,
        ix: usize,
        item: &Item,
        steps: Option<(usize, bool)>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        match item {
            Item::User { text, at } => {
                let prev = self.chat.items[..ix].iter().rev().find_map(|i| match i {
                    Item::User { at, .. } => Some(*at),
                    _ => None,
                });
                let time = show_time(prev, *at).then(|| time_label(*at, chrono::Local::now())).flatten();
                // A tint of the accent, with the normal text color, so that every theme has good
                // contrast. The tint is opaque, so that the tail can be on the bubble without a
                // darker area.
                let bubble = theme
                    .background
                    .blend(accent(&theme).opacity(if theme.is_dark() { 0.24 } else { 0.12 }));
                // The time above the message, as in Messages.
                v_flex()
                    .when_some(time, |el, (day, time)| {
                        el.child(
                            h_flex()
                                .justify_center()
                                .gap_1()
                                .pb_3()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(div().font_semibold().child(day))
                                .child(time),
                        )
                    })
                    .child(
                    h_flex()
                        .justify_end()
                        .pl(px(80.))
                        .pr(px(7.))
                        .child(
                            div()
                                .relative()
                                // Without this, a long word or URL makes the bubble wider than the pane.
                                .min_w_0()
                                .px_4()
                                .py_2p5()
                                .rounded(px(18.))
                                .bg(bubble)
                                .text_color(theme.foreground)
                                .child(TextView::markdown(("item", ix), SharedString::from(text.clone())).selectable(true))
                                // The tail at the bottom right, as a sent message in Messages.
                                .child(
                                    div()
                                        .absolute()
                                        .right(px(-7.))
                                        .bottom_0()
                                        .size(px(20.))
                                        .rounded_bl(px(16.))
                                        .bg(bubble),
                                )
                                .child(
                                    div()
                                        .absolute()
                                        .right(px(-10.))
                                        .bottom_0()
                                        .w(px(10.))
                                        .h(px(20.))
                                        .rounded_bl(px(10.))
                                        .bg(theme.background),
                                ),
                        )
                    )
                    .into_any_element()
            }
            Item::Agent(text) => {
                let group: SharedString = format!("agent-message-{ix}").into();
                let copy = text.clone();
                let bubble = agent_bubble(&theme);
                h_flex()
                    .group(group.clone())
                    .items_end()
                    .gap_3()
                    .pl(px(TAIL_INSET))
                    .pr(px(40.))
                    .child(
                        div()
                            .relative()
                            .min_w_0()
                            .px_4()
                            .py_2p5()
                            .rounded(px(18.))
                            .bg(bubble)
                            .child(TextView::markdown(("item", ix), SharedString::from(text.clone())).selectable(true))
                            // The tail at the bottom left: a gray corner, and a corner of the
                            // background color on it that cuts the curve.
                            .child(
                                div()
                                    .absolute()
                                    .left(px(-7.))
                                    .bottom_0()
                                    .size(px(20.))
                                    .rounded_br(px(16.))
                                    .bg(bubble),
                            )
                            .child(
                                div()
                                    .absolute()
                                    .left(px(-10.))
                                    .bottom_0()
                                    .w(px(10.))
                                    .h(px(20.))
                                    .rounded_br(px(10.))
                                    .bg(theme.background),
                            ),
                    )
                    // The buttons show when the pointer is on the message. They are in a column at
                    // the right, so the message gets no empty line below it.
                    .when_some(steps, |el, (turn, open)| el.child(self.steps_button(turn, open, group.clone(), cx)))
                    .child(
                        div().flex_shrink_0().invisible().group_hover(group, |s| s.visible()).child(
                            Button::new(("copy", ix))
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(KitIcon::Copy).text_color(theme.muted_foreground))
                                .tooltip("Copy")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()))
                                }),
                        ),
                    )
                    .into_any_element()
            }
            Item::Thought(text) => h_flex()
                .items_start()
                .gap_2()
                .pl(px(TAIL_INSET))
                .text_color(theme.muted_foreground)
                .child(Icon::new(KitIcon::Brain).size(px(14.)).mt_0p5())
                .child(div().flex_1().min_w_0().text_sm().italic().child(text.clone()))
                .into_any_element(),
            Item::Tool {
                id,
                title,
                kind: tool_kind,
                status,
                detail,
                ..
            } => {
                // A click on the header opens or closes the output. A failed tool starts open.
                let key = id.clone();
                let open = detail.is_some() && (self.toggled_tools.contains(&key) != (*status == ToolStatus::Failed));
                v_flex()
                    .ml(px(TAIL_INSET))
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.foreground.opacity(0.02))
                    .overflow_hidden()
                    .child(
                        h_flex()
                            .id(("tool", ix))
                            .px_3()
                            .py_2()
                            .gap_2()
                            .when(detail.is_some(), |el| {
                                el.cursor_pointer()
                                    .hover(|el| el.bg(theme.foreground.opacity(0.03)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if !this.toggled_tools.remove(&key) {
                                            this.toggled_tools.insert(key.clone());
                                        }
                                        if let Some(row) = this.rows.iter().position(|r| *r == Row::Item(ix)) {
                                            this.scroller.update(cx, |s, cx| {
                                                s.remeasure_items(row..row + 1, cx);
                                            });
                                        }
                                        cx.notify();
                                    }))
                            })
                            .child(Icon::new(tool_icon(tool_kind)).size(px(14.)).text_color(theme.muted_foreground))
                            .child(div().flex_1().min_w_0().truncate().text_sm().child(title.clone()))
                            // Claude Code keeps a subagent call pending while the subagent works.
                            .child(status_icon(
                                if tool_kind == SUBAGENT_KIND && *status == ToolStatus::Pending && self.chat.busy {
                                    ToolStatus::Running
                                } else {
                                    *status
                                },
                                &theme,
                            ))
                            .when(detail.is_some(), |el| {
                                el.child(
                                    Icon::new(if open { KitIcon::ChevronDown } else { KitIcon::ChevronRight })
                                        .size(px(14.))
                                        .text_color(theme.muted_foreground),
                                )
                            }),
                    )
                    .when(tool_kind == SUBAGENT_KIND, |el| el.child(self.subagent_line(ix, id, *status, &theme)))
                    .when_some(detail.clone().filter(|_| open), |el, d| {
                        let short: String = d.chars().take(4000).collect();
                        el.child(
                            div()
                                .id(("tool-detail", ix))
                                .px_3()
                                .py_2()
                                .max_h(px(240.))
                                .overflow_y_scroll()
                                .border_t_1()
                                .border_color(theme.border)
                                .bg(theme.foreground.opacity(0.03))
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.muted_foreground)
                                .child(short),
                        )
                    })
                    .into_any_element()
            }
            Item::Plan(entries) => {
                let done = entries.iter().filter(|(_, s)| *s == ToolStatus::Done).count();
                v_flex()
                    .ml(px(TAIL_INSET))
                    .p_3()
                    .gap_1p5()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.foreground.opacity(0.02))
                    .child(
                        h_flex()
                            .gap_2()
                            .pb_1()
                            .child(Icon::new(KitIcon::ListTodo).size(px(14.)))
                            .child(div().flex_1().text_sm().font_semibold().child("Plan"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{done} of {}", entries.len())),
                            ),
                    )
                    .children(entries.iter().map(|(text, s)| {
                        h_flex()
                            .gap_2()
                            .items_start()
                            .child(div().mt_0p5().child(status_icon(*s, &theme)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .when(*s == ToolStatus::Done, |el| {
                                        el.text_color(theme.muted_foreground).line_through()
                                    })
                                    .child(text.clone()),
                            )
                    }))
                    .into_any_element()
            }
            Item::Notice(text) => h_flex()
                .justify_center()
                .child(
                    div()
                        .min_w_0()
                        .px_3()
                        .py_1()
                        .rounded_full()
                        .text_center()
                        .bg(theme.foreground.opacity(0.05))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(text.clone()),
                )
                .into_any_element(),
            Item::Compacted => h_flex()
                .gap_3()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(div().flex_1().h(px(1.)).bg(theme.border))
                .child(
                    h_flex()
                        .id(("compacted", ix))
                        .gap_1p5()
                        .cursor_pointer()
                        .hover(|el| el.text_color(theme.foreground))
                        .on_click(cx.listener(|this, _, _, _| this.open_history()))
                        .child(Icon::new(KitIcon::Brain).size(px(12.)))
                        .child("The summary in help-memory.md has the messages above"),
                )
                .child(div().flex_1().h(px(1.)).bg(theme.border))
                .into_any_element(),
            // An error uses the full width of the column, as the permission card does.
            Item::Error(text) => h_flex()
                .px_3()
                .py_2()
                .gap_2()
                .items_start()
                .rounded_lg()
                .bg(theme.danger.opacity(0.08))
                .border_1()
                .border_color(theme.danger.opacity(0.3))
                .text_sm()
                .text_color(theme.danger)
                .child(Icon::new(KitIcon::CircleAlert).size(px(14.)).mt_1())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(TextView::markdown(("item", ix), SharedString::from(text.clone())).selectable(true)),
                )
                .into_any_element(),
        }
    }

    fn render_permission(&self, p: &PendingPermission, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        // One primary button, for the usual answer. Two primary buttons side by side compete.
        let first_allow = p.options.iter().position(|o| o.allow);
        v_flex()
            .gap_3()
            .p_3()
            .rounded_xl()
            .bg(theme.warning.opacity(0.08))
            .border_1()
            .border_color(theme.warning.opacity(0.4))
            .child(
                h_flex()
                    .gap_2()
                    .items_start()
                    .child(Icon::new(KitIcon::ShieldAlert).size(px(16.)).text_color(theme.warning))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_semibold().child("The agent asks for permission"))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(TextView::markdown("permission-title", SharedString::from(p.title.clone()))),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .justify_end()
                    .children(p.options.iter().enumerate().map(|(i, o)| {
                        let id = o.id.clone();
                        let button = Button::new(("perm", i)).small().label(o.name.clone());
                        let button = if Some(i) == first_allow { button.primary() } else { button.outline() };
                        button.on_click(cx.listener(move |this, _, _, cx| this.answer_permission(Some(id.clone()), cx)))
                    })),
            )
        .into_any_element()
    }

    /// The top of a new chat: the agent, a question, and the folder that the agent works in.
    fn render_greeting(&self, cwd: &Path, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        v_flex()
            .items_center()
            .gap_3()
            .px_6()
            .child(live_avatar("avatar-greeting", 44.))
            .child(
                div()
                    .text_2xl()
                    .font_semibold()
                    .child("What can I help you with?"),
            )
            // A click on the folder opens the folder picker.
            .child(
                h_flex()
                    .id("pick-folder")
                    .px_2p5()
                    .py_1()
                    .gap_1p5()
                    .rounded_full()
                    .cursor_pointer()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .hover(|el| el.bg(theme.secondary).text_color(theme.foreground))
                    .on_click(cx.listener(|this, _, _, cx| this.pick_folder(cx)))
                    .child(Icon::new(KitIcon::Folder).size(px(14.)))
                    .child(display_path(cwd))
                    .child(Icon::new(KitIcon::ChevronDown).size(px(12.))),
            )
            .into_any_element()
    }

    /// Prompts to start from. A click puts the prompt in the composer.
    fn render_suggestions(&self, kind: AgentKind, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        const SUGGESTIONS: [(KitIcon, &str, &str); 3] = [
            (KitIcon::BookOpen, "Explain this project", "Explain how this project works. Start with the main parts."),
            (KitIcon::Bug, "Find a bug", "Find a bug in this project and fix it. Tell me what you changed."),
            (KitIcon::FlaskConical, "Write tests", "Write tests for the part of this project with the fewest tests."),
        ];
        h_flex()
            .w_full()
            .max_w(px(COLUMN))
            .mx_auto()
            .px_6()
            .gap_2()
            .flex_wrap()
            .justify_center()
            .children(SUGGESTIONS.iter().enumerate().map(|(i, &(icon, label, prompt))| {
                h_flex()
                    .id(("suggestion", i))
                    .px_3()
                    .py_1p5()
                    .gap_1p5()
                    .rounded_full()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .cursor_pointer()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .hover(|el| el.bg(theme.secondary).text_color(theme.foreground))
                    .on_click(cx.listener(move |this, _, window, cx| this.suggest(prompt, window, cx)))
                    .child(Icon::new(icon).size(px(14.)).text_color(agent_color(kind)))
                    .child(label)
            }))
            .into_any_element()
    }

    /// The agent icon in the composer. A click opens a menu to change the agent.
    /// The field to write a message, as in Messages: a "+" button for more actions, and a
    /// round field with the send button at its end.
    fn render_composer(&self, busy: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let action = if busy {
            Button::new("stop")
                .with_size(px(SEND))
                .rounded(ButtonRounded::Size(px(SEND / 2.)))
                .icon(Icon::new(KitIcon::Square))
                .tooltip("Stop")
                .on_click(cx.listener(|this, _, _, cx| this.cancel(cx)))
        } else {
            Button::new("send")
                .with_size(px(SEND))
                .primary()
                .rounded(ButtonRounded::Size(px(SEND / 2.)))
                .icon(Icon::new(KitIcon::ArrowUp))
                .tooltip("Send")
                .on_click(cx.listener(|this, _, window, cx| this.send(window, cx)))
        };
        // The actions that are not about the message.
        let view = cx.entity().downgrade();
        let more = Button::new("composer-more")
            .with_size(px(COMPOSER_LINE + 2.))
            .secondary()
            .rounded(ButtonRounded::Size(px(COMPOSER_LINE / 2. + 1.)))
            .icon(Icon::new(KitIcon::Plus).text_color(theme.muted_foreground))
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                let folder = view.clone();
                menu.item(
                    PopupMenuItem::new("Change the folder…")
                        .icon(Icon::new(KitIcon::Folder))
                        .disabled(busy)
                        .on_click(move |_, _, cx| {
                            let _ = folder.update(cx, |this, cx| this.pick_folder(cx));
                        }),
                )
            });
        h_flex()
            .items_end()
            .gap_2()
            .child(more)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_end()
                    // No vertical padding: the text field has its own. With one line, the
                    // field is as high as the "+" button, and the send button has the same
                    // space on each side.
                    .pl_1()
                    .pr(px(SEND_INSET))
                    .rounded(px(COMPOSER_LINE / 2. + 1.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Textarea::new(&self.composer).appearance(false)),
                    )
                    .child(div().flex_shrink_0().mb(px(SEND_INSET)).child(action)),
            )
            .into_any_element()
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A hot patch can change the type that `render_view` returns, so return `AnyElement`.
        crate::hotpatch::call(|| self.render_view(window, cx).into_any_element())
    }
}

impl AppView {
    fn render_view(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let chat = &self.chat;
        let kind = chat.kind;
        let busy = chat.busy;
        // Before the first message, the pane shows the greeting. Notices and errors from the start
        // of the agent show below the greeting.
        let empty = chat.items.iter().all(|i| matches!(i, Item::Notice(_) | Item::Error(_)));
        let cwd = chat.cwd.clone();
        let permission = chat.permission.as_ref().map(|p| self.render_permission(p, cx));
        let composer = v_flex()
            .w_full()
            .max_w(px(COLUMN))
            .mx_auto()
            .px_6()
            .gap_2()
            .children(permission)
            .child(self.render_composer(busy, cx));

        // The assistant in the middle, as the contact in Messages. The strip also moves the window.
        // Without the sidebar, the traffic lights and the button that shows the sidebar are on it.
        // The two sides have the same width, so the middle stays in the middle.
        let toolbar = self
            .toolbar("main-toolbar", cx)
            .h(px(HEADER))
            .items_start()
            .px_4()
            .child(
                h_flex()
                    .flex_1()
                    // The traffic lights are in the middle of the first TOOLBAR of height.
                    .h(px(TOOLBAR))
                    .when(!self.sidebar_open, |el| {
                        el.pl(px(68.)).child(sidebar_toggle("show-sidebar", "Show sidebar", cx))
                    }),
            )
            .child(
                v_flex()
                    .pt_2()
                    .items_center()
                    .gap_1()
                    .child(live_avatar("avatar-header", 44.))
                    .child(div().text_sm().font_medium().child(ASSISTANT_NAME)),
            )
            .child(div().flex_1());

        // An empty conversation shows only the composer, in the middle of the pane.
        let body = if empty {
            v_flex()
                .flex_1()
                .justify_center()
                // Sit a little above the center.
                .pb(px(TOOLBAR * 2.))
                .gap_6()
                .child(self.render_greeting(&cwd, &theme, cx))
                .when(!chat.items.is_empty(), |el| {
                    el.child(
                        v_flex()
                            .w_full()
                            .max_w(px(COLUMN))
                            .mx_auto()
                            .px_6()
                            .gap_2()
                            .children((0..self.rows.len()).map(|row| self.render_row(row, cx))),
                    )
                })
                .child(composer)
                .child(self.render_suggestions(kind, &theme, cx))
                .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .min_h_0()
                .child(
                    // The scroller renders only the rows that show, so a long conversation scrolls
                    // smoothly.
                    MessageScroller::new(
                        "messages",
                        self.scroller.clone(),
                        cx.processor(|this, ix: usize, _, cx| {
                            v_flex().w_full().max_w(px(COLUMN)).mx_auto().px_6().child(this.render_row(ix, cx))
                        }),
                    )
                    .flex_1()
                    .min_h_0()
                    .with_list_style(StyleRefinement::default().py_6())
                    .with_row_style(StyleRefinement::default().px_0().pb_4())
                    .with_bottom_fade(theme.background),
                )
                .child(composer.pb_5().pt_1())
                .into_any_element()
        };

        h_flex()
            .size_full()
            .text_color(theme.foreground)
            .on_action(cx.listener(|this, _: &StopAgent, _, cx| this.cancel(cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .when(self.sidebar_open, |el| el.child(self.render_sidebar(cx)).child(div().w(px(INSET)).h_full()))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .bg(theme.background)
                    .child(toolbar)
                    .child(body),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat(items: Vec<Item>, busy: bool) -> Chat {
        let mut chat = Chat::new(AgentKind::Claude, items, PathBuf::new());
        chat.busy = busy;
        chat
    }

    fn turn() -> Vec<Item> {
        vec![
            Item::User { text: "hi".into(), at: 0 },
            Item::Thought("hmm".into()),
            Item::Agent("Let me look.".into()),
            Item::Error("oops".into()),
            Item::Agent("Done.".into()),
        ]
    }

    #[test]
    fn a_done_turn_shows_the_final_reply_and_hides_the_steps() {
        let rows = build_rows(&chat(turn(), false), &HashSet::new());
        assert!(rows == [Row::Item(0), Row::Item(3), Row::Reply { ix: 4, turn: 0, open: false }]);
    }

    #[test]
    fn open_steps_show_all_items() {
        let rows = build_rows(&chat(turn(), false), &HashSet::from([0]));
        let reply = Row::Reply { ix: 4, turn: 0, open: true };
        assert!(rows == [Row::Item(0), Row::Item(1), Row::Item(2), Row::Item(3), reply]);
    }

    #[test]
    fn the_time_shows_after_a_gap() {
        assert!(show_time(None, 1_000));
        assert!(show_time(Some(0), 1_700_000_000));
        assert!(!show_time(Some(1_000), 1_000 + TIME_GAP - 1));
        assert!(show_time(Some(1_000), 1_000 + TIME_GAP));
        assert!(!show_time(None, 0));
    }

    #[test]
    fn the_time_label_is_as_in_messages() {
        use chrono::TimeZone as _;
        let now = chrono::Local.with_ymd_and_hms(2026, 10, 19, 15, 0, 0).unwrap();
        let at = |y, m, d| chrono::Local.with_ymd_and_hms(y, m, d, 9, 41, 0).unwrap().timestamp();
        let label = |t| time_label(t, now).unwrap();
        assert_eq!(label(at(2026, 10, 19)), ("Today".into(), "9:41 AM".into()));
        assert_eq!(label(at(2026, 10, 18)), ("Yesterday".into(), "9:41 AM".into()));
        assert_eq!(label(at(2026, 10, 13)), ("Tuesday".into(), "9:41 AM".into()));
        assert_eq!(label(at(2026, 10, 12)), ("Mon, Oct 12".into(), "at 9:41 AM".into()));
        assert_eq!(label(at(2025, 10, 12)), ("Oct 12, 2025".into(), "at 9:41 AM".into()));
    }

    fn tool(id: &str, kind: &str, parent: Option<&str>) -> Item {
        Item::Tool {
            id: id.into(),
            title: id.into(),
            kind: kind.into(),
            status: ToolStatus::Running,
            detail: None,
            parent: parent.map(Into::into),
        }
    }

    #[test]
    fn a_subagent_card_always_shows_and_holds_its_tool_calls() {
        let items = vec![
            Item::User { text: "hi".into(), at: 0 },
            tool("task", SUBAGENT_KIND, None),
            tool("read", "read", Some("task")),
            tool("ls", "execute", None),
            Item::Agent("Done.".into()),
        ];
        let rows = build_rows(&chat(items, false), &HashSet::new());
        assert!(rows == [Row::Item(0), Row::Item(1), Row::Reply { ix: 4, turn: 0, open: false }]);
        let mut items = vec![Item::User { text: "hi".into(), at: 0 }, tool("task", SUBAGENT_KIND, None)];
        items.push(tool("read", "read", Some("task")));
        let rows = build_rows(&chat(items, true), &HashSet::from([0]));
        assert!(rows == [Row::Item(0), Row::Item(1), Row::Working { steps: None }]);
    }

    #[test]
    fn a_working_turn_hides_all_agent_text() {
        let rows = build_rows(&chat(turn(), true), &HashSet::new());
        assert!(rows == [Row::Item(0), Row::Item(3), Row::Working { steps: Some((0, false)) }]);
    }
}
