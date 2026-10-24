//! A Markdown file in a panel at the right side of the window: the memory file, the
//! conversation summary, or a document that the agent wrote for a long reply. The user edits
//! the Markdown as formatted text, as in Notion. The editor is a web page
//! (see `web/doc-editor`) in a web view. The page sends each change, and the app saves it.

use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    rc::Rc,
    time::SystemTime,
};

use gpui::{ClipboardItem, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _, Task, Window, div, px};
use gpui_component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _, Theme,
    button::{Button, ButtonVariants as _},
    v_flex,
};
use gpui_kit_assets::IconName as KitIcon;
use serde_json::{Value, json};
use wry::{
    NewWindowResponse, Rect, WebView, WebViewBuilder,
    dpi::{LogicalPosition, LogicalSize},
    http::{Response, header::CONTENT_TYPE},
};

use super::{AppView, Context, INSET, Item, TOOLBAR, accent, mcp_app};

/// The width of the panel.
const WIDTH: f32 = 480.;
/// The scheme of the address of the editor page.
const SCHEME: &str = "helpdoc";
/// The editor page, built from `web/doc-editor` with `npm run build`.
const EDITOR: &str = include_str!("../../web/doc-editor.html");

/// A Markdown file in the panel.
pub(super) struct Doc {
    title: SharedString,
    icon: KitIcon,
    pub(super) path: PathBuf,
    /// The text of the file, as the app read it or saved it last.
    text: String,
    /// The time of the last change to the file, when the app read it or saved it.
    modified: Option<SystemTime>,
    /// The app makes the web view when it renders the panel the first time, because the web
    /// view needs the window.
    webview: Option<Rc<WebView>>,
    /// The place of the web view in the window.
    bounds: mcp_app::PanelBounds,
    /// The page can get the text. The app sends nothing before that.
    ready: bool,
    /// The app saved a change from the editor.
    saved: bool,
    /// The colors and fonts that the page has.
    theme: Option<Value>,
    /// The editor got the keyboard focus. The next render takes the focus from the elements of
    /// the app, so that the keys of the app, such as Command-Z, do not go to the composer.
    blur: bool,
    /// The task that gets the messages of the page.
    _inbox: Option<Task<()>>,
}

impl Doc {
    fn open(title: SharedString, icon: KitIcon, path: &Path) -> Self {
        let mut doc = Self {
            title,
            icon,
            path: path.to_path_buf(),
            text: String::new(),
            modified: None,
            webview: None,
            bounds: Default::default(),
            ready: false,
            saved: false,
            theme: None,
            blur: false,
            _inbox: None,
        };
        doc.read();
        doc
    }

    fn read(&mut self) {
        self.modified = modified(&self.path);
        self.text = std::fs::read_to_string(&self.path).unwrap_or_default();
    }

    /// Read the file again if it changed after the last read or save. The agent can change the
    /// file while it shows. Returns `true` if the text changed.
    fn reload(&mut self) -> bool {
        if modified(&self.path) == self.modified {
            return false;
        }
        let old = std::mem::take(&mut self.text);
        self.read();
        self.text != old
    }

    fn save(&mut self, text: String) {
        if text == self.text {
            return;
        }
        if let Err(e) = std::fs::write(&self.path, &text) {
            tracing::warn!("cannot save {}: {e}", self.path.display());
            return;
        }
        self.text = text;
        self.modified = modified(&self.path);
        self.saved = true;
    }

    /// Call a function of the page with one argument.
    fn call(&self, function: &str, arg: &Value) {
        if let Some(webview) = &self.webview
            && let Err(e) = webview.evaluate_script(&format!("window.helpDoc.{function}({arg})"))
        {
            tracing::warn!("cannot send to the editor: {e}");
        }
    }

    fn load(&self) {
        self.call("load", &json!(self.text));
    }

    /// Hide the web view now. The web view goes when the panel goes.
    fn hide(&self) {
        if let Some(webview) = &self.webview {
            let _ = webview.set_visible(false);
        }
    }

    fn file_name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }
}

/// The documents that a message of the agent links to: the Markdown files in `dir` that the
/// text names. The agent writes a long reply to a document. See `memory::preamble`.
pub(super) fn links(text: &str, dir: &Path) -> Vec<PathBuf> {
    // The path of the folder has a space, and a link can have it as `%20`.
    let text = text.replace("%20", " ");
    let prefix = format!("{}/", dir.display());
    let mut paths = Vec::new();
    for (start, _) in text.match_indices(&prefix) {
        let rest = &text[start + prefix.len()..];
        let Some(end) = rest.find(".md").filter(|&end| end > 0) else { continue };
        let name = &rest[..end + 3];
        if name.contains(['/', '\n', '<', '>', '(', ')', '[', ']', '`']) {
            continue;
        }
        let path = dir.join(name);
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

/// The title of a document, from its file name: `trip-plan.md` is "Trip plan".
pub(super) fn title(path: &Path) -> String {
    let stem = path.file_stem().map(|s| s.to_string_lossy().replace(['-', '_'], " ")).unwrap_or_default();
    let mut chars = stem.trim().chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// A reply with more characters than this goes to a document. See `long`.
const LONG_REPLY: usize = 1500;
/// The most characters of the summary that stays in the chat.
const SUMMARY: usize = 300;

/// The headings of a Markdown text, without the `#` marks. The lines in code blocks are not
/// headings.
fn headings(text: &str) -> impl Iterator<Item = &str> {
    let mut code = false;
    text.lines().filter_map(move |line| {
        let line = line.trim_start();
        if line.starts_with("```") {
            code = !code;
        }
        let rest = line.trim_start_matches('#');
        (!code && line.starts_with('#') && rest.starts_with(' ')).then(|| rest.trim())
    })
}

/// The reply is too long for the chat: it has many characters, or the headings of a document.
pub(super) fn long(text: &str) -> bool {
    text.chars().count() > LONG_REPLY || headings(text).count() >= 2
}

/// The first paragraph of plain text, cut at the end of a sentence or a word.
fn summary(text: &str) -> Option<String> {
    let plain = |p: &&str| {
        let p = p.trim_start();
        !p.is_empty()
            && !p.starts_with(['#', '-', '*', '|', '>', '`', '<'])
            && !p.split_once(". ").is_some_and(|(n, _)| n.chars().all(|c| c.is_ascii_digit()))
    };
    let paragraph = text.split("\n\n").find(plain)?.trim().replace('\n', " ");
    if paragraph.chars().count() <= SUMMARY {
        return Some(paragraph);
    }
    let cut: String = paragraph.chars().take(SUMMARY).collect();
    Some(match cut.rfind(". ") {
        Some(end) if end > SUMMARY / 3 => cut[..=end].to_string(),
        _ => format!("{}…", cut[..cut.rfind(' ').unwrap_or(cut.len())].trim_end()),
    })
}

/// A file name from a title: lowercase words and hyphens.
fn slug(title: &str) -> String {
    let mut slug = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.chars().take(50).collect();
    match slug.trim_matches('-') {
        "" => "reply".into(),
        s => s.into(),
    }
}

/// Write a long reply of the agent to a new document in `dir`. Returns the text for the chat:
/// the first paragraph of the reply and a link to the document.
pub(super) fn write_reply(text: &str, dir: &Path) -> std::io::Result<String> {
    let first = headings(text).next().map(str::to_string);
    let title = first.clone().unwrap_or_else(|| "Reply".into());
    let stem = slug(&title);
    let path = (1..)
        .map(|n| dir.join(if n == 1 { format!("{stem}.md") } else { format!("{stem}-{n}.md") }))
        .find(|p| !p.exists())
        .unwrap_or_default();
    let body = match first {
        Some(_) => format!("{}\n", text.trim()),
        None => format!("# {title}\n\n{}\n", text.trim()),
    };
    std::fs::create_dir_all(dir)?;
    std::fs::write(&path, body)?;
    let summary = summary(text).unwrap_or_else(|| "The reply is in a document.".into());
    Ok(format!("{summary}\n\n[{title}](<{}>)", path.display()))
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// A color as CSS.
fn css(color: Hsla) -> String {
    let c = gpui::Rgba::from(color);
    let byte = |v: f32| (v * 255.).round() as u8;
    format!("rgba({}, {}, {}, {:.3})", byte(c.r), byte(c.g), byte(c.b), c.a)
}

/// The colors and fonts of the app, for the page.
fn page_theme(theme: &Theme) -> Value {
    let accent = accent(theme);
    // A font of the system has a name that starts with a period. The page uses the system font.
    let font = |name: &str, fallback: &str| {
        if name.is_empty() || name.starts_with('.') { fallback.to_string() } else { format!("\"{name}\", {fallback}") }
    };
    json!({
        "background": css(theme.background),
        "foreground": css(theme.foreground),
        "muted": css(theme.muted),
        "muted-foreground": css(theme.muted_foreground),
        "border": css(theme.border),
        "popover": css(theme.popover),
        "accent": css(accent),
        "selection": css(accent.opacity(0.2)),
        "font": font(&theme.font_family, "-apple-system, BlinkMacSystemFont, system-ui, sans-serif"),
        "mono": font(&theme.mono_font_family, "ui-monospace, Menlo, monospace"),
    })
}

fn build_webview(messages: async_channel::Sender<String>, window: &Window) -> wry::Result<WebView> {
    WebViewBuilder::new()
        .with_incognito(true)
        .with_devtools(cfg!(debug_assertions))
        .with_bounds(Rect { position: LogicalPosition::new(0., 0.).into(), size: LogicalSize::new(0., 0.).into() })
        .with_custom_protocol(SCHEME.into(), |_, request| {
            let found = matches!(request.uri().path(), "" | "/" | "/index.html");
            Response::builder()
                .status(if found { 200 } else { 404 })
                .header(CONTENT_TYPE, "text/html; charset=utf-8")
                .body(Cow::Borrowed(if found { EDITOR.as_bytes() } else { &[][..] }))
                .unwrap_or_default()
        })
        .with_ipc_handler(move |request| {
            let _ = messages.try_send(request.into_body());
        })
        // The page stays on its page. A link that opens a window opens in the browser.
        .with_navigation_handler(|url| url.starts_with(&format!("{SCHEME}://")) || url == "about:blank")
        .with_new_window_req_handler(|url, _| {
            let _ = mcp_app::open_link(&url);
            NewWindowResponse::Deny
        })
        .with_url(format!("{SCHEME}://localhost/"))
        .build_as_child(window)
}

impl AppView {
    /// Show a Markdown file in the panel. If the file shows already, the panel closes.
    pub(super) fn open_doc(&mut self, title: impl Into<SharedString>, icon: KitIcon, path: &Path, cx: &mut Context<Self>) {
        let same = self.doc.as_ref().is_some_and(|d| d.path == path);
        self.close_doc(cx);
        if !same {
            self.close_app(cx);
            self.doc = Some(Doc::open(title.into(), icon, path));
        }
        cx.notify();
    }

    /// Close the panel. The page sent each change already, so the file has all of them.
    pub(super) fn close_doc(&mut self, cx: &mut Context<Self>) {
        if let Some(doc) = self.doc.take() {
            doc.hide();
        }
        cx.notify();
    }

    /// The user clicked the app outside the editor. The keys go to the app again.
    pub(super) fn unfocus_doc(&self) {
        if let Some(webview) = self.doc.as_ref().and_then(|d| d.webview.as_ref()) {
            let _ = webview.focus_parent();
        }
    }

    /// Show the new text of the file in the editor, if the file changed.
    pub(super) fn reload_doc(&mut self, cx: &mut Context<Self>) {
        let Some(doc) = &mut self.doc else { return };
        if doc.reload() && doc.ready {
            doc.load();
            cx.notify();
        }
    }

    /// A message from the page.
    fn on_doc_message(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(doc) = &mut self.doc else { return };
        let Ok(message) = serde_json::from_str::<Value>(&text) else { return };
        match message.get("type").and_then(Value::as_str) {
            Some("ready") => {
                doc.ready = true;
                doc.theme = None;
                doc.load();
            }
            Some("change") => {
                let Some(markdown) = message.get("markdown").and_then(Value::as_str) else { return };
                doc.save(markdown.to_string());
            }
            Some("focus") => doc.blur = true,
            Some("paste") => {
                let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else { return };
                doc.call("paste", &json!(text));
                return;
            }
            _ => return,
        }
        cx.notify();
    }

    /// Make the web view if the panel has none, and give the page a change of the theme.
    fn prepare_doc(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<(Rc<WebView>, mcp_app::PanelBounds)> {
        let theme = page_theme(cx.theme());
        let doc = self.doc.as_mut()?;
        if std::mem::take(&mut doc.blur) {
            window.blur(cx);
        }
        if doc.webview.is_none() {
            let (tx, rx) = async_channel::unbounded::<String>();
            match build_webview(tx, window) {
                Ok(webview) => doc.webview = Some(Rc::new(webview)),
                Err(e) => {
                    tracing::warn!("cannot show the editor: {e}");
                    self.doc = None;
                    return None;
                }
            }
            doc._inbox = Some(cx.spawn(async move |this, cx| {
                while let Ok(text) = rx.recv().await {
                    if this.update(cx, |this, cx| this.on_doc_message(text, cx)).is_err() {
                        break;
                    }
                }
            }));
        }
        if doc.ready && doc.theme.as_ref() != Some(&theme) {
            doc.call("theme", &theme);
            doc.theme = Some(theme);
        }
        Some((doc.webview.clone()?, doc.bounds.clone()))
    }

    /// Show a document that the agent wrote in the panel.
    pub(super) fn open_document(&mut self, path: &Path, cx: &mut Context<Self>) {
        self.open_doc(title(path), KitIcon::FileText, path, cx);
    }

    /// At the end of a turn, move a long final reply to a document, so that the chat stays
    /// short. Then show the document of the reply in the panel.
    pub(super) fn finish_reply(&mut self, cx: &mut Context<Self>) {
        let items = &mut self.chat.items;
        let turn = items.iter().rposition(|i| matches!(i, Item::User { .. })).map_or(0, |i| i + 1);
        let Some(Item::Agent(text)) = items[turn..].iter_mut().rev().find(|i| matches!(i, Item::Agent(_))) else {
            return;
        };
        let dir = &self.env.docs_dir;
        if links(text, dir).is_empty() && long(text) {
            match write_reply(text, dir) {
                Ok(short) => *text = short,
                Err(e) => tracing::warn!("cannot write the reply to a document: {e}"),
            }
        }
        let Some(path) = links(text, dir).into_iter().find(|p| p.is_file()) else { return };
        if self.doc.as_ref().is_none_or(|d| d.path != path) {
            self.open_document(&path, cx);
        }
    }

    /// The panel with the Markdown file. It floats inset from the window edges, as the
    /// sidebar does.
    pub(super) fn render_doc(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let (webview, bounds) = self.prepare_doc(window, cx)?;
        let doc = self.doc.as_ref()?;
        let theme = cx.theme().clone();
        let accent = accent(&theme);

        let status = if doc.saved { format!("{} · Saved", doc.file_name()) } else { doc.file_name() };
        // The icon of the file, in a tinted square.
        let badge = div()
            .size(px(28.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .bg(accent.opacity(0.12))
            .child(Icon::new(doc.icon.clone()).size(px(15.)).text_color(accent));
        let heading = v_flex()
            .flex_1()
            .min_w_0()
            .child(div().text_sm().font_semibold().truncate().child(doc.title.clone()))
            .child(div().text_xs().text_color(theme.muted_foreground).truncate().child(status));
        let header = self
            .toolbar("doc-toolbar", cx)
            .h(px(TOOLBAR))
            .pl_3()
            .pr_1p5()
            .gap_2p5()
            .border_b_1()
            .border_color(theme.border.opacity(0.6))
            .child(badge)
            .child(heading)
            .child({
                let text = doc.text.clone();
                Button::new("copy-doc")
                    .ghost()
                    .small()
                    .icon(KitIcon::Copy)
                    .tooltip("Copy the Markdown")
                    .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(text.clone())))
            })
            .child(
                Button::new("close-doc")
                    .ghost()
                    .small()
                    .icon(KitIcon::X)
                    .tooltip("Close")
                    .on_click(cx.listener(|this, _, _, cx| this.close_doc(cx))),
            );

        // The web view is a native view on top of the window. It follows the place of this
        // element. The space below it keeps the round corners of the card.
        let body = div().flex_1().min_h_0().pb_3().child(
            gpui::canvas(
                move |b, _, _| {
                    bounds.set(Some(b));
                    mcp_app::place(&webview, b);
                },
                |_, _, _, _| {},
            )
            .size_full(),
        );

        let card = v_flex()
            .size_full()
            .rounded(px(16.))
            .overflow_hidden()
            .bg(theme.background)
            .border_1()
            .border_color(theme.border)
            .shadow_md()
            .child(header)
            .child(body);
        Some(
            div()
                .w(px(WIDTH + INSET * 2.))
                .h_full()
                .flex_shrink_0()
                .p(px(INSET))
                .child(card)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_links_to_the_documents_in_the_folder() {
        let dir = Path::new("/Users/me/Library/Application Support/Help/documents");
        let text = "The plan is ready: [Trip plan](</Users/me/Library/Application Support/Help/documents/trip-plan.md>). \
                    Also file:///Users/me/Library/Application%20Support/Help/documents/budget.md and \
                    [again](</Users/me/Library/Application Support/Help/documents/trip-plan.md>). \
                    Not /Users/me/notes.md, not /Users/me/Library/Application Support/Help/documents/sub/x.md.";
        assert_eq!(links(text, dir), vec![dir.join("trip-plan.md"), dir.join("budget.md")]);
        assert!(links("no documents", dir).is_empty());
    }

    #[test]
    fn a_long_reply_goes_to_a_document() {
        assert!(!long("Paris."));
        assert!(!long("# One heading\n\nShort."));
        assert!(long("## A\n\ntext\n\n## B\n\ntext"));
        assert!(!long("```sh\n# a comment\n# another\n```"));
        assert!(long(&"word ".repeat(400)));

        let dir = std::env::temp_dir().join(format!("help-docs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let reply = "I wrote the plan below. It has two parts.\n\n---\n\n## Trip plan: Day 1\n\n- Go\n\n## Day 2\n\n- Come back";
        let chat = write_reply(reply, &dir).unwrap();
        let path = dir.join("trip-plan-day-1.md");
        assert_eq!(chat, format!("I wrote the plan below. It has two parts.\n\n[Trip plan: Day 1](<{}>)", path.display()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{reply}\n"));
        assert_eq!(links(&chat, &dir), vec![path]);
        // A second reply with the same title gets a new file.
        assert!(write_reply(reply, &dir).unwrap().contains("trip-plan-day-1-2.md"));
        // A reply with no heading gets one.
        let chat = write_reply(&format!("1. a\n\n{}", "Long text. ".repeat(40)), &dir).unwrap();
        assert!(chat.ends_with(&format!("[Reply](<{}>)", dir.join("reply.md").display())));
        assert!(chat.starts_with("Long text.") && chat.len() < 400);
        assert!(std::fs::read_to_string(dir.join("reply.md")).unwrap().starts_with("# Reply\n\n1. a"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_title_comes_from_the_file_name() {
        assert_eq!(title(Path::new("/d/trip-plan.md")), "Trip plan");
        assert_eq!(title(Path::new("/d/code_review.md")), "Code review");
    }
}
