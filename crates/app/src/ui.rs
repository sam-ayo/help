//! The main window: one conversation with the assistant, as in a messages app, and a sidebar.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

use acp_host::{AgentKind, Command, Event, PermissionOptionView, Session, ToolStatus};
use gpui::{
    AppContext as _, Context, Div, Entity, Focusable as _, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Render, ScrollHandle, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    Anchor, Window, div, prelude::FluentBuilder as _, px, rgb,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, InteractiveElementExt as _, Sizable as _, StyledExt as _, Theme, h_flex,
    button::{Button, ButtonRounded, ButtonVariants as _},
    input::{InputEvent, Textarea, TextareaState},
    menu::{DropdownMenu as _, PopupMenuItem},
    progress::Progress,
    spinner::Spinner,
    text::TextView,
    v_flex,
};
use gpui_kit_assets::IconName as KitIcon;
use tokio::sync::{mpsc, oneshot};
use vm::VmPhase;

use crate::{Env, theme};

mod demo;
mod history;

gpui::actions!(agent_desk, [NewTopic, StopAgent, ToggleSidebar]);

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
    /// A new topic starts here. A new session gets no messages from before this item. The text
    /// is the title of the topic, or empty.
    Topic(String),
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
    /// Each new session gets a new number. The events of an older session do not apply.
    generation: u64,
    /// The session got the earlier messages of the topic, or the topic had none.
    primed: bool,
    busy: bool,
    closed: bool,
    permission: Option<PendingPermission>,
    /// The folder that the agent works in.
    cwd: PathBuf,
}

impl Chat {
    /// A conversation with no agent session.
    fn new(kind: AgentKind, items: Vec<Item>, cwd: PathBuf) -> Self {
        Self {
            kind,
            items,
            session: None,
            generation: 0,
            primed: false,
            busy: false,
            closed: false,
            permission: None,
            cwd,
        }
    }

    /// The items of the current topic: the items after the last topic divider.
    fn topic_items(&self) -> &[Item] {
        let start = self.items.iter().rposition(|i| matches!(i, Item::Topic(_))).map_or(0, |ix| ix + 1);
        &self.items[start..]
    }
}

pub struct AppView {
    env: Env,
    chat: Chat,
    composer: Entity<TextareaState>,
    scroll: ScrollHandle,
    vm_phase: VmPhase,
    vm_busy: bool,
    /// The mouse went down on a toolbar. A move then drags the window.
    drag_pending: bool,
    /// The sidebar shows. The user can hide it to give the conversation more room.
    sidebar_open: bool,
    /// The tool cards that the user opened or closed, by tool id. A card for a failed tool
    /// starts open. The other cards start closed.
    toggled_tools: HashSet<String>,
    _subs: Vec<gpui::Subscription>,
}

impl AppView {
    pub fn new(env: Env, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Message your assistant")
                .auto_grow(2, 10)
                .submit_on_enter(true)
        });
        let subs = vec![cx.subscribe_in(&composer, window, |this, _state, ev: &InputEvent, window, cx| {
            if let InputEvent::PressEnter { shift: false, .. } = ev {
                this.send(window, cx);
            }
        })];

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
        let mut view = Self {
            env,
            chat,
            composer,
            scroll: ScrollHandle::new(),
            vm_phase,
            vm_busy: false,
            drag_pending: false,
            sidebar_open: true,
            toggled_tools: HashSet::new(),
            _subs: subs,
        };
        // The agent starts now, so that it is ready for the first message.
        view.connect(cx);
        view.scroll.scroll_to_bottom();
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
    /// session stops its agent.
    fn connect(&mut self, cx: &mut Context<Self>) {
        // The demo starts no agents. See `demo`.
        if demo::enabled() {
            return;
        }
        let chat = &mut self.chat;
        chat.generation += 1;
        let generation = chat.generation;
        let (tx, mut rx) = mpsc::unbounded_channel();
        chat.session = Some(acp_host::start(
            &self.env.runtime,
            chat.kind,
            chat.cwd.clone(),
            self.env.bridge.clone(),
            tx,
        ));
        chat.primed = false;
        chat.closed = false;
        chat.busy = false;
        if let Some(p) = chat.permission.take() {
            let _ = p.reply.send(None);
        }
        cx.spawn(async move |this, cx| {
            while let Some(event) = rx.recv().await {
                let ok = this.update(cx, |this, cx| {
                    if this.chat.generation != generation {
                        return;
                    }
                    // Keep the conversation at the end of each turn, and when the agent stops.
                    let save = matches!(event, Event::TurnEnded(_) | Event::Closed);
                    apply(&mut this.chat, event);
                    if save {
                        this.save(cx);
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
        if demo::enabled() {
            self.chat.items.push(Item::Notice("The demo has no agent.".into()));
            cx.notify();
            return;
        }
        // An agent that stopped gets a new session.
        if self.chat.session.is_none() {
            self.connect(cx);
        }
        let chat = &mut self.chat;
        // The first prompt of a session has the earlier messages of the topic.
        let prompt = if chat.primed { text.clone() } else { history::prompt_with_context(chat, &text) };
        let Some(session) = &chat.session else { return };
        session.send(Command::Prompt(prompt));
        chat.primed = true;
        chat.items.push(Item::User(text));
        chat.busy = true;
        self.composer.update(cx, |s, cx| s.set_value("", window, cx));
        self.scroll.scroll_to_bottom();
        self.save(cx);
        cx.notify();
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.chat.permission.take() {
            let _ = p.reply.send(None);
        }
        if let Some(s) = &self.chat.session {
            s.send(Command::Cancel);
        }
        cx.notify();
    }

    fn answer_permission(&mut self, option: Option<String>, cx: &mut Context<Self>) {
        if let Some(p) = self.chat.permission.take() {
            let _ = p.reply.send(option);
        }
        cx.notify();
    }

    /// The conversation has a message in the current topic.
    fn topic_started(&self) -> bool {
        self.chat.topic_items().iter().any(|i| matches!(i, Item::User(_)))
    }

    /// Start a new topic. The agent forgets the earlier messages. The conversation keeps them.
    fn new_topic(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.chat.busy || !self.topic_started() {
            return;
        }
        self.chat.items.push(Item::Topic(String::new()));
        self.connect(cx);
        self.save(cx);
        self.scroll.scroll_to_bottom();
        window.focus(&self.composer.focus_handle(cx), cx);
        cx.notify();
    }

    /// Use a different agent. The new agent gets the earlier messages of the topic with the
    /// next message.
    fn pick_agent(&mut self, kind: AgentKind, cx: &mut Context<Self>) {
        if kind == self.chat.kind || self.chat.busy {
            return;
        }
        save_agent(&self.env.agent_file, kind);
        self.chat.kind = kind;
        if self.topic_started() {
            self.chat.items.push(Item::Notice(format!("{} continues the conversation.", agent_name(kind))));
        }
        self.connect(cx);
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    /// Ask for the folder that the agent works in. The agent starts again in the new folder,
    /// and gets the earlier messages of the topic with the next message.
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
                this.connect(cx);
                this.scroll.scroll_to_bottom();
                cx.notify();
            });
        })
        .detach();
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
                    this.chat.items.push(Item::Error(format!("Agent Mac: {e:#}")));
                }
                cx.notify();
            });
        })
        .detach();
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

        let disabled = self.chat.busy || !self.topic_started();
        let new_topic = h_flex()
            .id("new-topic")
            .mx_2()
            .px_2()
            .h(px(32.))
            .gap_2p5()
            .rounded(px(8.))
            .text_sm()
            .font_medium()
            .when(disabled, |el| el.text_color(theme.muted_foreground))
            .when(!disabled, |el| {
                el.cursor_pointer()
                    .text_color(accent)
                    .hover(|el| el.bg(accent.opacity(0.08)))
                    .on_click(cx.listener(|this, _, window, cx| this.new_topic(window, cx)))
            })
            .child(Icon::new(KitIcon::SquarePen).size(px(16.)))
            .child(div().flex_1().child("New topic"))
            .child(div().text_xs().text_color(theme.muted_foreground).child("⌘N"));

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
            .child(div().pt_2().child(new_topic))
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

    fn render_vm_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let phase = &self.vm_phase;
        let busy = self.vm_busy;
        let supported = vm::AgentMac::is_supported();
        let installed = self.env.mac.paths().bundle.join(".installed").exists();
        let mut actions = h_flex().gap_0p5();
        match phase {
            // A Mac that cannot run VMs gets only the note, not a button that does nothing.
            VmPhase::NotInstalled | VmPhase::Error(_) if !installed && !supported => {}
            VmPhase::NotInstalled | VmPhase::Error(_) if !installed => {
                actions = actions.child(
                    Button::new("vm-install")
                        .small()
                        .primary()
                        .w_full()
                        .icon(Icon::new(KitIcon::Download))
                        .label("Install macOS")
                        .disabled(busy)
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
                                    this.chat.items.push(Item::Error(format!("Agent Mac: {e:#}")));
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
                        // A tint of the accent, with the normal text color, so that every theme has
                        // good contrast.
                        .bg(accent(&theme).opacity(if theme.is_dark() { 0.24 } else { 0.12 }))
                        .text_color(theme.foreground)
                        .child(TextView::markdown(("item", ix), SharedString::from(text.clone())).selectable(true)),
                )
                .into_any_element(),
            Item::Agent(text) => {
                let group: SharedString = format!("agent-message-{ix}").into();
                let copy = text.clone();
                h_flex()
                    .group(group.clone())
                    .items_start()
                    .gap_3()
                    .child(avatar(kind, 28.))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .pt_0p5()
                            .gap_1()
                            .child(TextView::markdown(("item", ix), SharedString::from(text.clone())).selectable(true))
                            // The copy button shows when the pointer is on the message. Only the
                            // visibility changes, so the layout stays the same.
                            .child(
                                h_flex().invisible().group_hover(group, |s| s.visible()).child(
                                    Button::new(("copy", ix))
                                        .ghost()
                                        .xsmall()
                                        .icon(Icon::new(KitIcon::Copy).text_color(theme.muted_foreground))
                                        .tooltip("Copy")
                                        .on_click(move |_, _, cx| {
                                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.clone()))
                                        }),
                                ),
                            ),
                    )
                    .into_any_element()
            }
            Item::Thought(text) => h_flex()
                .items_start()
                .gap_2()
                .pl(px(40.))
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
            } => {
                // A click on the header opens or closes the output. A failed tool starts open.
                let key = id.clone();
                let open = detail.is_some() && (self.toggled_tools.contains(&key) != (*status == ToolStatus::Failed));
                v_flex()
                    .ml(px(40.))
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
                                        cx.notify();
                                    }))
                            })
                            .child(Icon::new(tool_icon(tool_kind)).size(px(14.)).text_color(theme.muted_foreground))
                            .child(div().flex_1().min_w_0().truncate().text_sm().child(title.clone()))
                            .child(status_icon(*status, &theme))
                            .when(detail.is_some(), |el| {
                                el.child(
                                    Icon::new(if open { KitIcon::ChevronDown } else { KitIcon::ChevronRight })
                                        .size(px(14.))
                                        .text_color(theme.muted_foreground),
                                )
                            }),
                    )
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
            // A line across the conversation with the title of the topic in the middle.
            Item::Topic(title) => {
                let line = || div().flex_1().h(px(1.)).bg(theme.border);
                let label = if title.is_empty() { "New topic".to_string() } else { title.clone() };
                h_flex()
                    .py_2()
                    .gap_3()
                    .child(line())
                    .child(
                        div()
                            .max_w(px(COLUMN / 2.))
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(label),
                    )
                    .child(line())
                    .into_any_element()
            }
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
    fn render_greeting(&self, kind: AgentKind, cwd: &Path, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        v_flex()
            .items_center()
            .gap_3()
            .px_6()
            .child(avatar(kind, 44.))
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
                            .on_click(move |_, _, cx| {
                                let _ = view.update(cx, |this, cx| this.pick_agent(k, cx));
                            }),
                    )
                })
            })
            .into_any_element()
    }

    fn render_composer(&self, kind: AgentKind, busy: bool, cx: &mut Context<Self>) -> gpui::AnyElement {
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
                            .when(!busy, |el| el.child("Return to send, Shift-Return for a new line")),
                    )
                    .child(action),
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
        let empty = chat.items.is_empty();
        let cwd = chat.cwd.clone();
        let mut items: Vec<gpui::AnyElement> = chat
            .items
            .iter()
            .enumerate()
            .map(|(ix, item)| self.render_item(ix, item, kind, cx))
            .collect();
        // While the agent works, a row at the end shows it. A permission request shows instead.
        if busy && chat.permission.is_none() {
            items.push(
                h_flex()
                    .gap_3()
                    .child(avatar(kind, 28.))
                    .child(Spinner::new().with_size(px(14.)).color(accent(&theme)))
                    .child(div().text_sm().text_color(theme.muted_foreground).child("Working…"))
                    .into_any_element(),
            );
        }
        let permission = chat.permission.as_ref().map(|p| self.render_permission(p, cx));
        // The line below the name of the assistant, as the status of a contact in a messages app.
        let (status, status_color) = if chat.permission.is_some() {
            ("Waiting for your permission".to_string(), theme.warning)
        } else if busy {
            ("Working…".to_string(), theme.muted_foreground)
        } else if chat.closed {
            ("Stopped. Send a message to start again.".to_string(), theme.muted_foreground)
        } else {
            (format!("{} · {}", kind.label(), display_path(&cwd)), theme.muted_foreground)
        };
        let can_start_topic = !busy && self.topic_started();
        let composer = v_flex()
            .w_full()
            .max_w(px(COLUMN))
            .mx_auto()
            .px_6()
            .gap_2()
            .children(permission)
            .child(self.render_composer(kind, busy, cx));

        let toolbar = self
            .toolbar("main-toolbar", cx)
            .px_4()
            .gap_1()
            .border_b_1()
            .border_color(theme.border)
            // Without the sidebar, the traffic lights are on this toolbar.
            .when(!self.sidebar_open, |el| {
                el.pl(px(84.)).child(sidebar_toggle("show-sidebar", "Show sidebar", cx))
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2p5()
                    .child(avatar(kind, 30.))
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().text_sm().font_semibold().child(agent_name(kind)))
                            .child(div().truncate().text_xs().text_color(status_color).child(status)),
                    ),
            )
            .when(busy, |el| {
                el.child(
                    Button::new("stop-turn")
                        .ghost()
                        .small()
                        .icon(Icon::new(KitIcon::CircleStop))
                        .tooltip_with_action("Stop", &StopAgent, None)
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                )
            })
            .child(
                Button::new("pick-folder-toolbar")
                    .ghost()
                    .small()
                    .icon(Icon::new(KitIcon::Folder))
                    .tooltip("Change the folder")
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| this.pick_folder(cx))),
            )
            .child(
                Button::new("new-topic-toolbar")
                    .ghost()
                    .small()
                    .icon(Icon::new(KitIcon::SquarePen))
                    .tooltip_with_action("New topic", &NewTopic, None)
                    .disabled(!can_start_topic)
                    .on_click(cx.listener(|this, _, window, cx| this.new_topic(window, cx))),
            );

        // An empty conversation shows only the composer, in the middle of the pane.
        let body = if empty {
            v_flex()
                .flex_1()
                .justify_center()
                // Sit a little above the center.
                .pb(px(TOOLBAR * 2.))
                .gap_6()
                .child(self.render_greeting(kind, &cwd, &theme, cx))
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
            .on_action(cx.listener(|this, _: &NewTopic, window, cx| this.new_topic(window, cx)))
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
