//! clipd-pick — the GTK4 + libadwaita command palette.
//!
//! Single-instance application. Launching a second time while the first is
//! still alive simply re-shows / re-focuses the existing window (handled by
//! GApplication's HANDLES_COMMAND_LINE + activate machinery).
//!
//! The hot-path is intentionally tight:
//!   activate → build/show window → focus search entry → query daemon for
//!   recent clips → render. First paint should land under 80 ms on a warm
//!   cache.

mod daemon_client;
mod emoji;
mod search;
mod style;
mod window;

use adw::prelude::*;
use gtk::glib;

const APP_ID: &str = "io.github.devtochukwu.Clipd";

fn main() -> glib::ExitCode {
    init_logging();

    // Ensure libadwaita styling is loaded (replaces gtk::init for Adw apps).
    // Default flags = single-instance + auto-activate on second invocation.
    // We deliberately do NOT use HANDLES_COMMAND_LINE — that flag stalls
    // GTK's automatic processing of the xdg-activation token Mutter passes
    // when our hotkey fires, which on Wayland leaves the new window in a
    // focus-stealing-prevention bounce loop (focus in → focus out repeating).
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .build();

    app.connect_startup(|app| {
        style::install();

        // Keep the GApplication alive even when its last window closes.
        // Required because the clipboard wl_data_source created via GTK's
        // clipboard.set_text() is owned by *this* Wayland connection. If
        // the process exits, the source dies and the clip vanishes before
        // the user gets to Ctrl-V it.
        //
        // NB: `app.hold()` returns a `gio::ApplicationHoldGuard` whose
        // Drop releases the hold. The naive `app.hold();` we had before
        // dropped the guard on the very next line, so the hold lived ~0ns
        // and the process still exited on last-window-close. `mem::forget`
        // leaks the guard for the process lifetime — exactly what we want.
        std::mem::forget(app.hold());
    });

    app.connect_activate(window::show);

    app.run()
}

fn init_logging() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_env("CLIPD_PICK_LOG")
        .unwrap_or_else(|_| EnvFilter::new("clipd_pick=info,warn"));
    fmt().with_env_filter(filter).with_target(false).init();
}
