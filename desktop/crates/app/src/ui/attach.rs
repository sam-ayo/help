//! Files in messages: the attachments of the user, and the images and files of the agent.
//! An image shows as a picture. A video shows as a picture from Quick Look, when Quick Look
//! can make one. A different file shows as an icon with its name and size.

use std::{
    hash::{DefaultHasher, Hash as _, Hasher as _},
    path::{Path, PathBuf},
    process::Command,
};

use acp_host::Attachment;
use gpui::{
    App, ElementId, InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, Sizable as _, h_flex,
    button::{Button, ButtonVariants as _},
    menu::{ContextMenuExt as _, PopupMenuItem},
    v_flex,
};
use gpui_kit_assets::IconName as KitIcon;

/// The height of a picture in a message.
const PICTURE: f32 = 160.;
/// The widest picture in a message.
const MAX_PICTURE: f32 = 320.;
/// The narrowest picture in a message. A tall picture gets this width, and Cover cuts it.
const MIN_PICTURE: f32 = 96.;
/// The size of a picture in the composer.
const PREVIEW: f32 = 56.;
/// The most characters of a file name in a tile in a message (two lines), and in the composer
/// (one line).
const TILE_NAME: usize = 64;
const PREVIEW_NAME: usize = 18;

/// The text with an ellipsis in the middle if it has more than `max` characters. Use it for
/// names with an important end, as file names: "IMG_2025…edited.HEIC" keeps the extension.
pub fn middle(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max || max < 3 {
        return text.to_string();
    }
    let ext = text.rfind('.').map_or(0, |i| text[i..].chars().count()).min(max / 3);
    let tail = ((max - 1) / 2).max(ext);
    let head = max - 1 - tail;
    let start: String = chars[..head].iter().collect();
    let end: String = chars[chars.len() - tail..].iter().collect();
    format!("{}…{}", start.trim_end(), end.trim_start())
}

/// The image types that GPUI can draw.
fn drawable(a: &Attachment) -> bool {
    matches!(
        a.mime.as_str(),
        "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/bmp" | "image/tiff" | "image/svg+xml"
    )
}

/// A file size for the user: "512 bytes", "12 KB", "3.4 MB".
pub fn size_label(bytes: u64) -> String {
    const KB: f64 = 1000.;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} bytes")
    } else if b < KB * KB {
        format!("{:.0} KB", b / KB)
    } else if b < KB * KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else {
        format!("{:.1} GB", b / (KB * KB * KB))
    }
}

fn icon(a: &Attachment) -> KitIcon {
    let mime = a.mime.as_str();
    match mime {
        _ if a.is_image() => KitIcon::FileImage,
        _ if a.is_video() => KitIcon::FilePlay,
        _ if mime.starts_with("audio/") => KitIcon::FileMusic,
        "text/csv" | "application/vnd.ms-excel" | "application/vnd.apple.numbers" => KitIcon::FileSpreadsheet,
        _ if mime.contains("spreadsheetml") => KitIcon::FileSpreadsheet,
        "application/zip" | "application/gzip" | "application/x-tar" => KitIcon::FileArchive,
        "application/json" | "text/html" | "text/xml" => KitIcon::FileCode,
        _ if mime.starts_with("text/") || mime == "application/pdf" || mime.contains("document") => KitIcon::FileText,
        _ => KitIcon::File,
    }
}

fn run(program: &str, args: &[&std::ffi::OsStr]) {
    if let Err(e) = Command::new(program).args(args).spawn() {
        tracing::warn!("cannot run {program}: {e}");
    }
}

/// Open the file in its default app.
pub fn open(path: &Path) {
    run("open", &[path.as_os_str()]);
}

/// Show the file at full size in Quick Look.
pub fn quick_look(path: &Path) {
    run("qlmanage", &["-p".as_ref(), path.as_os_str()]);
}

/// Show the file in Finder.
pub fn reveal(path: &Path) {
    run("open", &["-R".as_ref(), path.as_os_str()]);
}

/// The place of the Quick Look picture of a video. The name comes from the path of the video.
pub fn thumbnail_path(dir: &Path, a: &Attachment) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    a.path.hash(&mut hasher);
    a.size.hash(&mut hasher);
    dir.join("thumbs").join(format!("{:016x}.png", hasher.finish()))
}

/// Make the Quick Look picture of a video, if it is not there yet. This can take a second, so
/// call it off the UI thread. Returns true if a new picture is there.
pub fn make_thumbnail(dir: &Path, a: &Attachment) -> bool {
    let target = thumbnail_path(dir, a);
    if !a.is_video() || target.exists() {
        return false;
    }
    let Some(out) = target.parent().map(|p| p.join(format!("tmp-{}", std::process::id()))) else {
        return false;
    };
    if std::fs::create_dir_all(&out).is_err() {
        return false;
    }
    let ok = Command::new("qlmanage")
        .args(["-t", "-s", "480", "-o"])
        .arg(&out)
        .arg(&a.path)
        .output()
        .is_ok_and(|o| o.status.success());
    // Quick Look writes `<file name>.png` in the output folder.
    let made = out.join(format!("{}.png", a.path.file_name().unwrap_or_default().to_string_lossy()));
    let moved = ok && std::fs::rename(&made, &target).is_ok();
    let _ = std::fs::remove_dir_all(&out);
    moved
}

/// The picture that shows for the attachment, if it has one.
fn picture(a: &Attachment, dir: &Path) -> Option<PathBuf> {
    if drawable(a) && a.path.exists() {
        return Some(a.path.clone());
    }
    let thumb = thumbnail_path(dir, a);
    (a.is_video() && thumb.exists()).then_some(thumb)
}

/// The menu of an attachment.
fn with_menu(el: gpui::Stateful<gpui::Div>, path: PathBuf) -> impl IntoElement {
    el.context_menu(move |menu, _, _| {
        let (open_path, look, finder, copy) = (path.clone(), path.clone(), path.clone(), path.clone());
        menu.item(PopupMenuItem::new("Open").on_click(move |_, _, _| open(&open_path)))
            .item(PopupMenuItem::new("Quick Look").on_click(move |_, _, _| quick_look(&look)))
            .item(PopupMenuItem::new("Show in Finder").on_click(move |_, _, _| reveal(&finder)))
            .separator()
            .item(PopupMenuItem::new("Copy Path").on_click(move |_, _, cx| {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(copy.to_string_lossy().into()))
            }))
    })
}

/// The attachments of a message, in rows that wrap. A click opens a file in its default app.
/// The menu has Quick Look and Finder.
pub fn tiles(
    id: impl Into<SharedString>,
    files: &[Attachment],
    dir: &Path,
    end: bool,
    cx: &App,
) -> gpui::AnyElement {
    let id: SharedString = id.into();
    let theme = cx.theme();
    h_flex()
        .flex_wrap()
        .gap_2()
        .when(end, |el| el.justify_end())
        .children(files.iter().enumerate().map(|(i, a)| {
            let path = a.path.clone();
            let el_id = ElementId::NamedInteger(id.clone(), i as u64);
            let missing = !a.path.exists();
            let tile = match picture(a, dir) {
                // The box has the height of a picture, and a width in limits. The picture fills
                // it and Cover cuts it. A `max_w` on the image itself makes it lower too, because
                // the image keeps its aspect ratio: a panorama becomes a thin line.
                Some(src) => div()
                    .id(el_id)
                    .relative()
                    .flex()
                    .h(px(PICTURE))
                    .min_w(px(MIN_PICTURE))
                    .max_w(px(MAX_PICTURE))
                    .rounded(px(12.))
                    .overflow_hidden()
                    .border_1()
                    .border_color(theme.border)
                    .cursor_pointer()
                    .child(
                        gpui::img(src)
                            .h_full()
                            .min_w_0()
                            .flex_grow(1.)
                            .object_fit(ObjectFit::Cover)
                            .with_fallback({
                                let name = a.name.clone();
                                move || div().p_2().text_xs().child(name.clone()).into_any_element()
                            }),
                    )
                    .when(a.is_video(), |el| {
                        el.child(
                            div().absolute().inset_0().flex().items_center().justify_center().child(
                                div()
                                    .p_2()
                                    .rounded_full()
                                    .bg(gpui::black().opacity(0.5))
                                    .child(Icon::new(KitIcon::Play).size(px(18.)).text_color(gpui::white())),
                            ),
                        )
                    }),
                None => div()
                    .id(el_id)
                    .child(
                        h_flex()
                            .gap_2p5()
                            .px_3()
                            .py_2()
                            .max_w(px(MAX_PICTURE))
                            .rounded(px(12.))
                            .border_1()
                            .border_color(theme.border)
                            .bg(theme.background)
                            .cursor_pointer()
                            .hover(|el| el.bg(theme.secondary))
                            .child(Icon::new(icon(a)).size(px(22.)).text_color(theme.muted_foreground))
                            .child(
                                v_flex()
                                    .min_w_0()
                                    .child(div().text_sm().line_clamp(2).child(middle(&a.name, TILE_NAME)))
                                    .child(div().text_xs().text_color(theme.muted_foreground).child(if missing {
                                        "The file is not there".to_string()
                                    } else {
                                        size_label(a.size)
                                    })),
                            ),
                    ),
            };
            let tooltip_path = a.path.display().to_string();
            with_menu(
                tile.on_click(move |_, _, _| open(&path))
                    .tooltip(move |window, cx| gpui_component::tooltip::Tooltip::new(tooltip_path.clone()).build(window, cx)),
                a.path.clone(),
            )
        }))
        .into_any_element()
}

/// The attachments in the composer, each with a button that removes it. They are in one row
/// that scrolls, as in Messages. Rows that wrap make the composer higher than a small window.
pub fn previews(
    files: &[Attachment],
    dir: &Path,
    remove: impl Fn(usize, &mut Window, &mut App) + Clone + 'static,
    cx: &App,
) -> gpui::AnyElement {
    let theme = cx.theme();
    h_flex()
        .id("composer-attachments")
        .overflow_x_scroll()
        .gap_2()
        .px_2()
        .pt_2()
        .children(files.iter().enumerate().map(|(i, a)| {
            let path = a.path.clone();
            let remove = remove.clone();
            let body = match picture(a, dir) {
                Some(src) => div()
                    .size(px(PREVIEW))
                    .rounded(px(10.))
                    .overflow_hidden()
                    .border_1()
                    .border_color(theme.border)
                    .child(gpui::img(src).size_full().object_fit(ObjectFit::Cover))
                    .into_any_element(),
                None => h_flex()
                    .h(px(PREVIEW))
                    .max_w(px(220.))
                    .gap_2()
                    .px_3()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.secondary)
                    .child(Icon::new(icon(a)).size(px(20.)).text_color(theme.muted_foreground))
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(div().text_sm().truncate().child(middle(&a.name, PREVIEW_NAME)))
                            .child(div().text_xs().text_color(theme.muted_foreground).child(size_label(a.size))),
                    )
                    .into_any_element(),
            };
            div()
                .id(("attachment", i))
                .flex_shrink_0()
                .group("attachment")
                .relative()
                .cursor_pointer()
                .on_click(move |_, _, _| quick_look(&path))
                .child(body)
                .child(
                    div()
                        .absolute()
                        .top(px(-6.))
                        .right(px(-6.))
                        .child(
                            Button::new(("remove-attachment", i))
                                .xsmall()
                                .rounded(gpui_component::button::ButtonRounded::Size(px(9.)))
                                .with_size(px(18.))
                                .secondary()
                                .icon(Icon::new(KitIcon::X).size(px(10.)))
                                .tooltip("Remove")
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    remove(i, window, cx)
                                }),
                        ),
                )
        }))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_well() {
        assert_eq!(size_label(512), "512 bytes");
        assert_eq!(size_label(12_300), "12 KB");
        assert_eq!(size_label(3_400_000), "3.4 MB");
        assert_eq!(size_label(1_100_000_000), "1.1 GB");
    }

    #[test]
    fn long_names_lose_their_middle() {
        assert_eq!(middle("report.pdf", 34), "report.pdf");
        let name = "Q3 Board Deck — FINAL (revised) v12 [approved by legal].pdf";
        assert_eq!(middle(name, TILE_NAME), name);
        let short = middle(name, 34);
        assert_eq!(short.chars().count(), 34);
        assert!(short.starts_with("Q3 Board Deck") && short.ends_with("legal].pdf"), "{short}");
        let id = middle("9f8e7d6c-5b4a-4c3d-8e2f-1a0b9c8d7e6f.zip", PREVIEW_NAME);
        assert_eq!(id.chars().count(), PREVIEW_NAME);
        assert!(id.ends_with("7e6f.zip"), "{id}");
    }
}
