//! The avatar of the assistant: a "marble" avatar from Boring Avatars
//! (https://boringavatars.com, MIT), with the seed "assistant". A port of the marble variant
//! of boring-avatars 2.0.4.
//!
//! The moving avatar is a loop of frames. The two blobs of color turn and drift, so the
//! gradient moves. GPUI plays the frames as it plays a GIF. It stops when the window is not
//! active, and when "Reduce motion" is on in the macOS settings.
//!
//! The Dock icon is the avatar too, in the same colors. See `dock`.

use std::{
    f32::consts::TAU,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use super::dock;
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
        let svg = renderer.parse_svg(svg(palette, i as f32 / FRAMES as f32).as_bytes())?;
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
/// place and size of the body of a macOS app icon, with a shadow below it as the other icons
/// in the Dock have.
fn render_icon(renderer: &SvgRenderer, palette: &Palette, t: f32, width: i32) -> anyhow::Result<Vec<u8>> {
    let avatar = svg(palette, t).replacen("<svg ", r#"<svg x="100" y="100" width="824" height="824" "#, 1);
    let icon = format!(
        r##"<svg width="1024" height="1024" viewBox="0 0 1024 1024" xmlns="http://www.w3.org/2000/svg">
<defs><filter id="shadow" x="-20%" y="-20%" width="140%" height="140%"><feDropShadow dx="0" dy="14" stdDeviation="18" flood-color="#000000" flood-opacity="0.3"/></filter></defs>
<g filter="url(#shadow)">{avatar}</g>
</svg>"##
    );
    let svg = renderer.parse_svg(icon.as_bytes())?;
    let image = renderer.render_parsed(&svg, SvgSize::ExactSize(size(DevicePixels(width), DevicePixels(width))))?;
    let size = image.size(0);
    let mut bytes = image.as_bytes(0).unwrap_or_default().to_vec();
    // RenderImage keeps BGRA. The PNG needs RGBA.
    for pixel in bytes.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png).write_image(
        &bytes,
        size.width.0 as u32,
        size.height.0 as u32,
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(png)
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
/// the same, so the loop has no jump.
fn svg(palette: &Palette, t: f32) -> String {
    let [_, a, b] = blobs();
    let angle = TAU * t;
    // The two blobs turn in opposite directions, one full turn a loop, and drift on circles.
    let (ax, ay, ar) = (a.translate_x + 6. * angle.sin(), a.translate_y + 6. * angle.cos(), a.rotate + 360. * t);
    let (bx, by, br) = (b.translate_x - 6. * (angle + 2.).sin(), b.translate_y + 6. * (angle + 2.).cos(), b.rotate - 360. * t);
    // As in boring-avatars, both blobs use the scale of the second blob.
    let s = b.scale;
    format!(
        r##"<svg viewBox="0 0 80 80" fill="none" xmlns="http://www.w3.org/2000/svg">
<mask id="mask" maskUnits="userSpaceOnUse" x="0" y="0" width="80" height="80"><rect width="80" height="80" rx="160" fill="#FFFFFF"/></mask>
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
}
