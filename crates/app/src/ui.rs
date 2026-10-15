//! The main window: a sidebar, a list of chats, and the chat view, as in a mail app.

use std::time::Duration;

use acp_host::{AgentKind, Command, Event, PermissionOptionView, Session, ToolStatus};
use gpui::{
    AppContext as _, Context, Div, Entity, Focusable as _, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Render, ScrollHandle, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    Anchor, Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, InteractiveElementExt as _, Sizable as _, StyledExt as _, Theme, h_flex,
    button::{Button, ButtonRounded, ButtonVariants as _},
    input::{Input, InputEvent, InputState, Textarea, TextareaState},
    menu::{DropdownMenu as _, PopupMenuItem},
    progress::Progress,
    spinner::Spinner,
    text::TextView,
    v_flex,
};
use gpui_kit_assets::IconName as KitIcon;
use chrono::{DateTime, Local};
use tokio::sync::{mpsc, oneshot};
use vm::VmPhase;

use crate::{Env, theme};

mod demo;

gpui::actions!(
    agent_desk,
    [
        NewChat,
        CloseChat,
        NextChat,
        PreviousChat,
        StopAgent,
        FocusSearch,
        ToggleSidebar,
        ShowAllChats,
        ShowClaudeChats,
        ShowCodexChats,
    ]
);

enum Item {
    User(String),
    Agent(String),
    Thought(String),
    Tool {
        id: String,
        title: String,
        kind: String,
        status: ToolStatus,
        detail: Option<String>,
    },
    Plan(Vec<(String, ToolStatus)>),
    Notice(String),
    Error(String),
}

struct PendingPermission {
    title: String,
    options: Vec<PermissionOptionView>,
    reply: oneshot::Sender<Option<String>>,
}

struct Chat {
    id: u64,
    kind: AgentKind,
    title: String,
    items: Vec<Item>,
    session: Option<Session>,
    busy: bool,
    closed: bool,
    permission: Option<PendingPermission>,
    /// The time of the last message or agent event, for the chat list.
    updated: DateTime<Local>,
    /// The text in the composer that the user did not send. Each chat keeps its own.
    draft: String,
}

pub struct AppView {
    env: Env,
    chats: Vec<Chat>,
    active: usize,
    next_id: u64,
    composer: Entity<TextareaState>,
    search: Entity<InputState>,
    /// The chat list shows only the chats of this agent. `None` shows all chats.
    filter: Option<AgentKind>,
    scroll: ScrollHandle,
    vm_phase: VmPhase,
    vm_busy: bool,
    /// The mouse went down on a toolbar. A move then drags the window.
    drag_pending: bool,
    /// The sidebar shows. The user can hide it to give the chat more room.
    sidebar_open: bool,
    /// The agent for new chats: the agent that the user picked last.
    agent: AgentKind,
    _subs: Vec<gpui::Subscription>,
}

impl AppView {
    pub fn new(env: Env, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Ask anything")
                .auto_grow(2, 10)
                .submit_on_enter(true)
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search chats"));
        let subs = vec![
            cx.subscribe_in(&composer, window, |this, _state, ev: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { shift: false, .. } = ev {
                    this.send(window, cx);
                }
            }),
            cx.subscribe(&search, |_, _, ev: &InputEvent, cx| {
                if let InputEvent::Change = ev {
                    cx.notify();
                }
            }),
        ];

        // Follow the VM state.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let ok = this.update(cx, |this, cx| {
                    let phase = this.env.mac.phase();
                    if phase != this.vm_phase {
                        this.vm_phase = phase;
                        cx.notify();
                    }
                });
                if ok.is_err() {
                    break;
                }
            }
        })
        .detach();

        let vm_phase = env.mac.phase();
        let agent = load_agent(&env.agent_file);
        let mut view = Self {
            env,
            chats: vec![],
            active: 0,
            next_id: 1,
            composer,
            search,
            filter: None,
            scroll: ScrollHandle::new(),
            vm_phase,
            vm_busy: false,
            drag_pending: false,
            sidebar_open: true,
            agent,
            _subs: subs,
        };
        if demo::enabled() {
            view.chats = demo::chats();
            view.next_id = view.chats.len() as u64 + 1;
        } else {
            view.new_chat(agent, window, cx);
        }
        view
    }

    fn new_chat(&mut self, kind: AgentKind, window: &mut Window, cx: &mut Context<Self>) {
        self.save_draft(cx);
        self.composer.update(cx, |s, cx| s.set_value("", window, cx));
        let id = self.next_id;
        self.next_id += 1;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let session = acp_host::start(&self.env.runtime, kind, self.env.workspace.clone(), self.env.bridge.clone(), tx);
        self.chats.push(Chat {
            id,
            kind,
            title: "New chat".into(),
            items: vec![],
            session: Some(session),
            busy: false,
            closed: false,
            permission: None,
            updated: Local::now(),
            draft: String::new(),
        });
        self.active = self.chats.len() - 1;
        // Show the new chat in the list.
        if self.filter.is_some_and(|f| f != kind) {
            self.filter = None;
        }
        // The user can type at once. The session queues prompts until the agent is ready.
        window.focus(&self.composer.focus_handle(cx), cx);
        cx.spawn(async move |this, cx| {
            while let Some(event) = rx.recv().await {
                let ok = this.update(cx, |this, cx| {
                    if let Some(chat) = this.chats.iter_mut().find(|c| c.id == id) {
                        apply(chat, event);
                        chat.updated = Local::now();
                    }
                    this.scroll.scroll_to_bottom();
                    cx.notify();
                });
                if ok.is_err() {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        let Some(chat) = self.chats.get_mut(self.active) else { return };
        if chat.busy || chat.closed {
            return;
        }
        let Some(session) = &chat.session else { return };
        if chat.items.iter().all(|i| !matches!(i, Item::User(_))) {
            chat.title = text.lines().next().unwrap_or("").chars().take(40).collect();
        }
        session.send(Command::Prompt(text.clone()));
        chat.items.push(Item::User(text));
        chat.busy = true;
        chat.updated = Local::now();
        self.composer.update(cx, |s, cx| s.set_value("", window, cx));
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(chat) = self.chats.get_mut(self.active) {
            if let Some(p) = chat.permission.take() {
                let _ = p.reply.send(None);
            }
            if let Some(s) = &chat.session {
                s.send(Command::Cancel);
            }
        }
        cx.notify();
    }

    fn answer_permission(&mut self, option: Option<String>, cx: &mut Context<Self>) {
        if let Some(chat) = self.chats.get_mut(self.active) {
            if let Some(p) = chat.permission.take() {
                let _ = p.reply.send(option);
            }
        }
        cx.notify();
    }

    /// Use a different agent. A chat with no prompts changes its agent. A chat with prompts keeps
    /// its agent, because the new agent does not know the conversation, so a new chat starts.
    fn pick_agent(&mut self, kind: AgentKind, window: &mut Window, cx: &mut Context<Self>) {
        if kind != self.agent {
            self.agent = kind;
            save_agent(&self.env.agent_file, kind);
        }
        let Some(chat) = self.chats.get(self.active) else {
            self.new_chat(kind, window, cx);
            return;
        };
        if chat.kind == kind {
            return;
        }
        // The text that the user typed is for the new agent. It goes to the new chat.
        let draft = self.composer.read(cx).value().to_string();
        if !chat.items.iter().any(|i| matches!(i, Item::User(_))) {
            // Dropping the session stops the old agent. Keep the chat in the same place in the list.
            let ix = self.active;
            self.chats.remove(ix);
            // No chat is active now, so `new_chat` keeps no draft.
            self.active = usize::MAX;
            self.new_chat(kind, window, cx);
            let chat = self.chats.pop().expect("new_chat adds a chat");
            self.chats.insert(ix, chat);
            self.active = ix;
        } else {
            if let Some(chat) = self.chats.get_mut(self.active) {
                chat.draft.clear();
            }
            self.active = usize::MAX;
            self.new_chat(kind, window, cx);
        }
        self.composer.update(cx, |s, cx| s.set_value(draft, window, cx));
        cx.notify();
    }

    fn close_chat(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix < self.chats.len() {
            // Dropping the session stops the agent process.
            self.chats.remove(ix);
            if self.chats.is_empty() {
                self.new_chat(self.agent, window, cx);
            }
            self.active = self.active.min(self.chats.len() - 1);
            self.load_draft(window, cx);
        }
        cx.notify();
    }

    /// Run a slow VM action off the UI thread.
    fn vm_action(&mut self, cx: &mut Context<Self>, f: impl FnOnce(vm::AgentMac) -> anyhow::Result<()> + Send + 'static) {
        if self.vm_busy {
            return;
        }
        self.vm_busy = true;
        let mac = self.env.mac.clone();
        let task = cx.background_spawn(async move { f(mac) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.vm_busy = false;
                if let Err(e) = result {
                    if let Some(chat) = this.chats.get_mut(this.active) {
                        chat.items.push(Item::Error(format!("Agent Mac: {e:#}")));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// The indexes of the chats that the list shows: the agent filter and the search apply.
    /// The chat with the newest activity comes first, as in a mail app.
    fn visible(&self, cx: &Context<Self>) -> Vec<usize> {
        let query = self.search.read(cx).value().to_lowercase();
        let query = query.trim();
        let mut visible: Vec<usize> = self
            .chats
            .iter()
            .enumerate()
            .filter(|(_, c)| self.filter.is_none_or(|f| f == c.kind))
            .filter(|(_, c)| {
                query.is_empty() || c.title.to_lowercase().contains(query) || preview(c).to_lowercase().contains(query)
            })
            .map(|(ix, _)| ix)
            .collect();
        visible.sort_by(|&a, &b| self.chats[b].updated.cmp(&self.chats[a].updated));
        visible
    }

    fn select(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix != self.active {
            self.save_draft(cx);
            self.active = ix;
            self.load_draft(window, cx);
            self.scroll.scroll_to_bottom();
        }
        window.focus(&self.composer.focus_handle(cx), cx);
        cx.notify();
    }

    /// Keep the composer text in the active chat.
    fn save_draft(&mut self, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        if let Some(chat) = self.chats.get_mut(self.active) {
            chat.draft = text;
        }
    }

    /// Put the draft of the active chat in the composer.
    fn load_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.chats.get(self.active).map(|c| c.draft.clone()).unwrap_or_default();
        self.composer.update(cx, |s, cx| s.set_value(draft, window, cx));
    }

    /// Go to the next chat in the list (`step` 1) or the previous chat (`step` -1).
    fn step(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let visible = self.visible(cx);
        let Some(pos) = visible.iter().position(|&ix| ix == self.active) else {
            if let Some(&first) = visible.first() {
                self.select(first, window, cx);
            }
            return;
        };
        if let Some(&ix) = pos.checked_add_signed(step).and_then(|p| visible.get(p)) {
            self.select(ix, window, cx);
        }
    }

    fn set_filter(&mut self, filter: Option<AgentKind>, cx: &mut Context<Self>) {
        self.filter = filter;
        cx.notify();
    }

    /// Put a suggested prompt in the composer, for the user to change or send.
    fn suggest(&mut self, text: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |s, cx| s.set_value(text, window, cx));
        window.focus(&self.composer.focus_handle(cx), cx);
    }
}

fn apply(chat: &mut Chat, event: Event) {
    match event {
        Event::Ready { .. } => {}
        Event::AgentText(t) => match chat.items.last_mut() {
            Some(Item::Agent(s)) => s.push_str(&t),
            _ => chat.items.push(Item::Agent(t)),
        },
        Event::Thought(t) => match chat.items.last_mut() {
            Some(Item::Thought(s)) => s.push_str(&t),
            _ => chat.items.push(Item::Thought(t)),
        },
        Event::ToolCall { id, title, kind, status } => chat.items.push(Item::Tool {
            id,
            title,
            kind,
            status,
            detail: None,
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
            if reason != "EndTurn" {
                chat.items.push(Item::Notice(format!("Turn ended: {reason}")));
            }
        }
        Event::Error(e) => chat.items.push(Item::Error(e)),
        Event::Closed => {
            chat.closed = true;
            chat.busy = false;
            chat.session = None;
            chat.items.push(Item::Notice("The agent process stopped.".into()));
        }
    }
}

/// The width of the content column, as in a mail message.
const COLUMN: f32 = 760.;
/// The height of the toolbars. The traffic lights sit in the sidebar toolbar.
const TOOLBAR: f32 = 52.;
/// The width of the sidebar panel.
const SIDEBAR: f32 = 232.;
/// The width of the chat list, as the message list in a mail app.
const LIST: f32 = 340.;
/// The space between the sidebar panel and the window edges.
const INSET: f32 = 8.;

fn agent_key(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "claude",
        AgentKind::Codex => "codex",
    }
}

fn load_agent(path: &std::path::Path) -> AgentKind {
    let key = std::fs::read_to_string(path).unwrap_or_default();
    AgentKind::ALL
        .into_iter()
        .find(|&k| agent_key(k) == key.trim())
        .unwrap_or(AgentKind::Claude)
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

fn agent_icon(kind: AgentKind) -> KitIcon {
    match kind {
        AgentKind::Claude => KitIcon::Asterisk,
        AgentKind::Codex => KitIcon::CodeXml,
    }
}

fn agent_name(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "Claude",
        AgentKind::Codex => "Codex",
    }
}

/// A round badge with the agent mark, as the sender photo in Mail.
fn avatar(kind: AgentKind, size: f32) -> Div {
    let color = agent_color(kind);
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded_full()
        .bg(color.opacity(0.16))
        .child(Icon::new(agent_icon(kind)).size(px(size * 0.55)).text_color(color))
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
        _ => KitIcon::Wrench,
    }
}

/// The color of the dot that shows the Agent Mac state, as in a container list.
fn phase_color(phase: &VmPhase, theme: &Theme) -> Hsla {
    match phase {
        VmPhase::Running => theme.success,
        VmPhase::Starting | VmPhase::Stopping | VmPhase::Downloading { .. } | VmPhase::Installing { .. } => {
            theme.warning
        }
        VmPhase::Error(_) => theme.danger,
        VmPhase::NotInstalled | VmPhase::Stopped => theme.muted_foreground,
    }
}

/// A small gray title above a group of sidebar rows.
fn section_header(label: &'static str, theme: &Theme) -> Div {
    div()
        .px_4()
        .pt_3()
        .pb_1()
        .text_xs()
        .font_medium()
        .text_color(theme.muted_foreground)
        .child(label)
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

/// A sidebar row with an icon, a label, and an optional count. The selected row has a tint.
fn nav_row(
    id: impl Into<gpui::ElementId>,
    icon: KitIcon,
    label: impl Into<SharedString>,
    count: Option<usize>,
    selected: bool,
    theme: &Theme,
) -> Stateful<Div> {
    h_flex()
        .id(id)
        .mx_2()
        .px_2()
        .h(px(32.))
        .gap_2p5()
        .rounded(px(8.))
        .cursor_pointer()
        .text_sm()
        .when(selected, |el| el.bg(theme.foreground.opacity(0.07)).font_medium())
        .when(!selected, |el| el.hover(|el| el.bg(theme.foreground.opacity(0.04))))
        .child(
            Icon::new(icon)
                .size(px(16.))
                .text_color(if selected { accent(theme) } else { theme.foreground.opacity(0.75) }),
        )
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
        .children(
            count
                .filter(|&n| n > 0)
                .map(|n| div().text_xs().text_color(theme.muted_foreground).child(n.to_string())),
        )
}

/// The time of a chat for the list, as in a mail app: the time today, the weekday in the last
/// week, and the date before that.
fn short_time(t: DateTime<Local>) -> String {
    let now = Local::now();
    let days = (now.date_naive() - t.date_naive()).num_days();
    if days == 0 {
        t.format("%-I:%M %p").to_string()
    } else if days == 1 {
        "Yesterday".into()
    } else if days < 7 {
        t.format("%A").to_string()
    } else {
        t.format("%b %-d").to_string()
    }
}

/// A short line for the chat list: the last text of the agent, or what it does now.
fn preview(chat: &Chat) -> String {
    if chat.permission.is_some() {
        return "Waiting for your permission".into();
    }
    if chat.closed {
        return "The agent stopped".into();
    }
    let last = chat.items.iter().rev().find_map(|i| match i {
        Item::Agent(t) | Item::User(t) => Some(t.as_str()),
        _ => None,
    });
    match last {
        Some(t) => t.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(80).collect(),
        None if chat.busy => "Working…".into(),
        None => String::new(),
    }
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
        let count = |kind: Option<AgentKind>| {
            self.chats.iter().filter(|c| kind.is_none_or(|k| k == c.kind)).count()
        };

        // The app mark, as the logo of a mail app.
        let brand = h_flex()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(22.))
                    .rounded(px(6.))
                    .bg(accent.opacity(0.14))
                    .child(Icon::new(KitIcon::MessagesSquare).size(px(13.)).text_color(accent)),
            )
            .child(
                h_flex()
                    .text_base()
                    .font_semibold()
                    .child("agent desk")
                    .child(div().text_color(accent).child(".")),
            );

        let new_chat = h_flex()
            .id("new-chat")
            .mx_2()
            .px_2()
            .h(px(32.))
            .gap_2p5()
            .rounded(px(8.))
            .cursor_pointer()
            .text_sm()
            .font_medium()
            .text_color(accent)
            .hover(|el| el.bg(accent.opacity(0.08)))
            .on_click(cx.listener(|this, _, window, cx| this.new_chat(this.agent, window, cx)))
            .child(Icon::new(KitIcon::SquarePen).size(px(16.)))
            .child(div().flex_1().child("New chat"))
            .child(div().text_xs().text_color(theme.muted_foreground).child("⌘N"));

        let mut filters = vec![nav_row("filter-all", KitIcon::Inbox, "All chats", Some(count(None)), self.filter.is_none(), &theme)
            .on_click(cx.listener(|this, _, _, cx| this.set_filter(None, cx)))];
        for kind in AgentKind::ALL {
            filters.push(
                nav_row(
                    SharedString::from(format!("filter-{}", agent_key(kind))),
                    agent_icon(kind),
                    agent_name(kind),
                    Some(count(Some(kind))),
                    self.filter == Some(kind),
                    &theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_filter(Some(kind), cx))),
            );
        }

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
            .child(div().pt_2().child(new_chat))
            .child(section_header("Chats", &theme))
            .child(v_flex().gap_px().children(filters))
            .child(section_header("Agent Mac", &theme))
            .child(self.render_vm_panel(cx))
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
        let menu = Button::new("theme-menu")
            .ghost()
            .small()
            .icon(Icon::new(KitIcon::Palette))
            .label(label)
            .dropdown_menu_with_anchor(Anchor::BottomLeft, |menu, window, cx| {
                let choice = cx.global::<theme::ThemeChoice>().clone();
                let base = choice.base;
                let accent = choice.accent;
                let appearance = choice.appearance;
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

    /// The middle column: the chats that match the filter and the search, as a message list.
    fn render_list(&self, visible: &[usize], cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let title = match self.filter {
            None => "All chats",
            Some(kind) => agent_name(kind),
        };
        let rows: Vec<_> = visible
            .iter()
            .map(|&ix| self.render_chat_row(ix, &self.chats[ix], &theme, cx))
            .collect();
        let empty = rows.is_empty();
        v_flex()
            .w(px(LIST))
            .h_full()
            .flex_shrink_0()
            .bg(theme.background)
            .border_r_1()
            .border_color(theme.border)
            .child(
                self.toolbar("list-toolbar", cx)
                    .px_4()
                    .gap_2()
                    // Without the sidebar, the traffic lights are on this toolbar.
                    .when(!self.sidebar_open, |el| {
                        el.pl(px(84.)).child(sidebar_toggle("show-sidebar", "Show sidebar", cx))
                    })
                    .child(div().flex_1().text_lg().font_semibold().child(title))
                    .child(
                        Button::new("list-new-chat")
                            .ghost()
                            .small()
                            .icon(Icon::new(KitIcon::SquarePen))
                            .tooltip_with_action("New chat", &NewChat, None)
                            .on_click(cx.listener(|this, _, window, cx| this.new_chat(this.agent, window, cx))),
                    ),
            )
            .child(
                div().px_4().pb_3().child(
                    h_flex()
                        .h(px(36.))
                        .px_2()
                        .rounded(px(8.))
                        .bg(theme.foreground.opacity(0.045))
                        .child(
                            Input::new(&self.search)
                                .appearance(false)
                                .prefix(Icon::new(KitIcon::Search).size(px(15.)).text_color(theme.muted_foreground))
                                .cleanable(true),
                        ),
                ),
            )
            .child(
                v_flex()
                    .id("chat-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .border_t_1()
                    .border_color(theme.border)
                    .children(rows)
                    .when(empty, |el| {
                        el.child(
                            div()
                                .p_6()
                                .text_center()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("No chats"),
                        )
                    }),
            )
            .into_any_element()
    }

    fn render_chat_row(&self, ix: usize, chat: &Chat, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let active = ix == self.active;
        let group: SharedString = format!("chat-row-{ix}").into();
        let trailing = if chat.permission.is_some() {
            Some(
                Icon::new(KitIcon::ShieldAlert)
                    .size(px(14.))
                    .text_color(theme.warning)
                    .into_any_element(),
            )
        } else if chat.busy {
            Some(
                Spinner::new()
                    .with_size(px(12.))
                    .color(accent(theme))
                    .into_any_element(),
            )
        } else {
            None
        };
        // A dot before the name, as an unread mark: the chat needs the user.
        let attention = chat.permission.is_some();
        let preview = match preview(chat) {
            p if p.is_empty() => "No messages yet".to_string(),
            p => p,
        };
        h_flex()
            .id(("chat", ix))
            .group(group.clone())
            .px_4()
            .py_3()
            .gap_3()
            .items_start()
            .border_b_1()
            .border_color(theme.border)
            .cursor_pointer()
            .when(active, |el| el.bg(accent(theme).opacity(if theme.is_dark() { 0.14 } else { 0.07 })))
            .when(!active, |el| el.hover(|el| el.bg(theme.foreground.opacity(0.025))))
            .on_click(cx.listener(move |this, _, window, cx| this.select(ix, window, cx)))
            .child(avatar(chat.kind, 36.))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .h(px(20.))
                            .when(attention, |el| el.child(div().size(px(7.)).rounded_full().bg(accent(theme))))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .font_semibold()
                                    .child(agent_name(chat.kind)),
                            )
                            // The time and the status show until the pointer is on the row. Then the close
                            // button shows over them. Hover styles change only the visibility, not the
                            // layout: GPUI lays out without the hover style, and paints with it.
                            .child(
                                div()
                                    .relative()
                                    .flex_shrink_0()
                                    .child(
                                        h_flex()
                                            .h(px(20.))
                                            .gap_1p5()
                                            .group_hover(group.clone(), |s| s.invisible())
                                            .children(trailing)
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(short_time(chat.updated)),
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .absolute()
                                            .top_0()
                                            .right_0()
                                            .h(px(20.))
                                            .invisible()
                                            .group_hover(group, |s| s.visible())
                                            .child(
                                                Button::new(("close", ix))
                                                    .ghost()
                                                    .xsmall()
                                                    .icon(Icon::new(KitIcon::X).text_color(theme.muted_foreground))
                                                    .tooltip("Close chat")
                                                    .on_click(cx.listener(move |this, _, window, cx| {
                                                        cx.stop_propagation();
                                                        this.close_chat(ix, window, cx)
                                                    })),
                                            ),
                                    ),
                            ),
                    )
                    .child(div().truncate().text_sm().child(chat.title.clone()))
                    .child(div().truncate().text_xs().text_color(theme.muted_foreground).child(preview)),
            )
            .into_any_element()
    }

    fn render_vm_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let phase = &self.vm_phase;
        let busy = self.vm_busy;
        let supported = vm::AgentMac::is_supported();
        let installed = self.env.mac.paths().bundle.join(".installed").exists();
        let mut actions = h_flex().gap_0p5();
        match phase {
            VmPhase::NotInstalled | VmPhase::Error(_) if !installed => {
                actions = actions.child(
                    Button::new("vm-install")
                        .small()
                        .primary()
                        .w_full()
                        .icon(Icon::new(KitIcon::Download))
                        .label("Install macOS")
                        .disabled(busy || !supported)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.env.mac.install();
                            cx.notify();
                        })),
                );
            }
            VmPhase::Stopped | VmPhase::Error(_) => {
                actions = actions.child(
                    Button::new("vm-start")
                        .xsmall()
                        .ghost()
                        .icon(Icon::new(KitIcon::Play).text_color(theme.success))
                        .tooltip("Start")
                        .loading(busy)
                        .on_click(cx.listener(|this, _, _, cx| this.vm_action(cx, |mac| mac.start()))),
                );
            }
            VmPhase::Running => {
                actions = actions
                    .child(
                        Button::new("vm-show")
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(KitIcon::Monitor))
                            .tooltip("Show screen")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Err(e) = vm::show_window() {
                                    if let Some(chat) = this.chats.get_mut(this.active) {
                                        chat.items.push(Item::Error(format!("Agent Mac: {e:#}")));
                                    }
                                    cx.notify();
                                }
                            })),
                    )
                    .child(
                        Button::new("vm-stop")
                            .xsmall()
                            .ghost()
                            .icon(Icon::new(KitIcon::Square).text_color(theme.danger))
                            .tooltip("Stop")
                            .loading(busy)
                            .on_click(cx.listener(|this, _, _, cx| this.vm_action(cx, |mac| mac.stop()))),
                    );
            }
            VmPhase::Starting | VmPhase::Stopping => {
                actions = actions.child(Spinner::new().xsmall().color(theme.muted_foreground));
            }
            _ => {}
        }
        let fraction = match phase {
            VmPhase::Downloading { fraction } | VmPhase::Installing { fraction } => Some(*fraction),
            _ => None,
        };
        let note = if !supported {
            Some(("This Mac cannot run macOS VMs (for example, inside another VM).", theme.warning))
        } else if matches!(phase, VmPhase::NotInstalled) {
            Some(("Downloads about 15 GB and uses up to 96 GB of disk.", theme.muted_foreground))
        } else {
            None
        };
        let dot = phase_color(phase, &theme);
        // The install button is wide, so it goes below the name. The other buttons are icons.
        let primary_action = matches!(phase, VmPhase::NotInstalled | VmPhase::Error(_)) && !installed;
        let (inline, below) = if primary_action { (None, Some(actions)) } else { (Some(actions), None) };

        // A machine row with a status dot, as in a container list.
        v_flex()
            .child(
                v_flex()
                    .mx_2()
                    .px_2()
                    .py_1p5()
                    .gap_2()
                    .rounded(px(8.))
                    .child(
                        h_flex()
                            .gap_2p5()
                            .child(
                                div()
                                    .relative()
                                    .flex()
                                    .flex_shrink_0()
                                    .items_center()
                                    .justify_center()
                                    .size(px(26.))
                                    .rounded(px(7.))
                                    .bg(theme.foreground.opacity(0.07))
                                    .child(Icon::new(KitIcon::Laptop).size(px(15.)))
                                    .child(
                                        div()
                                            .absolute()
                                            .right(px(-2.))
                                            .bottom(px(-2.))
                                            .size(px(7.))
                                            .rounded_full()
                                            .bg(dot),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().text_sm().font_medium().child("macOS VM"))
                                    .child(
                                        div()
                                            .truncate()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(phase.label()),
                                    ),
                            )
                            .children(inline),
                    )
                    .when_some(fraction, |el, f| {
                        el.child(Progress::new("vm-progress").value((f * 100.) as f32))
                    })
                    .when_some(note, |el, (text, color)| el.child(div().text_xs().text_color(color).child(text)))
                    .children(below),
            )
            .into_any_element()
    }

    fn render_item(&self, ix: usize, item: &Item, kind: AgentKind, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        match item {
            Item::User(text) => h_flex()
                .justify_end()
                .pl(px(80.))
                .child(
                    div()
                        .px_4()
                        .py_2p5()
                        .rounded(px(18.))
                        .bg(accent(&theme))
                        .text_color(theme.primary_foreground)
                        .child(TextView::markdown(("item", ix), SharedString::from(text.clone())).selectable(true)),
                )
                .into_any_element(),
            Item::Agent(text) => h_flex()
                .items_start()
                .gap_3()
                .child(avatar(kind, 28.))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .pt_0p5()
                        .child(TextView::markdown(("item", ix), SharedString::from(text.clone())).selectable(true)),
                )
                .into_any_element(),
            Item::Thought(text) => h_flex()
                .items_start()
                .gap_2()
                .pl(px(40.))
                .text_color(theme.muted_foreground)
                .child(Icon::new(KitIcon::Brain).size(px(14.)).mt_0p5())
                .child(div().flex_1().min_w_0().text_sm().italic().child(text.clone()))
                .into_any_element(),
            Item::Tool {
                title,
                kind: tool_kind,
                status,
                detail,
                ..
            } => v_flex()
                .ml(px(40.))
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.foreground.opacity(0.02))
                .overflow_hidden()
                .child(
                    h_flex()
                        .px_3()
                        .py_2()
                        .gap_2()
                        .child(Icon::new(tool_icon(tool_kind)).size(px(14.)).text_color(theme.muted_foreground))
                        .child(div().flex_1().min_w_0().truncate().text_sm().child(title.clone()))
                        .child(status_icon(*status, &theme)),
                )
                .when_some(detail.clone(), |el, d| {
                    let short: String = d.chars().take(1200).collect();
                    el.child(
                        div()
                            .px_3()
                            .py_2()
                            .max_h(px(180.))
                            .overflow_hidden()
                            .border_t_1()
                            .border_color(theme.border)
                            .bg(theme.foreground.opacity(0.03))
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .text_color(theme.muted_foreground)
                            .child(short),
                    )
                })
                .into_any_element(),
            Item::Plan(entries) => {
                let done = entries.iter().filter(|(_, s)| *s == ToolStatus::Done).count();
                v_flex()
                    .ml(px(40.))
                    .p_3()
                    .gap_1p5()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
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
                        .px_3()
                        .py_1()
                        .rounded_full()
                        .bg(theme.foreground.opacity(0.05))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(text.clone()),
                )
                .into_any_element(),
            Item::Error(text) => h_flex()
                .ml(px(40.))
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
                .child(Icon::new(KitIcon::CircleAlert).size(px(14.)).mt_0p5())
                .child(div().flex_1().min_w_0().child(text.clone()))
                .into_any_element(),
        }
    }

    fn render_permission(&self, p: &PendingPermission, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
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
                        let button = if o.allow { button.primary() } else { button.outline() };
                        button.on_click(cx.listener(move |this, _, _, cx| this.answer_permission(Some(id.clone()), cx)))
                    })),
            )
        .into_any_element()
    }

    /// The top of a new chat: the agent, a question, and the folder that the agent works in.
    fn render_greeting(&self, kind: AgentKind, theme: &Theme) -> gpui::AnyElement {
        let home = std::env::var("HOME").unwrap_or_default();
        let folder = self.env.workspace.display().to_string();
        let folder = match folder.strip_prefix(&home) {
            Some(rest) if !home.is_empty() => format!("~{rest}"),
            _ => folder,
        };
        v_flex()
            .items_center()
            .gap_3()
            .px_6()
            .child(avatar(kind, 44.))
            .child(
                div()
                    .text_2xl()
                    .font_semibold()
                    .child(format!("What should {} work on?", agent_name(kind))),
            )
            .child(
                h_flex()
                    .gap_1p5()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(Icon::new(KitIcon::Folder).size(px(14.)))
                    .child(folder),
            )
            .into_any_element()
    }

    /// Prompts to start from. A click puts the prompt in the composer.
    fn render_suggestions(&self, kind: AgentKind, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        const SUGGESTIONS: [(KitIcon, &str, &str); 4] = [
            (KitIcon::BookOpen, "Explain this project", "Explain how this project works. Start with the main parts."),
            (KitIcon::Bug, "Find a bug", "Find a bug in this project and fix it. Tell me what you changed."),
            (KitIcon::FlaskConical, "Write tests", "Write tests for the part of this project with the fewest tests."),
            (KitIcon::Monitor, "Test on the Agent Mac", "Use the Agent Mac to start the app and check that it works."),
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
    fn render_agent_picker(&self, kind: AgentKind, busy: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = cx.entity().downgrade();
        Button::new("agent-picker")
            .ghost()
            .xsmall()
            .icon(Icon::new(agent_icon(kind)).size(px(13.)).text_color(agent_color(kind)))
            .label(agent_name(kind))
            .tooltip("Change the agent")
            .disabled(busy)
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |menu, _, _| {
                AgentKind::ALL.iter().fold(menu, |menu, &k| {
                    let view = view.clone();
                    menu.item(
                        PopupMenuItem::new(k.label())
                            .icon(Icon::new(agent_icon(k)).text_color(agent_color(k)))
                            .checked(k == kind)
                            .on_click(move |_, window, cx| {
                                let _ = view.update(cx, |this, cx| this.pick_agent(k, window, cx));
                            }),
                    )
                })
            })
            .into_any_element()
    }

    fn render_composer(&self, kind: AgentKind, busy: bool, closed: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let action = if busy {
            Button::new("stop")
                .small()
                .rounded(ButtonRounded::Size(px(14.)))
                .icon(Icon::new(KitIcon::Square))
                .tooltip("Stop")
                .on_click(cx.listener(|this, _, _, cx| this.cancel(cx)))
        } else {
            Button::new("send")
                .small()
                .primary()
                .rounded(ButtonRounded::Size(px(14.)))
                .icon(Icon::new(KitIcon::ArrowUp))
                .tooltip("Send")
                .disabled(closed)
                .on_click(cx.listener(|this, _, window, cx| this.send(window, cx)))
        };
        v_flex()
            .rounded(px(16.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .shadow_md()
            .child(
                div()
                    .px_1()
                    .pt_1()
                    .child(Textarea::new(&self.composer).appearance(false)),
            )
            .child(
                h_flex()
                    .px_3()
                    .pb_2()
                    .gap_2()
                    .child(self.render_agent_picker(kind, busy, cx))
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .when(busy, |el| el.child("is working…")),
                    )
                    .child(action),
            )
        .into_any_element()
    }
}

impl Render for AppView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let visible = self.visible(cx);
        let chat = self.chats.get(self.active);
        let kind = chat.map(|c| c.kind).unwrap_or(AgentKind::Claude);
        let empty = chat.is_none_or(|c| c.items.is_empty());
        let items: Vec<gpui::AnyElement> = chat
            .map(|c| {
                c.items
                    .iter()
                    .enumerate()
                    .map(|(ix, item)| self.render_item(ix, item, kind, cx))
                    .collect()
            })
            .unwrap_or_default();
        let permission = chat
            .and_then(|c| c.permission.as_ref())
            .map(|p| self.render_permission(p, cx).into_any_element());
        let (busy, closed) = chat.map(|c| (c.busy, c.closed)).unwrap_or_default();
        let title = chat.map(|c| c.title.clone()).unwrap_or_default();
        let composer = v_flex()
            .w_full()
            .max_w(px(COLUMN))
            .mx_auto()
            .px_6()
            .gap_2()
            .children(permission)
            .child(self.render_composer(kind, busy, closed, cx));

        // The place of the chat in the list, as "1 of 19" in a mail app.
        let position = visible.iter().position(|&ix| ix == self.active);
        let prev = position.and_then(|p| p.checked_sub(1)).map(|p| visible[p]);
        let next = position.and_then(|p| visible.get(p + 1).copied());
        let active = self.active;
        let toolbar = self
            .toolbar("main-toolbar", cx)
            .px_4()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("close-chat")
                    .ghost()
                    .small()
                    .icon(Icon::new(KitIcon::Trash))
                    .tooltip_with_action("Close chat", &CloseChat, None)
                    .on_click(cx.listener(move |this, _, window, cx| this.close_chat(active, window, cx))),
            )
            .when(busy, |el| {
                el.child(
                    Button::new("stop-turn")
                        .ghost()
                        .small()
                        .icon(Icon::new(KitIcon::Square))
                        .tooltip_with_action("Stop", &StopAgent, None)
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                )
            })
            .child(div().flex_1())
            .when_some(position, |el, p| {
                el.child(
                    div()
                        .pr_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("{} of {}", p + 1, visible.len())),
                )
            })
            .child(
                Button::new("prev-chat")
                    .ghost()
                    .small()
                    .icon(Icon::new(KitIcon::ChevronUp))
                    .tooltip_with_action("Previous chat", &PreviousChat, None)
                    .disabled(prev.is_none())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(ix) = prev {
                            this.select(ix, window, cx)
                        }
                    })),
            )
            .child(
                Button::new("next-chat")
                    .ghost()
                    .small()
                    .icon(Icon::new(KitIcon::ChevronDown))
                    .tooltip_with_action("Next chat", &NextChat, None)
                    .disabled(next.is_none())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(ix) = next {
                            this.select(ix, window, cx)
                        }
                    })),
            );

        // The subject line and a chip with the agent, as a message header.
        let header = v_flex()
            .w_full()
            .max_w(px(COLUMN))
            .mx_auto()
            .px_6()
            .pt_6()
            .gap_2()
            .child(div().text_xl().font_semibold().child(title))
            .child(
                h_flex().child(
                    h_flex()
                        .px_2p5()
                        .py_0p5()
                        .gap_1p5()
                        .rounded_full()
                        .bg(theme.foreground.opacity(0.045))
                        .text_xs()
                        .child(div().size(px(6.)).rounded_full().bg(agent_color(kind)))
                        .child(kind.label())
                        .child(div().text_color(theme.muted_foreground).child("· ACP")),
                ),
            );

        // A new chat shows only the composer, in the middle of the pane, as ChatGPT does.
        let body = if empty {
            v_flex()
                .flex_1()
                .justify_center()
                // Sit a little above the center.
                .pb(px(TOOLBAR * 2.))
                .gap_6()
                .child(self.render_greeting(kind, &theme))
                .child(composer)
                .child(self.render_suggestions(kind, &theme, cx))
                .into_any_element()
        } else {
            v_flex()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .id("messages")
                        .flex_1()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .child(header)
                        .child(
                            v_flex()
                                .w_full()
                                .max_w(px(COLUMN))
                                .mx_auto()
                                .px_6()
                                .py_6()
                                .gap_4()
                                .children(items),
                        ),
                )
                .child(composer.pb_5().pt_1())
                .into_any_element()
        };

        h_flex()
            .size_full()
            .text_color(theme.foreground)
            .on_action(cx.listener(|this, _: &NewChat, window, cx| this.new_chat(this.agent, window, cx)))
            .on_action(cx.listener(|this, _: &CloseChat, window, cx| this.close_chat(this.active, window, cx)))
            .on_action(cx.listener(|this, _: &NextChat, window, cx| this.step(1, window, cx)))
            .on_action(cx.listener(|this, _: &PreviousChat, window, cx| this.step(-1, window, cx)))
            .on_action(cx.listener(|this, _: &StopAgent, _, cx| this.cancel(cx)))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                window.focus(&this.search.focus_handle(cx), cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ShowAllChats, _, cx| this.set_filter(None, cx)))
            .on_action(cx.listener(|this, _: &ShowClaudeChats, _, cx| this.set_filter(Some(AgentKind::Claude), cx)))
            .on_action(cx.listener(|this, _: &ShowCodexChats, _, cx| this.set_filter(Some(AgentKind::Codex), cx)))
            .when(self.sidebar_open, |el| el.child(self.render_sidebar(cx)).child(div().w(px(INSET)).h_full()))
            .child(self.render_list(&visible, cx))
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
