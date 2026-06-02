//! Clipboard watcher — disabled in v0.1.
//!
//! Why: GNOME's Mutter doesn't expose `wlr_data_control_v1` or
//! `ext_data_control_v1`. The standard `wl_data_device_manager` is
//! gated on the requesting client owning input focus, so a background
//! daemon physically cannot observe clipboard changes here. Polling
//! `wl-paste` works on wlroots but on GNOME it spawns short-lived
//! anonymous Wayland clients that Mutter's app-tracking (and Ubuntu
//! Dock's "Unknown" tile renderer) react to on every poll — visible
//! distortion in user input.
//!
//! New v0.1 architecture: the daemon does NOT touch the clipboard.
//! The picker, which has focus when it opens, reads the current clip
//! in-process via wl-clipboard-rs and sends it to the daemon via the
//! existing Ingest IPC. One clipboard read per hotkey press, performed
//! by a properly-identified Wayland client — no flicker, no distortion.
//!
//! Trade-off: clips you copy but never open the picker for are not
//! captured. v0.2 plan: ship a tiny GNOME Shell extension that taps
//! St.Clipboard (privileged inside Shell) and pushes changes to the
//! daemon over D-Bus, restoring continuous capture without any
//! flicker.
//!
//! This module is kept (as a no-op stub) so `main.rs` doesn't need to
//! change its call site; when v0.2 lands it can be repurposed for the
//! Shell-extension D-Bus listener.

use std::sync::Arc;

use anyhow::Result;

use crate::store::Store;

pub struct Watcher;

impl Watcher {
    pub fn spawn(_store: Arc<Store>) -> Result<Self> {
        tracing::info!(
            "clipboard capture: passive mode \
             (picker triggers ingestion on each open). \
             GNOME/Mutter does not expose ext-data-control-v1, so \
             background polling is intentionally disabled."
        );
        Ok(Self)
    }
}
