//! The assistant stays one keystroke away. It has an item in the menu bar, and a global
//! hotkey (Option-Space) shows or hides the window from any app. A close of the window hides
//! the app, so that the agent keeps its session. Only Quit stops the app.

use std::cell::RefCell;

use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};
use gpui::{App, Entity, Global, WindowHandle};
use gpui_component::Root;
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
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

/// The menu bar item, and the newest icon for it. The icon can come before the item exists.
#[derive(Default)]
struct MenuBar {
    tray: Option<TrayIcon>,
    icon: Option<Icon>,
}

thread_local! {
    static MENU_BAR: RefCell<MenuBar> = RefCell::default();
}

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
    MENU_BAR.with_borrow_mut(|m| {
        let mut builder = TrayIconBuilder::new().with_tooltip("Help").with_menu(Box::new(menu));
        if let Some(icon) = m.icon.take() {
            builder = builder.with_icon(icon);
        }
        m.tray = builder
            .build()
            .inspect_err(|e| tracing::warn!("cannot add the menu bar item: {e}"))
            .ok();
    });

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
        // The hotkey works while this value exists.
        let _keep = hotkeys;
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

/// Show the avatar in the menu bar. `rgba` is a square image `width` pixels wide.
pub fn set_menu_bar_icon(rgba: Vec<u8>, width: u32) {
    let icon = match Icon::from_rgba(rgba, width, width) {
        Ok(icon) => icon,
        Err(e) => return tracing::warn!("cannot use the avatar in the menu bar: {e}"),
    };
    MENU_BAR.with_borrow_mut(|m| match &m.tray {
        Some(tray) => {
            if let Err(e) = tray.set_icon(Some(icon)) {
                tracing::warn!("cannot use the avatar in the menu bar: {e}");
            }
        }
        None => m.icon = Some(icon),
    });
}
