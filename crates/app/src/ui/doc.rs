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

use gpui::{ClipboardItem, prelude::FluentBuilder as _, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _, Task, Window, div, px};
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
pub(super) const WIDTH: f32 = 480.;
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
}

/// The documents that a message of the agent links to: the Markdown files in `dir` that the
/// text names. The agent writes a long reply to a document. See `memory::preamble`.
pub(super) fn links(text: &str, dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for (_, _, path) in text.lines().flat_map(|line| doc_spans(line, dir)) {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

/// The document that the target of a link opens, if the target is a Markdown file in `dir`.
pub(super) fn link_target(target: &str, dir: &Path) -> Option<PathBuf> {
    let target = PathBuf::from(target.trim_start_matches("file://").replace("%20", " "));
    (target.parent() == Some(dir) && target.extension().is_some_and(|e| e == "md")).then_some(target)
}

/// The links to documents in `dir` in a line: the byte range of each link, its label, and the
/// document. A link is `[label](<path>)`, `[label](path)`, or a path with no label.
fn doc_spans(line: &str, dir: &Path) -> Vec<(std::ops::Range<usize>, String, PathBuf)> {
    let mut spans = Vec::new();
    let mut at = 0;
    while let Some(open) = line[at..].find('[').map(|i| at + i) {
        let Some(close) = line[open..].find("](").map(|i| open + i) else { break };
        let rest = &line[close + 2..];
        let (target, len) = match rest.strip_prefix('<') {
            Some(r) => match r.find(">)") {
                Some(end) => (&r[..end], end + 3),
                None => break,
            },
            None => match rest.find(')') {
                Some(end) => (&rest[..end], end + 1),
                None => break,
            },
        };
        let end = close + 2 + len;
        if let Some(path) = link_target(target, dir) {
            spans.push((open..end, line[open + 1..close].to_string(), path));
        }
        at = end;
    }
    // A path with no label. The folder can have a space, and a path can have it as `%20`.
    // The file name ends at a space or a mark of Markdown, and has the `.md` extension.
    let folder = format!("{}/", dir.display());
    for prefix in [folder.clone(), folder.replace(' ', "%20")] {
        for (start, _) in line.match_indices(&prefix) {
            let begin = line[..start].strip_suffix("file://").map_or(start, |s| s.len());
            let rest = &line[start + prefix.len()..];
            let token = &rest[..rest.find(|c: char| c.is_whitespace() || "/<>()[]`\"'*".contains(c)).unwrap_or(rest.len())];
            // A sentence can end after the path.
            let name = token.trim_end_matches(['.', ',', ';', ':', '!', '?']);
            if name.len() <= 3 || !name.ends_with(".md") {
                continue;
            }
            let end = start + prefix.len() + name.len();
            if spans.iter().any(|(r, _, _)| r.start < end && begin < r.end) {
                continue;
            }
            spans.push((begin..end, String::new(), dir.join(name.replace("%20", " "))));
        }
    }
    spans.sort_by_key(|(r, _, _)| r.start);
    spans
}

/// The text of a message for the chat, without the links to its documents. The panel shows
/// the documents, so the chat does not show their names. A line with only a link goes. In a
/// sentence, a link becomes its label.
pub(super) fn without_links(text: &str, dir: &Path) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let spans = doc_spans(line, dir);
        if spans.is_empty() {
            lines.push(line.to_string());
            continue;
        }
        let (mut out, mut rest, mut last) = (String::new(), String::new(), 0);
        for (range, label, _) in &spans {
            out.push_str(&line[last..range.start]);
            rest.push_str(&line[last..range.start]);
            out.push_str(label);
            last = range.end;
        }
        out.push_str(&line[last..]);
        rest.push_str(&line[last..]);
        if rest.chars().any(char::is_alphanumeric) {
            lines.push(out.trim_end().to_string());
        }
    }
    let mut text = String::new();
    for line in lines {
        if line.trim().is_empty() && (text.is_empty() || text.ends_with("\n\n")) {
            continue;
        }
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text.trim_end().to_string()
}

/// The title of a document, from its file name: `trip-plan.md` is "Trip plan".
pub(super) fn title(path: &Path) -> String {
    // The file name has no accents and no capitals, so the first heading is better.
    if let Some(heading) = heading(path) {
        return heading;
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().replace(['-', '_'], " ")).unwrap_or_default();
    let mut chars = stem.trim().chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// The text of the first `# ` heading in the start of the file, if the file has one.
fn heading(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let mut head = Vec::new();
    std::fs::File::open(path).ok()?.take(4096).read_to_end(&mut head).ok()?;
    let head = String::from_utf8_lossy(&head);
    let heading = head.lines().find_map(|l| l.strip_prefix("# "))?.trim();
    (!heading.is_empty()).then(|| heading.to_string())
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

/// The most characters of a title that comes from the message of the user.
const TITLE: usize = 60;

/// A title from the message of the user: the first line, cut at a word, with a capital letter.
fn prompt_title(prompt: &str) -> Option<String> {
    let line = prompt.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.trim_end_matches(['.', '?', '!', ':', ',', ';']).trim_end();
    let line = if line.chars().count() <= TITLE {
        line.to_string()
    } else {
        let cut: String = line.chars().take(TITLE).collect();
        format!("{}…", cut[..cut.rfind(' ').unwrap_or(cut.len())].trim_end())
    };
    let mut chars = line.chars();
    let first = chars.next()?;
    Some(first.to_uppercase().chain(chars).collect())
}

/// Write a long reply of the agent to a new document in `dir`. Returns the text for the chat:
/// the first paragraph of the reply and a link to the document. If the reply has no heading,
/// the title comes from `prompt`, the message of the user.
pub(super) fn write_reply(text: &str, prompt: &str, dir: &Path) -> std::io::Result<String> {
    let first = headings(text).next().map(str::to_string);
    let title = first.clone().or_else(|| prompt_title(prompt)).unwrap_or_else(|| "Reply".into());
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

/// A script that gives the page the colors of the app before the page draws. Without it, the
/// page is white until it gets the colors from the app.
fn theme_script(theme: &Value) -> String {
    let rules: String = theme
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, value)| Some(format!("--help-{name}: {};", value.as_str()?)))
        .collect();
    format!(
        "(() => {{ const sheet = new CSSStyleSheet(); sheet.replaceSync({}); \
         document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet]; }})();",
        json!(format!(":root {{ {rules} }}"))
    )
}

fn build_webview(messages: async_channel::Sender<String>, theme: &Value, window: &Window) -> wry::Result<WebView> {
    WebViewBuilder::new()
        .with_incognito(true)
        .with_devtools(cfg!(debug_assertions))
        // The web view does not draw white before the page draws. The card shows below it.
        .with_transparent(true)
        // The web view shows when the editor has the text. See `on_doc_message`.
        .with_visible(false)
        .with_initialization_script(theme_script(theme))
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
            Some("loaded") => {
                if let Some(webview) = &doc.webview {
                    let _ = webview.set_visible(true);
                }
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
            match build_webview(tx, &theme, window) {
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
        let user = items.iter().rposition(|i| matches!(i, Item::User { .. }));
        let prompt = match user.map(|i| &items[i]) {
            Some(Item::User { text, .. }) => text.clone(),
            _ => String::new(),
        };
        let turn = user.map_or(0, |i| i + 1);
        let Some(Item::Agent(text)) = items[turn..].iter_mut().rev().find(|i| matches!(i, Item::Agent(_))) else {
            return;
        };
        let dir = &self.env.docs_dir;
        if links(text, dir).is_empty() && long(text) {
            match write_reply(text, &prompt, dir) {
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

        // The icon of the file, in a tinted square.
        let badge = div()
            .size(px(28.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(8.))
            .bg(accent.opacity(0.12))
            .child(Icon::new(doc.icon).size(px(15.)).text_color(accent));
        let heading = v_flex()
            .flex_1()
            .min_w_0()
            .child(div().text_sm().font_semibold().truncate().child(doc.title.clone()))
            // The header does not show the file name. It shows "Saved" after a change.
            .when(doc.saved, |el| el.child(div().text_xs().text_color(theme.muted_foreground).child("Saved")));
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
                .w(px((WIDTH + INSET * 2.).min(self.panel_room(window))))
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
        let chat = write_reply(reply, "plan my trip", &dir).unwrap();
        let path = dir.join("trip-plan-day-1.md");
        assert_eq!(chat, format!("I wrote the plan below. It has two parts.\n\n[Trip plan: Day 1](<{}>)", path.display()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), format!("{reply}\n"));
        assert_eq!(links(&chat, &dir), vec![path]);
        // A second reply with the same title gets a new file.
        assert!(write_reply(reply, "plan my trip", &dir).unwrap().contains("trip-plan-day-1-2.md"));
        // A reply with no heading gets one from the message of the user.
        let long_text = format!("1. a\n\n{}", "Long text. ".repeat(40));
        let chat = write_reply(&long_text, "how do I renew my passport?\nI live in Lagos.", &dir).unwrap();
        let path = dir.join("how-do-i-renew-my-passport.md");
        assert!(chat.ends_with(&format!("[How do I renew my passport](<{}>)", path.display())));
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("# How do I renew my passport\n\n1. a"));
        // With no message, the title is "Reply".
        let chat = write_reply(&long_text, "", &dir).unwrap();
        assert!(chat.ends_with(&format!("[Reply](<{}>)", dir.join("reply.md").display())));
        assert!(chat.starts_with("Long text.") && chat.len() < 400);
        assert!(std::fs::read_to_string(dir.join("reply.md")).unwrap().starts_with("# Reply\n\n1. a"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_chat_does_not_show_the_links_to_documents() {
        let dir = Path::new("/Users/me/Library/Application Support/Help/documents");
        let text = "The plan has two days.\n\n[Trip plan](</Users/me/Library/Application Support/Help/documents/trip-plan.md>)";
        assert_eq!(without_links(text, dir), "The plan has two days.");
        let text = "Read [the plan](file:///Users/me/Library/Application%20Support/Help/documents/trip-plan.md) first.\n\n\
                    - /Users/me/Library/Application Support/Help/documents/budget.md\n\nSee [the docs](https://x.org/a.md).";
        assert_eq!(without_links(text, dir), "Read the plan first.\n\nSee [the docs](https://x.org/a.md).");
        assert_eq!(without_links("No links.", dir), "No links.");
    }

    #[test]
    fn a_document_name_ends_at_its_last_extension() {
        let dir = Path::new("/Users/me/Library/Application Support/Help/documents");
        let text = "See /Users/me/Library/Application Support/Help/documents/plan.md.backup.md.";
        assert_eq!(links(text, dir), vec![dir.join("plan.md.backup.md")]);
        assert_eq!(without_links(text, dir), "See .");
        let target = "/Users/me/Library/Application%20Support/Help/documents/plan.md.backup.md";
        assert_eq!(link_target(target, dir), Some(dir.join("plan.md.backup.md")));
        assert_eq!(link_target("https://x.org/a.md", dir), None);
    }

    #[test]
    fn a_path_that_is_not_a_document_keeps_its_text() {
        let dir = Path::new("/Users/me/Library/Application Support/Help/documents");
        // A path with no `.md` name, before a link to a document on the same line.
        let text = "In /Users/me/Library/Application Support/Help/documents/ is [the plan](</Users/me/Library/Application Support/Help/documents/plan.md>).";
        assert_eq!(without_links(text, dir), "In /Users/me/Library/Application Support/Help/documents/ is the plan.");
        assert_eq!(links(text, dir), vec![dir.join("plan.md")]);
        // A path to a file that is not Markdown.
        let text = "The file /Users/me/Library/Application Support/Help/documents/data.csv has the numbers.";
        assert_eq!(without_links(text, dir), text);
        assert!(links(text, dir).is_empty());
    }

    #[test]
    fn a_long_message_gives_a_short_title() {
        assert_eq!(prompt_title("  \nfix the bug.  "), Some("Fix the bug".into()));
        let title = prompt_title(&"word ".repeat(30)).unwrap();
        assert!(title.ends_with("word…") && title.chars().count() <= TITLE + 1);
        assert_eq!(prompt_title("\n \n"), None);
    }

    #[test]
    fn the_page_gets_the_colors_before_it_draws() {
        let script = theme_script(&json!({ "background": "rgba(1, 2, 3, 1.000)", "font": "\"Inter\", sans-serif" }));
        assert!(script.contains(r#"--help-background: rgba(1, 2, 3, 1.000);"#));
        assert!(script.contains(r#"--help-font: \"Inter\", sans-serif;"#));
    }

    #[test]
    fn the_title_comes_from_the_file_name() {
        assert_eq!(title(Path::new("/d/trip-plan.md")), "Trip plan");
        assert_eq!(title(Path::new("/d/code_review.md")), "Code review");
        let path = std::env::temp_dir().join(format!("help-title-{}.md", std::process::id()));
        std::fs::write(&path, "Intro.\n\n# Paramètres de sécurité\n\n## Part\n").unwrap();
        assert_eq!(title(&path), "Paramètres de sécurité");
        std::fs::remove_file(&path).unwrap();
    }
}
