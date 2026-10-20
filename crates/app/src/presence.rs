//! The assistant stays one keystroke away. It has an item in the menu bar, and a global
//! hotkey (Option-Space) shows or hides the window from any app. A close of the window hides
//! the app, so that the agent keeps its session. Only Quit stops the app.

use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};
use gpui::{App, Entity, Global, WindowHandle};
use gpui_component::Root;
use tray_icon::{
    Icon, TrayIconBuilder,
    menu::{
        Menu, MenuEvent, MenuItem, PredefinedMenuItem,
        accelerator::{Accelerator, Code as MenuCode, Modifiers as MenuModifiers},
    },
};

use crate::ui::AppView;

gpui::actions!(presence, [HideWindow, Quit]);

/// The window that the menu bar item and the hotkey show.
struct Presence {
    window: WindowHandle<Root>,
    view: Entity<AppView>,
}

impl Global for Presence {}

enum Signal {
    Toggle,
    Show,
    CheckForUpdates,
    Quit,
}

/// Add the menu bar item and the hotkey. A close of `window` hides the app from now on.
pub fn install(window: WindowHandle<Root>, view: Entity<AppView>, cx: &mut App) {
    let _ = window.update(cx, |_, window, cx| {
        window.on_window_should_close(cx, |_, cx| {
            cx.hide();
            false
        });
    });
    cx.on_action(|_: &HideWindow, cx| cx.hide());
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.set_global(Presence { window, view });

    let (tx, rx) = async_channel::unbounded();

    let open = MenuItem::new(
        "Open Help",
        true,
        Some(Accelerator::new(MenuModifiers::ALT, MenuCode::Space)),
    );
    let version = MenuItem::new(concat!("Help ", env!("CARGO_PKG_VERSION")), false, None);
    let update = MenuItem::new("Check for Updates…", true, None);
    let quit = MenuItem::new("Quit Help", true, None);
    let menu = Menu::new();
    let _ = menu.append_items(&[
        &open,
        &PredefinedMenuItem::separator(),
        &version,
        &update,
        &PredefinedMenuItem::separator(),
        &quit,
    ]);
    let (open_id, update_id, quit_id) = (open.id().clone(), update.id().clone(), quit.id().clone());
    let menu_tx = tx.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        let signal = if e.id == open_id {
            Signal::Show
        } else if e.id == update_id {
            Signal::CheckForUpdates
        } else if e.id == quit_id {
            Signal::Quit
        } else {
            return;
        };
        let _ = menu_tx.try_send(signal);
    }));
    let tray = TrayIconBuilder::new()
        .with_icon_templated(menu_bar_icon())
        .with_tooltip("Help")
        .with_menu(Box::new(menu))
        .build()
        .inspect_err(|e| tracing::warn!("cannot add the menu bar item: {e}"))
        .ok();

    let hotkey = HotKey::new(Some(Modifiers::ALT), Code::Space);
    let hotkeys = GlobalHotKeyManager::new()
        .inspect_err(|e| tracing::warn!("cannot use global hotkeys: {e}"))
        .ok();
    if let Some(manager) = &hotkeys
        && let Err(e) = manager.register(hotkey)
    {
        tracing::warn!(
            "cannot use Option-Space as the hotkey. An other app uses it, possibly: {e}"
        );
    }
    GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
        if e.id == hotkey.id() && e.state == HotKeyState::Pressed {
            let _ = tx.try_send(Signal::Toggle);
        }
    }));

    cx.spawn(async move |cx| {
        // The menu bar item and the hotkey work while these values exist.
        let _keep = (tray, hotkeys);
        while let Ok(signal) = rx.recv().await {
            cx.update(|cx| match signal {
                Signal::Toggle => toggle(cx),
                Signal::Show => show(cx),
                Signal::CheckForUpdates => crate::updater::check(true, cx),
                Signal::Quit => cx.quit(),
            });
        }
    })
    .detach();
}

/// Bring the window to the front, with the keyboard focus in the composer.
pub fn show(cx: &mut App) {
    cx.activate(true);
    with_view(cx, |view, window, cx| {
        window.activate_window();
        view.update(cx, |v, cx| v.focus_composer(window, cx));
    });
}

/// Show the window, or hide the app if the window is in front.
fn toggle(cx: &mut App) {
    let Some(window) = cx.try_global::<Presence>().map(|p| p.window) else {
        return;
    };
    let active = window
        .update(cx, |_, window, _| window.is_window_active())
        .unwrap_or(false);
    if active { cx.hide() } else { show(cx) }
}

fn with_view(cx: &mut App, f: impl FnOnce(&Entity<AppView>, &mut gpui::Window, &mut App)) {
    let Some(p) = cx.try_global::<Presence>() else {
        return;
    };
    let (window, view) = (p.window, p.view.clone());
    let _ = window.update(cx, |_, window, cx| f(&view, window, cx));
}

/// A chat bubble with three dots, as a template image: black with alpha. macOS gives it the
/// color of the menu bar.
fn menu_bar_icon() -> Icon {
    const N: usize = 36;
    const SAMPLES: usize = 4;
    let inside = |x: f32, y: f32| {
        // The bubble: a rounded rectangle.
        let (l, t, r, b, rad) = (3.0, 5.0, 33.0, 26.0, 8.0);
        let cx = x.clamp(l + rad, r - rad);
        let cy = y.clamp(t + rad, b - rad);
        let in_bubble = x >= l
            && x <= r
            && y >= t
            && y <= b
            && (x - cx).powi(2) + (y - cy).powi(2) <= rad * rad;
        // The tail at the bottom left: a triangle.
        let in_tail = y >= 24.0 && y <= 32.0 && x >= 8.0 && x <= 8.0 + (32.0 - y) * 1.2;
        // The dots are holes in the bubble.
        let in_dot = [11.0, 18.0, 25.0]
            .iter()
            .any(|dx| (x - dx).powi(2) + (y - 15.5f32).powi(2) <= 2.4 * 2.4);
        (in_bubble || in_tail) && !in_dot
    };
    let mut rgba = Vec::with_capacity(N * N * 4);
    for py in 0..N {
        for px in 0..N {
            let mut hits = 0;
            for sy in 0..SAMPLES {
                for sx in 0..SAMPLES {
                    let x = px as f32 + (sx as f32 + 0.5) / SAMPLES as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / SAMPLES as f32;
                    hits += inside(x, y) as u32;
                }
            }
            let alpha = (hits * 255 / (SAMPLES * SAMPLES) as u32) as u8;
            rgba.extend_from_slice(&[0, 0, 0, alpha]);
        }
    }
    Icon::from_rgba(rgba, N as u32, N as u32).expect("the icon has the correct size")
}
