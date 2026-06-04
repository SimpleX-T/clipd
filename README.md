# clipd

A clipboard manager and emoji picker for GNOME on Wayland. One hotkey opens a
Raycast-style frosted command palette; tabs switch between clipboard history
and a Unicode 16 emoji catalog.

![clipd palette](screenshots/clipd-palette.png)

## Features

- **Clipboard history** — text, URLs, hex colors, JSON, code snippets, images
- **Emoji picker** — ~3,800 entries from Unicode 16, ranked by name / alias /
  shortcode match
- **Pin** — keep clips past the 24-hour auto-expiry
- **Auto-paste** — selected clip lands on the system clipboard *and* is pasted
  into the previously focused app
- **Super paste (`Ctrl+Shift+V`)** — for terminals and "paste as plain text"
- **Per-row context menu** — Pin, Paste, Super paste, Delete
- **GNOME Shell extension** — continuous capture, so the picker shows every
  copy you made even when it wasn't open

## Installing

### Ubuntu / GNOME (PPA, recommended)

```bash
sudo add-apt-repository ppa:devtochukwu/clipd
sudo apt update
sudo apt install clipd
```

Then, once per user (these can't be done by the package — they're
session-scoped):

```bash
systemctl --user enable --now clipd.service
gnome-extensions enable clipd@devtochukwu
# Bind a hotkey of your choice to /usr/bin/clipd-pick (Ctrl+Alt+V suggested).
```

Supported series: **questing** (25.04), **resolute** (26.04).

**Noble** (24.04 LTS) is not in the PPA. Noble's cargo is 1.75, which
predates Cargo features now used by the crates.io registry itself — so
even a fully-pinned dep tree fails to parse on noble. Noble users can
install from source after `rustup toolchain install stable`; see the
"From source" section. Jammy (22.04) is out for a different reason —
GTK 4.6 / libadwaita 1.0 are too old.

### From source

Dependencies (Ubuntu 24.04+ / Debian trixie):

```bash
sudo apt install \
    libgtk-4-dev libadwaita-1-dev libsqlite3-dev \
    wl-clipboard ydotool \
    cargo
```

Then:

```bash
git clone https://github.com/simplex-t/clipd
cd clipd
./scripts/install.sh
```

The installer builds the binaries, copies them to `~/.local/bin`, installs
the systemd user service and `.desktop` entry, registers the GNOME hotkey,
and copies the Shell extension into `~/.local/share/gnome-shell/extensions/`.

After install, **log out and back in** so GNOME Shell can pick up the
extension, then:

```bash
gnome-extensions enable clipd@devtochukwu
```

## Keyboard reference

| Key                    | Action                                              |
|------------------------|-----------------------------------------------------|
| `Ctrl+Alt+V`           | Open picker                                         |
| `Up` / `Down`          | Move selection                                      |
| `Enter`                | Paste (`Ctrl+V`)                                    |
| `Shift+Enter`          | Super paste (`Ctrl+Shift+V`) — terminals & friends  |
| `Ctrl+P`               | Pin / unpin selected clip                           |
| `Ctrl+D`               | Delete selected clip                                |
| `Ctrl+1` / `Ctrl+2`    | Switch tab (Clipboard / Emojis)                     |
| `Esc`                  | Close picker                                        |
| Right-click on a row   | Context menu                                        |

## Architecture

```
[any app: Ctrl+C]
   ↓ Mutter clipboard.owner-changed
[GNOME Shell] clipd extension
   ↓ spawn `clipd ingest --mime …`
[clipd daemon] classify + insert into SQLite
   ↑
[clipd-pick] Unix-socket IPC (bincode frames)
```

Three pieces:

| Piece            | Role                                                              |
|------------------|-------------------------------------------------------------------|
| `clipd` daemon   | systemd user service; owns the SQLite history; listens on `$XDG_RUNTIME_DIR/clipd.sock` |
| `clipd-pick`     | GTK4 / libadwaita palette UI; short-lived per activation          |
| Shell extension  | GJS; runs *inside* `gnome-shell` so it can use the privileged `St.Clipboard` |

The Shell extension exists because Mutter doesn't advertise
`wlr_data_control_v1` or `ext_data_control_v1` — the standard protocols a
background Wayland client would use to observe clipboard changes. Running
inside the Shell sidesteps that.

## License

Licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option.
