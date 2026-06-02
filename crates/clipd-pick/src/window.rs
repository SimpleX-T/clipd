//! The palette window — tabbed (Clipboard / Emojis), single search,
//! pin toggle, image clip support.
//!
//! Lifecycle: GApplication is held alive (see main.rs), so this window
//! is created/destroyed per activation while the process stays warm.
//! The on-disk Wayland clipboard offer is owned by detached `wl-copy`
//! grandchildren, not by us — see the long-form note in `activate_row`.

use std::cell::{Cell, RefCell};
use std::io::Write;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{SystemTime, UNIX_EPOCH};

use adw::prelude::*;
use clipd_proto::{Clip, ClipKind};
use gtk::gdk;
use gtk::glib::{self, clone};

use crate::daemon_client::Client;
use crate::search::Result_;

const MAX_RESULTS: u32 = 80;
const QUERY_DEBOUNCE_MS: u64 = 60;
const AUTO_PASTE_DELAY_MS: u64 = 150;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    Clipboard,
    Emoji,
}

/// How to fire the post-activation paste keystroke.
///
/// Mutter doesn't tell clients which app is focused, so the picker can't
/// auto-detect terminals. The user picks via Shift+Enter or the context
/// menu; everything else defaults to Normal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PasteMode {
    /// Ctrl+V — works in essentially everything except terminals.
    Normal,
    /// Ctrl+Shift+V — what terminals (gnome-terminal, kitty, foot, …)
    /// require, and what most browsers treat as "paste as plain text".
    Super,
}

pub fn show(app: &adw::Application) {
    if let Some(existing) = app
        .windows()
        .into_iter()
        .find(|w| w.is::<adw::ApplicationWindow>())
    {
        existing.present();
        return;
    }

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("clipd")
        .default_width(720)
        .default_height(460)
        .resizable(false)
        .build();
    window.add_css_class("clipd-pick");

    let palette = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(vec!["palette".to_string()])
        .build();

    // --- search row ----------------------------------------------------
    let search_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(0)
        .css_classes(vec!["search-row".to_string()])
        .build();
    let search_icon = gtk::Image::from_icon_name("system-search-symbolic");
    search_icon.add_css_class("search-icon");
    let entry = gtk::Entry::builder()
        .placeholder_text("Search…")
        .hexpand(true)
        .css_classes(vec!["search".to_string()])
        .build();
    entry.set_input_hints(gtk::InputHints::NO_EMOJI | gtk::InputHints::NO_SPELLCHECK);
    search_row.append(&search_icon);
    search_row.append(&entry);

    // --- tab bar -------------------------------------------------------
    let tab_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(0)
        .halign(gtk::Align::Center)
        .css_classes(vec!["tab-row".to_string(), "linked".to_string()])
        .build();
    let clip_tab = gtk::ToggleButton::builder()
        .label("Clipboard")
        .active(true)
        .css_classes(vec!["tab".to_string()])
        .build();
    let emoji_tab = gtk::ToggleButton::builder()
        .label("Emojis")
        .css_classes(vec!["tab".to_string()])
        .build();
    emoji_tab.set_group(Some(&clip_tab));
    // Tabs MUST NOT take keyboard focus. They're clickable for switching
    // but must never become the focus target — otherwise typing after a
    // tab click would land nowhere (toggle buttons don't consume text).
    clip_tab.set_can_focus(false);
    emoji_tab.set_can_focus(false);
    tab_row.append(&clip_tab);
    tab_row.append(&emoji_tab);

    // --- results list --------------------------------------------------
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .build();
    let listbox = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Browse)
        .css_classes(vec!["results".to_string()])
        .build();
    // Same reason as the tabs — keyboard focus stays on the search entry
    // so typing always lands there. Row activation goes through clicks
    // (row_activated) and Enter-in-entry (connect_activate); we never
    // need a row to "own" focus.
    listbox.set_can_focus(false);
    scroller.set_child(Some(&listbox));

    palette.append(&search_row);
    palette.append(&tab_row);
    palette.append(&scroller);

    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .css_classes(vec!["clipd-header".to_string()])
        .build();
    header.set_title_widget(Some(&gtk::Label::new(None)));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&palette));
    window.set_content(Some(&toolbar));

    // Shared state.
    let client = match Client::new() {
        Ok(c) => Rc::new(c),
        Err(e) => {
            tracing::error!("daemon client: {e:#}");
            return;
        }
    };
    let current_results: Rc<RefCell<Vec<Result_>>> = Rc::new(RefCell::new(Vec::new()));
    let active_tab: Rc<Cell<Tab>> = Rc::new(Cell::new(Tab::Clipboard));
    let latest_token: Rc<Cell<u32>> = Rc::new(Cell::new(0));
    // True while the right-click context popover is up. Suppresses the
    // window-level click-outside-to-close handler — the popover takes a
    // Wayland keyboard grab when it pops up, which flips the window's
    // `is_active` to false, and without this flag the whole palette
    // would close (taking the popover with it).
    let context_menu_open: Rc<Cell<bool>> = Rc::new(Cell::new(false));

    refresh(&client, &listbox, &current_results, &active_tab, "");

    // Defer the clipboard snapshot until after map (focus has settled).
    let snap_client = client.clone();
    let snap_listbox = listbox.downgrade();
    let snap_results = current_results.clone();
    let snap_entry = entry.downgrade();
    let snap_tab = active_tab.clone();
    window.connect_map(move |_w| {
        let c = snap_client.clone();
        let lb = snap_listbox.clone();
        let cr = snap_results.clone();
        let en = snap_entry.clone();
        let tab = snap_tab.clone();
        glib::idle_add_local_once(move || {
            snapshot_clipboard(&c, &lb, &cr, &en, &tab);
        });
    });

    // Search → debounced refresh.
    entry.connect_changed(clone!(
        #[weak] listbox,
        #[strong] client,
        #[strong] current_results,
        #[strong] active_tab,
        #[strong] latest_token,
        move |entry| {
            let q = entry.text().to_string();
            let token = latest_token.get().wrapping_add(1);
            latest_token.set(token);
            glib::timeout_add_local_once(
                std::time::Duration::from_millis(QUERY_DEBOUNCE_MS),
                clone!(
                    #[weak] listbox,
                    #[strong] client,
                    #[strong] current_results,
                    #[strong] active_tab,
                    #[strong] latest_token,
                    move || {
                        if latest_token.get() != token {
                            return;
                        }
                        refresh(&client, &listbox, &current_results, &active_tab, &q);
                    }
                ),
            );
        }
    ));

    // Tab buttons → switch view + re-refresh with current query.
    clip_tab.connect_toggled(clone!(
        #[weak] listbox,
        #[weak] entry,
        #[strong] client,
        #[strong] current_results,
        #[strong] active_tab,
        move |btn| {
            if btn.is_active() {
                active_tab.set(Tab::Clipboard);
                let q = entry.text().to_string();
                refresh(&client, &listbox, &current_results, &active_tab, &q);
            }
        }
    ));
    emoji_tab.connect_toggled(clone!(
        #[weak] listbox,
        #[weak] entry,
        #[strong] client,
        #[strong] current_results,
        #[strong] active_tab,
        move |btn| {
            if btn.is_active() {
                active_tab.set(Tab::Emoji);
                let q = entry.text().to_string();
                refresh(&client, &listbox, &current_results, &active_tab, &q);
            }
        }
    ));

    entry.connect_activate(clone!(
        #[weak] listbox,
        #[weak] window,
        #[strong] client,
        #[strong] current_results,
        move |_| {
            if let Some(row) = listbox.selected_row() {
                activate_row(
                    &client,
                    &current_results,
                    row.index(),
                    &window,
                    PasteMode::Normal,
                );
            }
        }
    ));

    // Key handling on the entry:
    //   Up/Down       → move list selection
    //   Esc           → close (also handled at window level for safety)
    //   Enter         → activate (Ctrl+V paste)
    //   Shift+Enter   → activate as super paste (Ctrl+Shift+V — terminals, "paste as plain text")
    //   Ctrl+P        → toggle pin on the selected clip
    //   Ctrl+D        → delete the selected clip
    //   Ctrl+1 / Ctrl+2 → switch tabs (Tab alone moves focus, deliberate)
    let key_controller = gtk::EventControllerKey::new();
    key_controller.connect_key_pressed(clone!(
        #[weak] listbox,
        #[weak] window,
        #[weak] entry,
        #[weak] clip_tab,
        #[weak] emoji_tab,
        #[strong] client,
        #[strong] current_results,
        #[strong] active_tab,
        #[upgrade_or] glib::Propagation::Proceed,
        move |_ctl, key, _code, mods| {
            let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
            let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
            match key {
                gdk::Key::Down => {
                    move_selection(&listbox, 1);
                    glib::Propagation::Stop
                }
                gdk::Key::Up => {
                    move_selection(&listbox, -1);
                    glib::Propagation::Stop
                }
                gdk::Key::Return | gdk::Key::KP_Enter if shift => {
                    // Shift+Enter — super paste. We intercept here so the
                    // entry's connect_activate (plain Enter) doesn't also
                    // fire and paste twice.
                    if let Some(row) = listbox.selected_row() {
                        activate_row(
                            &client,
                            &current_results,
                            row.index(),
                            &window,
                            PasteMode::Super,
                        );
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::p if ctrl => {
                    if let Some(row) = listbox.selected_row() {
                        toggle_pin(&client, &current_results, row.index());
                        let q = entry.text().to_string();
                        refresh(&client, &listbox, &current_results, &active_tab, &q);
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::d if ctrl => {
                    if let Some(row) = listbox.selected_row() {
                        let idx = row.index();
                        if delete_row(&client, &current_results, idx) {
                            let q = entry.text().to_string();
                            refresh(&client, &listbox, &current_results, &active_tab, &q);
                            // Reselect at the same visual position so the
                            // user can hold Ctrl+D to mow down clips.
                            let count = count_rows(&listbox);
                            if count > 0 {
                                let new_idx = idx.min(count - 1);
                                if let Some(r) = listbox.row_at_index(new_idx) {
                                    listbox.select_row(Some(&r));
                                }
                            }
                        }
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::_1 if ctrl => {
                    clip_tab.set_active(true);
                    glib::Propagation::Stop
                }
                gdk::Key::_2 if ctrl => {
                    emoji_tab.set_active(true);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        }
    ));
    entry.add_controller(key_controller);

    listbox.connect_row_activated(clone!(
        #[weak] window,
        #[strong] client,
        #[strong] current_results,
        move |_lb, row| {
            activate_row(
                &client,
                &current_results,
                row.index(),
                &window,
                PasteMode::Normal,
            );
        }
    ));

    // Right-click on any row → context menu (Pin/Unpin, Paste, Super
    // paste, Delete). One gesture on the listbox; we walk up from
    // pick(x, y) to find the row, instead of attaching a gesture per row
    // — saves churn on every refresh().
    let right_click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .build();
    right_click.connect_pressed(clone!(
        #[weak] listbox,
        #[weak] window,
        #[weak] entry,
        #[strong] client,
        #[strong] current_results,
        #[strong] active_tab,
        #[strong] context_menu_open,
        move |gesture, _n, x, y| {
            let Some(picked) = listbox.pick(x, y, gtk::PickFlags::DEFAULT) else {
                return;
            };
            let mut cursor: Option<gtk::Widget> = Some(picked);
            while let Some(w) = cursor {
                if let Ok(row) = w.clone().downcast::<gtk::ListBoxRow>() {
                    // Claim the press so it doesn't propagate up to other
                    // controllers or get re-emitted as a synthetic click
                    // that the popover (once open) would treat as
                    // press-outside.
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    listbox.select_row(Some(&row));
                    show_context_menu(
                        &row,
                        x,
                        y,
                        client.clone(),
                        current_results.clone(),
                        active_tab.clone(),
                        listbox.clone(),
                        entry.clone(),
                        window.clone(),
                        context_menu_open.clone(),
                    );
                    return;
                }
                cursor = w.parent();
            }
        }
    ));
    listbox.add_controller(right_click);

    // Window-level Esc handler — capture phase so it always wins over
    // any focused widget. Esc closes the window no matter where the
    // user is.
    let win_keys = gtk::EventControllerKey::new();
    win_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    win_keys.connect_key_pressed(clone!(
        #[weak] window,
        #[upgrade_or] glib::Propagation::Proceed,
        move |_ctl, key, _code, _mods| {
            if key == gdk::Key::Escape {
                window.close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
    ));
    window.add_controller(win_keys);

    // Click-outside-to-close.
    //
    // On Wayland the user clicking another window or the desktop causes
    // Mutter to send xdg_toplevel.configure with focused=false, which
    // GTK surfaces on the window's `is-active` property. We watch for
    // the false transition and close.
    //
    // BUT: during the initial activation handshake Mutter shuffles
    // focus a couple of times — the window briefly goes active=true →
    // false → true as the activation token is validated and the user
    // is still releasing the hotkey modifiers (Ctrl+Alt+V → release
    // sequence). Without a grace period the window slammed itself shut
    // before the user ever saw it. The 500 ms gate lets the initial
    // focus dance complete; any real "user clicked outside" event after
    // that still closes us promptly.
    let open_at = std::time::Instant::now();
    let was_active = Rc::new(Cell::new(false));
    window.connect_is_active_notify(clone!(
        #[strong] was_active,
        #[strong] context_menu_open,
        move |w| {
            if w.is_active() {
                was_active.set(true);
                return;
            }
            if !was_active.get() {
                return;
            }
            if open_at.elapsed() < std::time::Duration::from_millis(500) {
                return;
            }
            // A popped-up context popover takes the keyboard grab and
            // flips us inactive; that's not the user clicking outside.
            if context_menu_open.get() {
                return;
            }
            w.close();
        }
    ));

    gtk::prelude::GtkWindowExt::set_focus(&window, Some(&entry));
    window.present();
}

fn refresh(
    client: &Rc<Client>,
    listbox: &gtk::ListBox,
    current_results: &Rc<RefCell<Vec<Result_>>>,
    active_tab: &Rc<Cell<Tab>>,
    query: &str,
) {
    let results: Vec<Result_> = match active_tab.get() {
        Tab::Clipboard => {
            let clips = if query.is_empty() {
                client.recent(MAX_RESULTS).unwrap_or_default()
            } else {
                client.search(query, MAX_RESULTS).unwrap_or_default()
            };
            clips.into_iter().map(Result_::Clip).collect()
        }
        Tab::Emoji => crate::emoji::search(query, MAX_RESULTS as usize)
            .into_iter()
            .map(Result_::Emoji)
            .collect(),
    };

    while let Some(child) = listbox.first_child() {
        listbox.remove(&child);
    }
    if results.is_empty() {
        let empty = gtk::Label::builder()
            .label(match active_tab.get() {
                Tab::Clipboard => "Nothing copied yet — copy something to get started.",
                Tab::Emoji => "No emoji matches that.",
            })
            .css_classes(vec!["empty".to_string()])
            .halign(gtk::Align::Center)
            .build();
        listbox.append(&empty);
        current_results.borrow_mut().clear();
        return;
    }
    for r in &results {
        listbox.append(&build_row(r));
    }
    current_results.replace(results);
    if let Some(first) = listbox.row_at_index(0) {
        listbox.select_row(Some(&first));
    }
}

fn build_row(r: &Result_) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    let h = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    let v = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .build();

    match r {
        Result_::Clip(clip) => {
            if clip.kind == ClipKind::Image {
                if let Some(thumb) = build_image_thumb(&clip.body) {
                    h.append(&thumb);
                }
                let title = gtk::Label::builder()
                    .label("Image")
                    .xalign(0.0)
                    .css_classes(vec!["preview-text".to_string()])
                    .build();
                v.append(&title);
                let meta = gtk::Label::builder()
                    .label(format!(
                        "image · {} · {}",
                        relative_age(clip.last_used_at),
                        human_bytes(clip.bytes)
                    ))
                    .xalign(0.0)
                    .css_classes(vec!["meta".to_string()])
                    .build();
                v.append(&meta);
            } else {
                let icon = gtk::Image::from_icon_name(clip.kind.icon_hint());
                icon.add_css_class("kind-icon");
                h.append(&icon);

                if clip.kind == ClipKind::HexColor {
                    if let Some(swatch) = build_hex_swatch(&clip.body) {
                        h.append(&swatch);
                    }
                }

                let preview = gtk::Label::builder()
                    .label(&clip.preview)
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .single_line_mode(true)
                    .css_classes(vec!["preview-text".to_string()])
                    .build();
                v.append(&preview);

                let meta = gtk::Label::builder()
                    .label(format_clip_meta(clip))
                    .xalign(0.0)
                    .css_classes(vec!["meta".to_string()])
                    .build();
                v.append(&meta);
            }
            h.append(&v);

            if clip.pinned {
                let pin = gtk::Label::new(Some("📌"));
                pin.add_css_class("pin-badge");
                h.append(&pin);
            }
        }
        Result_::Emoji(emoji) => {
            row.add_css_class("emoji-row");
            let glyph = gtk::Label::new(Some(emoji.as_str()));
            glyph.add_css_class("emoji-glyph");
            glyph.set_width_chars(2);
            h.append(&glyph);

            let name = gtk::Label::builder()
                .label(emoji.name())
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(vec!["preview-text".to_string()])
                .build();
            v.append(&name);

            let aliases: Vec<&str> = emoji.shortcodes().take(4).collect();
            if !aliases.is_empty() {
                let meta = gtk::Label::builder()
                    .label(format!(":{}:", aliases.join(": :")))
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(vec!["meta".to_string()])
                    .build();
                v.append(&meta);
            }

            h.append(&v);
        }
    }

    row.set_child(Some(&h));
    row
}

fn build_image_thumb(body: &[u8]) -> Option<gtk::Widget> {
    let bytes = glib::Bytes::from(body);
    let texture = gdk::Texture::from_bytes(&bytes).ok()?;
    let pic = gtk::Picture::for_paintable(&texture);
    pic.set_content_fit(gtk::ContentFit::Cover);
    pic.set_size_request(80, 60);
    pic.add_css_class("clip-thumb");
    Some(pic.upcast())
}

fn build_hex_swatch(body: &[u8]) -> Option<gtk::Widget> {
    let s = std::str::from_utf8(body).ok()?.trim();
    let rest = s.strip_prefix('#')?;
    let (r, g, b) = match rest.len() {
        3 => {
            let bytes = rest.as_bytes();
            (
                expand_nibble(bytes[0])?,
                expand_nibble(bytes[1])?,
                expand_nibble(bytes[2])?,
            )
        }
        6 | 8 => (
            u8::from_str_radix(&rest[0..2], 16).ok()?,
            u8::from_str_radix(&rest[2..4], 16).ok()?,
            u8::from_str_radix(&rest[4..6], 16).ok()?,
        ),
        _ => return None,
    };
    let swatch = gtk::DrawingArea::builder()
        .content_width(14)
        .content_height(14)
        .css_classes(vec!["swatch".to_string()])
        .build();
    swatch.set_draw_func(move |_, cr, w, h| {
        cr.set_source_rgb(r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0);
        cr.rectangle(0.0, 0.0, w as f64, h as f64);
        let _ = cr.fill();
    });
    Some(swatch.upcast())
}

fn expand_nibble(c: u8) -> Option<u8> {
    let n = (c as char).to_digit(16)? as u8;
    Some((n << 4) | n)
}

fn format_clip_meta(clip: &Clip) -> String {
    let kind = match clip.kind {
        ClipKind::Text => "text",
        ClipKind::Url => "url",
        ClipKind::HexColor => "color",
        ClipKind::Json => "json",
        ClipKind::Code => "code",
        ClipKind::Image => "image",
    };
    format!(
        "{kind} · {} · {}",
        relative_age(clip.last_used_at),
        human_bytes(clip.bytes)
    )
}

fn relative_age(unix: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(unix);
    let d = (now - unix).max(0);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{}m ago", d / 60),
        3600..=86_399 => format!("{}h ago", d / 3600),
        86_400..=2_591_999 => format!("{}d ago", d / 86_400),
        _ => "long ago".into(),
    }
}

fn human_bytes(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.0} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / 1024.0 / 1024.0)
    }
}

fn move_selection(listbox: &gtk::ListBox, delta: i32) {
    let count = count_rows(listbox);
    if count == 0 {
        return;
    }
    let cur = listbox.selected_row().map(|r| r.index()).unwrap_or(0);
    let next = (cur + delta).rem_euclid(count);
    if let Some(row) = listbox.row_at_index(next) {
        listbox.select_row(Some(&row));
        // Deliberately NO row.grab_focus() — keyboard focus stays on
        // the search entry so typing keeps landing in it.
    }
}

fn count_rows(listbox: &gtk::ListBox) -> i32 {
    let mut n = 0;
    let mut child = listbox.first_child();
    while let Some(c) = child {
        if c.is::<gtk::ListBoxRow>() {
            n += 1;
        }
        child = c.next_sibling();
    }
    n
}

fn toggle_pin(
    client: &Rc<Client>,
    current_results: &Rc<RefCell<Vec<Result_>>>,
    idx: i32,
) {
    let results = current_results.borrow();
    let Some(r) = results.get(idx as usize) else {
        return;
    };
    let Result_::Clip(clip) = r else {
        return;
    };
    let new_pinned = !clip.pinned;
    let id = clip.id;
    drop(results);
    if let Err(e) = client.set_pinned(id, new_pinned) {
        tracing::warn!("set_pinned({id}, {new_pinned}): {e:#}");
    }
}

fn activate_row(
    client: &Rc<Client>,
    current_results: &Rc<RefCell<Vec<Result_>>>,
    idx: i32,
    window: &adw::ApplicationWindow,
    mode: PasteMode,
) {
    let results = current_results.borrow();
    let Some(r) = results.get(idx as usize).cloned() else {
        return;
    };
    drop(results);

    match r {
        Result_::Clip(clip) => {
            let (bytes, mime) = match clip.kind {
                ClipKind::Image => (clip.body, Some("image/png")),
                _ => (clip.body, None),
            };
            copy_via_wl_copy(&bytes, mime);
            // Bump last_used_at so this clip rises in the recents list.
            // We call `activate` because the daemon still wraps touch in
            // that method; the daemon's clipboard write inside activate
            // fails harmlessly on Mutter (we handle the real write here
            // via wl-copy above), then touch runs.
            let _ = client.activate(clip.id);
        }
        Result_::Emoji(emoji) => {
            copy_via_wl_copy(emoji.as_str().as_bytes(), None);
        }
    }

    spawn_paste(mode);
    window.close();
}

/// Fire the post-activation paste keystroke into whichever app has focus
/// after we close. ydotool talks to /dev/uinput; if it isn't installed
/// or the uinput group isn't set up, this is a silent no-op (the clip
/// still landed on the system clipboard via wl-copy, so the user can
/// paste manually).
///
/// Linux input keycodes used here:
///   LEFTCTRL  = 29
///   LEFTSHIFT = 42
///   V         = 47
fn spawn_paste(mode: PasteMode) {
    let keys = match mode {
        PasteMode::Normal => "29:1 47:1 47:0 29:0",
        // Ctrl down, Shift down, V down/up, Shift up, Ctrl up.
        PasteMode::Super => "29:1 42:1 47:1 47:0 42:0 29:0",
    };
    let _ = Command::new("sh")
        .args([
            "-c",
            &format!(
                "sleep {:.3}; command -v ydotool >/dev/null && ydotool key {keys}",
                AUTO_PASTE_DELAY_MS as f64 / 1000.0
            ),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// Returns true if a clip was actually deleted (false for emojis or
/// out-of-range indices — both no-ops).
fn delete_row(
    client: &Rc<Client>,
    current_results: &Rc<RefCell<Vec<Result_>>>,
    idx: i32,
) -> bool {
    let results = current_results.borrow();
    let Some(Result_::Clip(clip)) = results.get(idx as usize) else {
        return false;
    };
    let id = clip.id;
    drop(results);
    if let Err(e) = client.delete(id) {
        tracing::warn!("delete({id}): {e:#}");
        return false;
    }
    true
}

#[allow(clippy::too_many_arguments)]
fn show_context_menu(
    row: &gtk::ListBoxRow,
    x: f64,
    y: f64,
    client: Rc<Client>,
    current_results: Rc<RefCell<Vec<Result_>>>,
    active_tab: Rc<Cell<Tab>>,
    listbox: gtk::ListBox,
    entry: gtk::Entry,
    window: adw::ApplicationWindow,
    context_menu_open: Rc<Cell<bool>>,
) {
    let idx = row.index();
    let (is_clip, is_pinned) = {
        let results = current_results.borrow();
        match results.get(idx as usize) {
            Some(Result_::Clip(c)) => (true, c.pinned),
            Some(Result_::Emoji(_)) => (false, false),
            None => return,
        }
    };

    let popover = gtk::Popover::builder()
        .has_arrow(false)
        .autohide(true)
        .css_classes(vec!["clipd-menu".to_string()])
        .build();

    let menu_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .build();

    if is_clip {
        let label = if is_pinned { "Unpin" } else { "Pin" };
        let btn = menu_item_button(label, "view-pin-symbolic", None);
        btn.connect_clicked(clone!(
            #[weak] popover,
            #[weak] listbox,
            #[weak] entry,
            #[strong] client,
            #[strong] current_results,
            #[strong] active_tab,
            move |_| {
                popover.popdown();
                toggle_pin(&client, &current_results, idx);
                let q = entry.text().to_string();
                refresh(&client, &listbox, &current_results, &active_tab, &q);
            }
        ));
        menu_box.append(&btn);
    }

    let btn = menu_item_button("Paste", "edit-paste-symbolic", Some("Enter"));
    btn.connect_clicked(clone!(
        #[weak] popover,
        #[weak] window,
        #[strong] client,
        #[strong] current_results,
        move |_| {
            popover.popdown();
            activate_row(
                &client,
                &current_results,
                idx,
                &window,
                PasteMode::Normal,
            );
        }
    ));
    menu_box.append(&btn);

    let btn = menu_item_button(
        "Super paste",
        "utilities-terminal-symbolic",
        Some("Ctrl+Shift+V"),
    );
    btn.connect_clicked(clone!(
        #[weak] popover,
        #[weak] window,
        #[strong] client,
        #[strong] current_results,
        move |_| {
            popover.popdown();
            activate_row(
                &client,
                &current_results,
                idx,
                &window,
                PasteMode::Super,
            );
        }
    ));
    menu_box.append(&btn);

    if is_clip {
        let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
        sep.add_css_class("menu-sep");
        menu_box.append(&sep);

        let btn = menu_item_button("Delete", "edit-delete-symbolic", Some("Ctrl+D"));
        btn.add_css_class("destructive");
        btn.connect_clicked(clone!(
            #[weak] popover,
            #[weak] listbox,
            #[weak] entry,
            #[strong] client,
            #[strong] current_results,
            #[strong] active_tab,
            move |_| {
                popover.popdown();
                if delete_row(&client, &current_results, idx) {
                    let q = entry.text().to_string();
                    refresh(&client, &listbox, &current_results, &active_tab, &q);
                    let count = count_rows(&listbox);
                    if count > 0 {
                        let new_idx = idx.min(count - 1);
                        if let Some(r) = listbox.row_at_index(new_idx) {
                            listbox.select_row(Some(&r));
                        }
                    }
                }
            }
        ));
        menu_box.append(&btn);
    }

    popover.set_child(Some(&menu_box));
    popover.set_parent(row);

    // GestureClick coords are in listbox-local space, but a popover
    // parented to the row needs row-local coords. compute_bounds gives
    // the row's bounds in its parent's (i.e. the listbox's) coordinate
    // space — subtract its origin to get a row-local point.
    let (rx, ry) = match row.parent().and_then(|p| row.compute_bounds(&p)) {
        Some(b) => {
            let w = row.width().max(1);
            let h = row.height().max(1);
            (
                (x as i32 - b.x() as i32).clamp(0, w - 1),
                (y as i32 - b.y() as i32).clamp(0, h - 1),
            )
        }
        None => (0, 0),
    };
    popover.set_pointing_to(Some(&gdk::Rectangle::new(rx, ry, 1, 1)));

    // Pop the popover off the row when it closes — otherwise the next
    // refresh() that destroys the row leaves a dangling parented popover.
    // Clear the suppression flag and return focus to the search entry.
    popover.connect_closed(clone!(
        #[weak] entry,
        #[strong] context_menu_open,
        move |p| {
            p.unparent();
            context_menu_open.set(false);
            entry.grab_focus();
        }
    ));
    context_menu_open.set(true);
    popover.popup();
}

/// Build a flat single-line button suitable for use inside the context
/// menu popover. `shortcut`, if Some, renders as a dimmed accelerator
/// hint right-aligned (e.g. "Ctrl+D").
fn menu_item_button(label: &str, icon_name: &str, shortcut: Option<&str>) -> gtk::Button {
    let btn = gtk::Button::builder()
        .css_classes(vec!["menu-item".to_string()])
        .build();
    let inner = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .build();
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.add_css_class("menu-icon");
    inner.append(&icon);
    let lbl = gtk::Label::builder()
        .label(label)
        .xalign(0.0)
        .hexpand(true)
        .build();
    inner.append(&lbl);
    if let Some(s) = shortcut {
        let kbd = gtk::Label::builder()
            .label(s)
            .css_classes(vec!["menu-shortcut".to_string()])
            .build();
        inner.append(&kbd);
    }
    btn.set_child(Some(&inner));
    btn
}

fn copy_via_wl_copy(bytes: &[u8], mime: Option<&str>) {
    let mut cmd = Command::new("wl-copy");
    if let Some(m) = mime {
        cmd.arg("--type").arg(m);
    }
    let Ok(mut child) = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(bytes);
    }
    let _ = child.wait();
}

fn snapshot_clipboard(
    client: &Rc<Client>,
    listbox_weak: &glib::WeakRef<gtk::ListBox>,
    current_results: &Rc<RefCell<Vec<Result_>>>,
    entry_weak: &glib::WeakRef<gtk::Entry>,
    active_tab: &Rc<Cell<Tab>>,
) {
    // Text first.
    let text_out = Command::new("wl-paste")
        .arg("--type")
        .arg("text/plain")
        .arg("--no-newline")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    if let Ok(o) = text_out {
        if o.status.success() && !o.stdout.is_empty() {
            let body = o.stdout;
            tracing::info!("snapshot: captured {} text bytes", body.len());
            if let Err(e) = client.ingest("text/plain".to_string(), body) {
                tracing::warn!("snapshot ingest: {e:#}");
            } else {
                maybe_refresh(client, listbox_weak, current_results, entry_weak, active_tab);
                return;
            }
        }
    }
    // No text — try image.
    let img_out = Command::new("wl-paste")
        .arg("--type")
        .arg("image/png")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    if let Ok(o) = img_out {
        if o.status.success() && !o.stdout.is_empty() {
            let body = o.stdout;
            tracing::info!("snapshot: captured {} image bytes", body.len());
            if let Err(e) = client.ingest("image/png".to_string(), body) {
                tracing::warn!("snapshot ingest: {e:#}");
            } else {
                maybe_refresh(client, listbox_weak, current_results, entry_weak, active_tab);
            }
        }
    }
}

fn maybe_refresh(
    client: &Rc<Client>,
    listbox_weak: &glib::WeakRef<gtk::ListBox>,
    current_results: &Rc<RefCell<Vec<Result_>>>,
    entry_weak: &glib::WeakRef<gtk::Entry>,
    active_tab: &Rc<Cell<Tab>>,
) {
    let query_empty = entry_weak
        .upgrade()
        .map(|e| e.text().is_empty())
        .unwrap_or(true);
    if !query_empty {
        return;
    }
    if active_tab.get() != Tab::Clipboard {
        return;
    }
    if let Some(listbox) = listbox_weak.upgrade() {
        refresh(client, &listbox, current_results, active_tab, "");
    }
}
