//! Mouse and keyboard input through CoreGraphics events. The process needs the
//! Accessibility permission in the guest.

use std::thread::sleep;
use std::time::Duration;

use anyhow::{Result, bail};
use objc2_core_foundation::CGPoint;
use objc2_core_graphics::{
    CGDisplayBounds, CGEvent, CGEventField, CGEventFlags, CGEventTapLocation, CGEventType,
    CGMainDisplayID, CGMouseButton, CGScrollEventUnit,
};
use protocol::MouseButton;

pub fn display_size_points() -> (u32, u32) {
    let bounds = CGDisplayBounds(CGMainDisplayID());
    (bounds.size.width as u32, bounds.size.height as u32)
}

fn post(event: Option<objc2_core_foundation::CFRetained<CGEvent>>) {
    CGEvent::post(CGEventTapLocation::HIDEventTap, event.as_deref());
}

pub fn mouse_move(x: f64, y: f64) {
    post(CGEvent::new_mouse_event(
        None,
        CGEventType::MouseMoved,
        CGPoint { x, y },
        CGMouseButton::Left,
    ));
}

pub fn click(x: f64, y: f64, button: MouseButton, count: u32) {
    let (down, up, cg_button) = match button {
        MouseButton::Left => (CGEventType::LeftMouseDown, CGEventType::LeftMouseUp, CGMouseButton::Left),
        MouseButton::Right => (CGEventType::RightMouseDown, CGEventType::RightMouseUp, CGMouseButton::Right),
    };
    let point = CGPoint { x, y };
    mouse_move(x, y);
    sleep(Duration::from_millis(30));
    for n in 1..=count.max(1) {
        for kind in [down, up] {
            let event = CGEvent::new_mouse_event(None, kind, point, cg_button);
            CGEvent::set_integer_value_field(event.as_deref(), CGEventField::MouseEventClickState, n as i64);
            post(event);
        }
        sleep(Duration::from_millis(40));
    }
}

pub fn drag(from_x: f64, from_y: f64, to_x: f64, to_y: f64) {
    let from = CGPoint { x: from_x, y: from_y };
    mouse_move(from_x, from_y);
    post(CGEvent::new_mouse_event(None, CGEventType::LeftMouseDown, from, CGMouseButton::Left));
    let steps = 20;
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        let p = CGPoint {
            x: from_x + (to_x - from_x) * t,
            y: from_y + (to_y - from_y) * t,
        };
        post(CGEvent::new_mouse_event(None, CGEventType::LeftMouseDragged, p, CGMouseButton::Left));
        sleep(Duration::from_millis(10));
    }
    let to = CGPoint { x: to_x, y: to_y };
    post(CGEvent::new_mouse_event(None, CGEventType::LeftMouseUp, to, CGMouseButton::Left));
}

pub fn scroll(x: f64, y: f64, dx: i32, dy: i32) {
    mouse_move(x, y);
    post(CGEvent::new_scroll_wheel_event2(None, CGScrollEventUnit::Line, 2, -dy, -dx, 0));
}

pub fn type_text(text: &str) {
    // Send the text in small chunks. Long unicode strings in one event are cut.
    let units: Vec<u16> = text.encode_utf16().collect();
    for chunk in units.chunks(16) {
        for down in [true, false] {
            let event = CGEvent::new_keyboard_event(None, 0, down);
            unsafe {
                CGEvent::keyboard_set_unicode_string(event.as_deref(), chunk.len() as _, chunk.as_ptr());
            }
            post(event);
        }
        sleep(Duration::from_millis(8));
    }
}

pub fn key_combo(combo: &str) -> Result<()> {
    let mut flags = CGEventFlags::empty();
    let mut key = None;
    for part in combo.split('+').map(|p| p.trim().to_lowercase()) {
        match part.as_str() {
            "cmd" | "command" | "super" | "meta" => flags |= CGEventFlags::MaskCommand,
            "shift" => flags |= CGEventFlags::MaskShift,
            "ctrl" | "control" => flags |= CGEventFlags::MaskControl,
            "alt" | "option" | "opt" => flags |= CGEventFlags::MaskAlternate,
            "fn" => flags |= CGEventFlags::MaskSecondaryFn,
            other => key = Some(keycode(other)?),
        }
    }
    let Some(code) = key else { bail!("no key in combo '{combo}'") };
    for down in [true, false] {
        let event = CGEvent::new_keyboard_event(None, code, down);
        CGEvent::set_flags(event.as_deref(), flags);
        post(event);
    }
    Ok(())
}

/// macOS virtual key codes for a US keyboard layout.
fn keycode(name: &str) -> Result<u16> {
    const LETTERS: &str = "asdfhgzxcv?bqweryt123465=97-80]ou[ip?lj'k;\\,/nm.";
    Ok(match name {
        "return" | "enter" => 36,
        "tab" => 48,
        "space" => 49,
        "delete" | "backspace" => 51,
        "escape" | "esc" => 53,
        "forwarddelete" => 117,
        "home" => 115,
        "end" => 119,
        "pageup" => 116,
        "pagedown" => 121,
        "left" => 123,
        "right" => 124,
        "down" => 125,
        "up" => 126,
        "`" => 50,
        "f1" => 122,
        "f2" => 120,
        "f3" => 99,
        "f4" => 118,
        "f5" => 96,
        "f6" => 97,
        "f7" => 98,
        "f8" => 100,
        "f9" => 101,
        "f10" => 109,
        "f11" => 103,
        "f12" => 111,
        s if s.chars().count() == 1 => {
            let c = s.chars().next().unwrap();
            match LETTERS.chars().position(|l| l == c && l != '?') {
                Some(i) => i as u16,
                None => bail!("unknown key '{name}'"),
            }
        }
        _ => bail!("unknown key '{name}'"),
    })
}
