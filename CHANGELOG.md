# Changelog

All notable changes to clipd are documented here. Format roughly follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.2.0] — 2026-06-02

### Added
- Tabbed UI: separate Clipboard and Emoji views with a segmented switcher.
- Unicode 16 emoji catalog (~3,800 entries) via the `emojis` crate, with fuzzy
  ranking over names, aliases and shortcodes.
- Pin / unpin clips. Pinned clips survive the 24-hour TTL cleanup.
- Image clip support: PNG capture, thumbnail rendering in the picker,
  round-trip paste through `wl-copy --type image/png`.
- Auto-paste after activation via `ydotool` — the clip lands on the system
  clipboard *and* is pasted into the previously focused app.
- Super paste (`Shift+Enter` or context-menu item) — sends `Ctrl+Shift+V`
  instead of `Ctrl+V`, which is what terminals expect and what most browsers
  treat as "paste as plain text".
- Right-click context menu: Pin / Unpin, Paste, Super paste, Delete.
- `Ctrl+D` keyboard shortcut to delete the selected clip.
- GNOME Shell extension (`clipd@devtochukwu`) for continuous background
  capture. Hooks `Meta.SelectionType.SELECTION_CLIPBOARD` and forwards every
  change to the daemon via `clipd ingest`.
- Daemon startup TTL cleanup: unpinned clips older than 24 h are pruned.

### Fixed
- `Ctrl+P` now refreshes with the user's current search query instead of
  dropping it.
- Window-level `Esc` closes the picker regardless of the focused widget
  (capture-phase handler).
- Click-outside-to-close, with a 500 ms grace period so the Mutter
  activation handshake doesn't slam the window shut on open.
- Right-click context menu no longer triggers the click-outside handler
  (suppression flag while the popover is up).

## [0.1.0] — 2026-05-31

Initial release. Passive daemon + picker-triggered clipboard snapshot,
Mutter-compatible (no dependency on `wlr_data_control_v1` /
`ext_data_control_v1`, which Mutter does not expose).
