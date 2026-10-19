//! Hot patching for development. With the `hotpatch` feature, `dx serve --hot-patch`
//! compiles the functions that changed and patches them into the running app. The app
//! does not restart, so it keeps its state.
//!
//! A patch can change function bodies in this crate only. A change to the fields of a
//! struct, or to another crate, needs a full restart.

#[cfg(feature = "hotpatch")]
mod imp {
    use std::sync::Arc;

    /// Connects to the `dx` devserver, and draws the windows again after each patch.
    pub fn connect(cx: &mut gpui::App) {
        dioxus_devtools::connect_subsecond();
        // The patch arrives on the thread of the devserver connection. GPUI can draw only on
        // the main thread, so send a message to it.
        let (tx, rx) = async_channel::unbounded();
        subsecond::register_handler(Arc::new(move || {
            let _ = tx.try_send(());
        }));
        cx.spawn(async move |cx| {
            while rx.recv().await.is_ok() {
                cx.update(|cx| cx.refresh_windows());
            }
        })
        .detach();
    }

    /// Calls `f` through the patch table, so that the newest version of the code runs.
    pub fn call<O>(f: impl FnMut() -> O) -> O {
        subsecond::call(f)
    }
}

#[cfg(not(feature = "hotpatch"))]
mod imp {
    pub fn connect(_cx: &mut gpui::App) {}

    pub fn call<O>(mut f: impl FnMut() -> O) -> O {
        f()
    }
}

pub use imp::*;
