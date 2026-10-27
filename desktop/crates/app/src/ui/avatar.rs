//! The avatar of the assistant: a "marble" avatar from Boring Avatars
//! (https://boringavatars.com, MIT), with the seed "assistant". A port of the marble variant
//! of boring-avatars 2.0.4.
//!
//! The moving avatar is a loop of frames. The two blobs of color turn and drift, so the
//! gradient moves. GPUI plays the frames as it plays a GIF. It stops when the window is not
//! active, and when "Reduce motion" is on in the macOS settings.
//!
//! The Dock icon and the menu bar item are the avatar too, in the same colors. See `dock`
//! and `presence`.

use std::{
    f32::consts::TAU,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use super::dock;
use crate::presence;
use gpui::{App, DevicePixels, RenderImage, SvgRenderer, SvgSize, size};
use image::{Delay, Frame, ImageEncoder, RgbaImage};

/// The colors of an avatar. The user picks one in the theme menu.
pub struct Palette {
    pub key: &'static str,
    pub title: &'static str,
    pub background: &'static str,
    /// The blob in front of the background.
    pub blob: &'static str,
    /// The blob in front, with the "overlay" blend mode.
    pub overlay: &'static str,
}

impl Palette {
    /// The three colors, for the swatch in the menu.
    pub fn colors(&self) -> [u32; 3] {
        [hex(self.background), hex(self.blob), hex(self.overlay)]
    }
}

/// The first palette is the default. The "Aurora" colors are the colors that Boring Avatars
/// picks with the seed "assistant" from #22d3ee #818cf8 #e879f9 #38bdf8 #c084fc.
pub const PALETTES: [Palette; 8] = [
    Palette { key: "aurora", title: "Aurora", background: "#38bdf8", blob: "#c084fc", overlay: "#22d3ee" },
    Palette { key: "sunset", title: "Sunset", background: "#fb7185", blob: "#f97316", overlay: "#c084fc" },
    Palette { key: "mint", title: "Mint", background: "#2dd4bf", blob: "#a3e635", overlay: "#22d3ee" },
    Palette { key: "ocean", title: "Ocean", background: "#3b82f6", blob: "#14b8a6", overlay: "#6366f1" },
    Palette { key: "rose", title: "Rose", background: "#f472b6", blob: "#c084fc", overlay: "#fda4af" },
    Palette { key: "prism", title: "Prism", background: "#6366f1", blob: "#ec4899", overlay: "#06b6d4" },
    Palette { key: "citrus", title: "Citrus", background: "#fb923c", blob: "#facc15", overlay: "#f472b6" },
    Palette { key: "violet", title: "Violet", background: "#8b5cf6", blob: "#c4b5fd", overlay: "#6366f1" },
];

/// The seed of the shapes and their places.
const SEED: &str = "assistant";
/// 6 seconds at 30 frames a second.
const FRAMES: u32 = 180;
const FPS: u32 = 30;
/// The width of the still Dock icon in pixels.
const ICON: i32 = 1024;
/// The width of the frames of the moving Dock icon in pixels. That is sharp in the Dock at
/// its largest size, and the frames use less memory.
const ICON_FRAME: i32 = 256;
/// The width of the menu bar icon in pixels: 22 points, the height of the menu bar, on a
/// Retina screen.
const MENU_BAR: i32 = 44;
/// The corner radius of the avatar in SVG units: a circle.
const ROUND: f32 = 40.;
/// The corner radius of the Dock icon and the app icon: the rounded square of a macOS app
/// icon, 22.5 % of the width. macOS 26 and later puts an icon of another shape in a gray
/// square.
const ROUNDED_SQUARE: f32 = 18.;
/// The SVG is 80 units wide, and GPUI renders it at two pixels a unit, so 96 pixels. That
/// is sharp at 44 points on a Retina screen.
const SCALE: f32 = 0.6;

pub struct Images {
    /// The first frame, for the avatars next to the messages.
    pub still: Arc<RenderImage>,
    pub moving: Arc<RenderImage>,
}

/// The palette that the user picked, as an index in `PALETTES`.
static SELECTED: AtomicUsize = AtomicUsize::new(0);
/// The images on the screen. After a change of the palette, the old images show until the
/// new images are ready.
static SHOWN: Mutex<Option<Arc<Images>>> = Mutex::new(None);

pub fn selected() -> &'static Palette {
    &PALETTES[SELECTED.load(Ordering::Relaxed)]
}

/// The images, after the first render is done.
pub fn images() -> Option<Arc<Images>> {
    SHOWN.lock().ok()?.clone()
}

/// Use the palette in the file, or the default palette.
pub fn load(file: &Path, cx: &mut App) {
    let key = std::fs::read_to_string(file).unwrap_or_default();
    let index = PALETTES.iter().position(|p| p.key == key.trim()).unwrap_or(0);
    show(index, cx);
}

/// Use the palette, and keep the choice in the file.
pub fn select(index: usize, file: PathBuf, cx: &mut App) {
    if let Err(e) = std::fs::write(&file, PALETTES[index].key) {
        tracing::warn!("cannot save the avatar color: {e}");
    }
    show(index, cx);
}

/// Render the images off the UI thread, then show them in each window.
fn show(index: usize, cx: &mut App) {
    SELECTED.store(index, Ordering::Relaxed);
    let renderer = cx.svg_renderer();
    // The still Dock icon is one frame, so it shows before the moving avatar is ready.
    let still = cx.background_executor().spawn({
        let renderer = renderer.clone();
        async move { render_icon(&renderer, &PALETTES[index], 0., ICON) }
    });
    let frames = cx.background_executor().spawn({
        let renderer = renderer.clone();
        async move {
            // The same time for one loop as the avatar in the window.
            let count = FRAMES / FPS * dock::FPS;
            (0..count)
                .map(|i| render_icon(&renderer, &PALETTES[index], i as f32 / count as f32, ICON_FRAME))
                .collect::<anyhow::Result<Vec<_>>>()
        }
    });
    cx.spawn(async move |cx| {
        let still = match still.await {
            Ok(png) => png,
            Err(e) => return tracing::warn!("cannot render the Dock icon: {e}"),
        };
        if SELECTED.load(Ordering::Relaxed) != index {
            return;
        }
        let _ = cx.update(|_| dock::set_still(&still));
        let frames = match frames.await {
            Ok(pngs) => pngs,
            Err(e) => return tracing::warn!("cannot render the moving Dock icon: {e}"),
        };
        if SELECTED.load(Ordering::Relaxed) == index {
            let _ = cx.update(|_| dock::set_frames(&frames));
        }
    })
    .detach();
    let menu_bar = cx.background_executor().spawn({
        let renderer = renderer.clone();
        async move { render_menu_bar(&renderer, &PALETTES[index]) }
    });
    cx.spawn(async move |_| {
        let rgba = match menu_bar.await {
            Ok(rgba) => rgba,
            Err(e) => return tracing::warn!("cannot render the menu bar icon: {e}"),
        };
        if SELECTED.load(Ordering::Relaxed) == index {
            presence::set_menu_bar_icon(rgba, MENU_BAR as u32);
        }
    })
    .detach();
    let task = cx.background_executor().spawn(async move { render(&renderer, &PALETTES[index]) });
    cx.spawn(async move |cx| {
        let images = match task.await {
            Ok(images) => Arc::new(images),
            Err(e) => return tracing::warn!("cannot render the avatar: {e}"),
        };
        // The user picked another palette while this one rendered.
        if SELECTED.load(Ordering::Relaxed) != index {
            return;
        }
        let old = SHOWN.lock().ok().and_then(|mut shown| shown.replace(images));
        cx.update(|cx| {
            // Free the old frames on the GPU.
            if let Some(old) = old {
                for window in cx.windows() {
                    let _ = window.update(cx, |_, window, _| {
                        let _ = window.drop_image(old.still.clone());
                        let _ = window.drop_image(old.moving.clone());
                    });
                }
            }
            cx.refresh_windows();
        });
    })
    .detach();
}

fn render(renderer: &SvgRenderer, palette: &Palette) -> anyhow::Result<Images> {
    let delay = Delay::from_numer_denom_ms(1000, FPS);
    let mut frames = Vec::with_capacity(FRAMES as usize);
    let mut still = None;
    for i in 0..FRAMES {
        let svg = renderer.parse_svg(svg(palette, i as f32 / FRAMES as f32, ROUND).as_bytes())?;
        let image = renderer.render_parsed(&svg, SCALE)?;
        let size = image.size(0);
        let bytes = image.as_bytes(0).unwrap_or_default().to_vec();
        // The bytes are BGRA, as RenderImage keeps them. RgbaImage is only the container.
        let buffer = RgbaImage::from_raw(size.width.0 as u32, size.height.0 as u32, bytes)
            .ok_or_else(|| anyhow::anyhow!("the frame has the wrong size"))?;
        frames.push(Frame::from_parts(buffer, 0, 0, delay));
        still.get_or_insert(image);
    }
    Ok(Images {
        still: still.ok_or_else(|| anyhow::anyhow!("no frames"))?,
        moving: Arc::new(RenderImage::new(frames)),
    })
}

/// The Dock icon at the time `t` of the loop, as a PNG `width` pixels wide. The avatar has the
/// place, size, and shape of the body of a macOS app icon, with a shadow below it as the other
/// icons in the Dock have. The app icon is the first frame. See `scripts/make_icon.sh`.
fn render_icon(renderer: &SvgRenderer, palette: &Palette, t: f32, width: i32) -> anyhow::Result<Vec<u8>> {
    let avatar = svg(palette, t, ROUNDED_SQUARE).replacen("<svg ", r#"<svg x="100" y="100" width="824" height="824" "#, 1);
    let icon = format!(
        r##"<svg width="1024" height="1024" viewBox="0 0 1024 1024" xmlns="http://www.w3.org/2000/svg">
<defs><filter id="shadow" x="-20%" y="-20%" width="140%" height="140%"><feDropShadow dx="0" dy="14" stdDeviation="18" flood-color="#000000" flood-opacity="0.3"/></filter></defs>
<g filter="url(#shadow)">{avatar}</g>
</svg>"##
    );
    let bytes = rgba(renderer, &icon, width)?;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png).write_image(
        &bytes,
        width as u32,
        width as u32,
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(png)
}

/// The menu bar icon, as RGBA pixels. The avatar is 18 points wide, with a margin, so that it
/// has about the size of the other items in the menu bar.
fn render_menu_bar(renderer: &SvgRenderer, palette: &Palette) -> anyhow::Result<Vec<u8>> {
    let avatar = svg(palette, 0., ROUND).replacen("<svg ", r#"<svg x="4" y="4" width="36" height="36" "#, 1);
    let icon = format!(
        r#"<svg width="44" height="44" viewBox="0 0 44 44" xmlns="http://www.w3.org/2000/svg">{avatar}</svg>"#
    );
    rgba(renderer, &icon, MENU_BAR)
}

/// The SVG as RGBA pixels, in a square `width` pixels wide.
fn rgba(renderer: &SvgRenderer, svg: &str, width: i32) -> anyhow::Result<Vec<u8>> {
    let svg = renderer.parse_svg(svg.as_bytes())?;
    let image = renderer.render_parsed(&svg, SvgSize::ExactSize(size(DevicePixels(width), DevicePixels(width))))?;
    let mut bytes = image.as_bytes(0).unwrap_or_default().to_vec();
    // RenderImage keeps BGRA.
    for pixel in bytes.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Ok(bytes)
}

struct Blob {
    translate_x: f32,
    translate_y: f32,
    scale: f32,
    rotate: f32,
}

/// The hash of boring-avatars: a Java string hash with 32-bit integers.
fn hash(name: &str) -> u64 {
    let mut r: i32 = 0;
    for c in name.encode_utf16() {
        r = r.wrapping_shl(5).wrapping_sub(r).wrapping_add(c as i32);
    }
    r.unsigned_abs() as u64
}

fn digit(n: u64, place: u32) -> u64 {
    n / 10u64.pow(place) % 10
}

fn unit(n: u64, range: u64, place: Option<u32>) -> f32 {
    let value = (n % range) as f32;
    match place {
        Some(p) if digit(n, p) % 2 == 0 => -value,
        _ => value,
    }
}

fn blobs() -> [Blob; 3] {
    let n = hash(SEED);
    std::array::from_fn(|i| {
        let m = n * (i as u64 + 1);
        Blob {
            translate_x: unit(m, 8, Some(1)),
            translate_y: unit(m, 8, Some(2)),
            scale: 1.2 + unit(m, 4, None) / 10.,
            rotate: unit(m, 360, Some(1)),
        }
    })
}

pub fn hex(color: &str) -> u32 {
    u32::from_str_radix(color.trim_start_matches('#'), 16).unwrap_or(0)
}

/// The SVG at the time `t` of the loop, from 0 to 1. At `t` = 0 and `t` = 1 the frames are
/// the same, so the loop has no jump. `corner` is the corner radius, in units of the 80 x 80
/// view box.
fn svg(palette: &Palette, t: f32, corner: f32) -> String {
    let [_, a, b] = blobs();
    let angle = TAU * t;
    // The two blobs turn in opposite directions, one full turn a loop, and drift on circles.
    let (ax, ay, ar) = (a.translate_x + 6. * angle.sin(), a.translate_y + 6. * angle.cos(), a.rotate + 360. * t);
    let (bx, by, br) = (b.translate_x - 6. * (angle + 2.).sin(), b.translate_y + 6. * (angle + 2.).cos(), b.rotate - 360. * t);
    // As in boring-avatars, both blobs use the scale of the second blob.
    let s = b.scale;
    format!(
        r##"<svg viewBox="0 0 80 80" fill="none" xmlns="http://www.w3.org/2000/svg">
<mask id="mask" maskUnits="userSpaceOnUse" x="0" y="0" width="80" height="80"><rect width="80" height="80" rx="{corner}" fill="#FFFFFF"/></mask>
<g mask="url(#mask)">
<rect width="80" height="80" fill="{base}"/>
<path filter="url(#blur)" d="M32.414 59.35L50.376 70.5H72.5v-71H33.728L26.5 13.381l19.057 27.08L32.414 59.35z" fill="{a_color}" transform="translate({ax} {ay}) rotate({ar} 40 40) scale({s})"/>
<path filter="url(#blur)" style="mix-blend-mode:overlay" d="M22.216 24L0 46.75l14.108 38.129L78 86l-3.081-59.276-22.378 4.005 12.972 20.186-23.35 27.395L22.215 24z" fill="{b_color}" transform="translate({bx} {by}) rotate({br} 40 40) scale({s})"/>
</g>
<defs><filter id="blur" filterUnits="userSpaceOnUse" color-interpolation-filters="sRGB"><feFlood flood-opacity="0" result="BackgroundImageFix"/><feBlend in="SourceGraphic" in2="BackgroundImageFix" result="shape"/><feGaussianBlur stdDeviation="7" result="effect1_foregroundBlur"/></filter></defs>
</svg>"##,
        base = palette.background,
        a_color = palette.blob,
        b_color = palette.overlay,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The colors of the "Aurora" palette are the colors that Boring Avatars picks.
    #[test]
    fn aurora_has_the_colors_of_boring_avatars() {
        let colors = ["#22d3ee", "#818cf8", "#e879f9", "#38bdf8", "#c084fc"];
        let n = hash(SEED) as usize;
        let picked: [&str; 3] = std::array::from_fn(|i| colors[(n + i) % colors.len()]);
        let aurora = &PALETTES[0];
        assert_eq!(picked, [aurora.background, aurora.blob, aurora.overlay]);
    }

    #[test]
    fn all_frames_render() {
        let renderer = SvgRenderer::new(Arc::new(gpui_kit_assets::AllAssets));
        let start = std::time::Instant::now();
        let images = render(&renderer, &PALETTES[0]).unwrap();
        eprintln!("{FRAMES} frames in {:?}", start.elapsed());
        assert_eq!(images.moving.frame_count(), FRAMES as usize);
    }

    #[test]
    fn icon_renders() {
        let renderer = SvgRenderer::new(Arc::new(gpui_kit_assets::AllAssets));
        let png = render_icon(&renderer, &PALETTES[0], 0., ICON).unwrap();
        let image = image::load_from_memory(&png).unwrap();
        assert_eq!((image.width(), image.height()), (ICON as u32, ICON as u32));
    }

    #[test]
    fn menu_bar_icon_renders() {
        let renderer = SvgRenderer::new(Arc::new(gpui_kit_assets::AllAssets));
        let rgba = render_menu_bar(&renderer, &PALETTES[0]).unwrap();
        assert_eq!(rgba.len(), (MENU_BAR * MENU_BAR * 4) as usize);
    }

    /// The window of the disk image, in points. `scripts/package.sh` uses the same layout.
    const DMG_WIDTH: u32 = 640;
    const DMG_HEIGHT: u32 = 420;
    /// The moving avatar in the window: its left, top, and width.
    const DMG_AVATAR: (u32, u32, u32) = (250, 50, 140);
    /// The frames a second of the moving avatar. A GIF keeps the delays in hundredths of a
    /// second, so 20 frames a second is exact.
    const DMG_FPS: u32 = 20;

    /// Write the images of a release to the folder in `HELP_IMAGES_DIR`: the app icon
    /// (`AppIcon.png`), and the background of the window of the disk image
    /// (`DmgBackground.gif`). `scripts/make_icon.sh` runs this test.
    #[test]
    #[ignore = "scripts/make_icon.sh runs it"]
    fn write_release_images() {
        let dir = PathBuf::from(std::env::var("HELP_IMAGES_DIR").expect("HELP_IMAGES_DIR is set"));
        let renderer = SvgRenderer::new(Arc::new(gpui_kit_assets::AllAssets));
        let icon = render_icon(&renderer, &PALETTES[0], 0., ICON).unwrap();
        std::fs::write(dir.join("AppIcon.png"), icon).unwrap();
        let gif = render_dmg_background(&renderer, &PALETTES[0]);
        std::fs::write(dir.join("DmgBackground.gif"), gif).unwrap();
    }

    /// A loop of the avatar above an arrow from the app to the Applications folder. The first
    /// frame is the full window. The other frames are only the avatar, so the file is small.
    fn render_dmg_background(renderer: &SvgRenderer, palette: &Palette) -> Vec<u8> {
        let (left, top, width) = DMG_AVATAR;
        let count = FRAMES / FPS * DMG_FPS;
        let mut gif = Vec::new();
        let mut encoder = gif::Encoder::new(&mut gif, DMG_WIDTH as u16, DMG_HEIGHT as u16, &[]).unwrap();
        encoder.set_repeat(gif::Repeat::Infinite).unwrap();
        for i in 0..count {
            let avatar = svg(palette, i as f32 / count as f32, ROUND)
                .replacen("<svg ", &format!(r#"<svg x="{left}" y="{top}" width="{width}" height="{width}" "#), 1);
            let window = format!(
                r##"<svg width="{DMG_WIDTH}" height="{DMG_HEIGHT}" viewBox="0 0 {DMG_WIDTH} {DMG_HEIGHT}" xmlns="http://www.w3.org/2000/svg">
<rect width="{DMG_WIDTH}" height="{DMG_HEIGHT}" fill="#F5F5F7"/>
{avatar}
<path d="M270 290 H366 M348 274 L368 290 L348 306" stroke="#C7C7CC" stroke-width="6" fill="none" stroke-linecap="round" stroke-linejoin="round"/>
</svg>"##
            );
            let svg = renderer.parse_svg(window.as_bytes()).unwrap();
            let size = size(DevicePixels(DMG_WIDTH as i32), DevicePixels(DMG_HEIGHT as i32));
            let image = renderer.render_parsed(&svg, SvgSize::ExactSize(size)).unwrap();
            let mut bytes = image.as_bytes(0).unwrap().to_vec();
            // RenderImage keeps BGRA.
            for pixel in bytes.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            let image = RgbaImage::from_raw(DMG_WIDTH, DMG_HEIGHT, bytes).unwrap();
            let (x, y, w, h) = if i == 0 { (0, 0, DMG_WIDTH, DMG_HEIGHT) } else { (left, top, width, width) };
            let mut part = image::imageops::crop_imm(&image, x, y, w, h).to_image().into_raw();
            let mut frame = gif::Frame::from_rgba_speed(w as u16, h as u16, &mut part, 10);
            (frame.left, frame.top) = (x as u16, y as u16);
            frame.delay = (100 / DMG_FPS) as u16;
            encoder.write_frame(&frame).unwrap();
        }
        drop(encoder);
        gif
    }
}
