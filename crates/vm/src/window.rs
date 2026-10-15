//! A native window that shows the agent Mac screen.

use std::cell::RefCell;

use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSBackingStoreType, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use objc2_virtualization::VZVirtualMachineView;

use crate::VM;
use crate::config::{DISPLAY_HEIGHT, DISPLAY_WIDTH};

thread_local! {
    static WINDOW: RefCell<Option<Retained<NSWindow>>> = const { RefCell::new(None) };
}

/// Show the VM screen in its own window. Call this on the main thread.
pub fn show_window() -> Result<()> {
    let mtm = MainThreadMarker::new().context("show_window must run on the main thread")?;
    let vm = VM
        .with(|slot| slot.borrow().clone())
        .context("the agent Mac is not running")?;
    WINDOW.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(window) = slot.as_ref() {
            window.makeKeyAndOrderFront(None);
            return Ok(());
        }
        let rect = NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(DISPLAY_WIDTH as f64 * 0.75, DISPLAY_HEIGHT as f64 * 0.75),
        );
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect,
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Resizable
                    | NSWindowStyleMask::Miniaturizable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe { window.setReleasedWhenClosed(false) };
        let view = unsafe { VZVirtualMachineView::initWithFrame(VZVirtualMachineView::alloc(mtm), rect) };
        unsafe {
            view.setVirtualMachine(Some(&vm));
            view.setCapturesSystemKeys(true);
        }
        window.setContentView(Some(&view));
        window.makeFirstResponder(Some(&view));
        window.setTitle(&NSString::from_str("Agent Mac"));
        window.center();
        window.makeKeyAndOrderFront(None);
        *slot = Some(window);
        Ok(())
    })
}
