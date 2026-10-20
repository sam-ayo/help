//! The Dock icon: the avatar in the colors that the user picked. It moves while the agent
//! works, so that the user sees the work from other apps. At other times, and when "Reduce
//! motion" is on in the macOS settings, it is still. See `avatar` for the images.
//!
//! The images are `NSImage`s, which only the main thread can use. All functions here run on
//! the main thread.

use std::{cell::RefCell, time::Duration};

use gpui::App;
use objc2::{AllocAnyThread, MainThreadMarker, rc::Retained};
use objc2_app_kit::{NSApplication, NSImage, NSWorkspace};
use objc2_foundation::NSData;

/// The frames a second of the loop. The Dock draws each new icon, so the loop uses fewer
/// frames than the avatar in the window.
pub const FPS: u32 = 15;
const FRAME: Duration = Duration::from_millis(1000 / FPS as u64);

#[derive(Default)]
struct State {
    /// The icon when the agent does not work.
    still: Option<Retained<NSImage>>,
    /// The frames of the loop.
    frames: Vec<Retained<NSImage>>,
    moving: bool,
    /// The loop task runs. It stops after it shows the still icon.
    running: bool,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

/// Use a new still icon, as a PNG.
pub fn set_still(png: &[u8]) {
    let Some(image) = image(png) else { return };
    STATE.with_borrow_mut(|s| {
        if !s.running {
            show(&image);
        }
        s.still = Some(image);
    });
}

/// Use new frames for the loop, as PNGs.
pub fn set_frames(pngs: &[Vec<u8>]) {
    let frames = pngs.iter().filter_map(|png| image(png)).collect();
    STATE.with_borrow_mut(|s| s.frames = frames);
}

/// Start or stop the loop. It is safe to call this on each render.
pub fn set_moving(moving: bool, cx: &mut App) {
    let start = STATE.with_borrow_mut(|s| {
        s.moving = moving;
        let start = moving && !s.running;
        s.running |= start;
        start
    });
    if !start {
        return;
    }
    cx.spawn(async move |cx| {
        let mut i = 0;
        loop {
            let done = STATE.with_borrow_mut(|s| {
                let frame = (s.moving && !reduce_motion()).then(|| s.frames.get(i % s.frames.len().max(1))).flatten();
                if let Some(image) = frame.or(s.still.as_ref()) {
                    show(image);
                }
                // Stop after the still icon shows. Until the frames are ready, wait.
                s.running = s.moving;
                !s.running
            });
            if done {
                break;
            }
            i += 1;
            cx.background_executor().timer(FRAME).await;
        }
    })
    .detach();
}

fn image(png: &[u8]) -> Option<Retained<NSImage>> {
    let image = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(png));
    if image.is_none() {
        tracing::warn!("cannot read the Dock icon");
    }
    image
}

fn show(image: &NSImage) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(image)) };
}

fn reduce_motion() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}
