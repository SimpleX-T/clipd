#!/usr/bin/env bash
# clipd installer — local user install + GNOME hotkey wiring.
#
#   ./scripts/install.sh                    # build, install, enable
#   ./scripts/install.sh --no-build         # use whatever's in target/release
#   ./scripts/install.sh --hotkey '<Super>v' (default)
#   ./scripts/install.sh --no-hotkey        # skip GNOME keybinding step
#   ./scripts/install.sh uninstall          # tear everything down

set -euo pipefail

HOTKEY='<Super>v'
DO_BUILD=1
ACTION=do_install

# /usr/bin/install (coreutils) — fully qualified so our `do_install` shell
# function name can't shadow it. The whole reason this script used to loop
# was that `install` here was hitting the shell function recursively.
INSTALL=/usr/bin/install

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-build)   DO_BUILD=0; shift;;
        --hotkey)     HOTKEY="$2"; shift 2;;
        --no-hotkey)  HOTKEY=''; shift;;
        uninstall)    ACTION=do_uninstall; shift;;
        -h|--help)
            sed -n '2,12p' "$0"; exit 0;;
        *) echo "unknown arg: $1" >&2; exit 2;;
    esac
done

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN_DIR="$HOME/.local/bin"
UNIT_DIR="$HOME/.config/systemd/user"

# GNOME custom-keybindings dconf path. We park ours in slot 0; the script
# is careful not to clobber an existing user binding in that slot.
KB_ROOT='org.gnome.settings-daemon.plugins.media-keys'
KB_LIST_KEY="custom-keybindings"
KB_PATH='/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/clipd/'
KB_FULL="$KB_ROOT.custom-keybinding:$KB_PATH"

do_install() {
    if (( DO_BUILD )); then
        echo "==> cargo build --release"
        ( cd "$ROOT" && cargo build --release )
    fi
    mkdir -p "$BIN_DIR" "$UNIT_DIR"
    "$INSTALL" -m 755 "$ROOT/target/release/clipd"      "$BIN_DIR/clipd"
    "$INSTALL" -m 755 "$ROOT/target/release/clipd-pick" "$BIN_DIR/clipd-pick"

    "$INSTALL" -m 644 "$ROOT/data/clipd.service" "$UNIT_DIR/clipd.service"

    # .desktop file — required for the Wayland compositor to recognize
    # clipd-pick as a legitimate app via its application_id. Without this,
    # Mutter focus-stealing-prevention will bounce the palette window.
    DESKTOP_DIR="$HOME/.local/share/applications"
    mkdir -p "$DESKTOP_DIR"
    "$INSTALL" -m 644 "$ROOT/data/io.github.devtochukwu.Clipd.desktop" \
        "$DESKTOP_DIR/io.github.devtochukwu.Clipd.desktop"
    update-desktop-database "$DESKTOP_DIR" 2>/dev/null || true

    # App icon — resolves the .desktop file's Icon=io.github.devtochukwu.Clipd
    # entry. Without it the dock/Activities preview falls back to a generic
    # paste glyph.
    ICON_DIR="$HOME/.local/share/icons/hicolor/scalable/apps"
    mkdir -p "$ICON_DIR"
    "$INSTALL" -m 644 \
        "$ROOT/data/icons/hicolor/scalable/apps/io.github.devtochukwu.Clipd.svg" \
        "$ICON_DIR/io.github.devtochukwu.Clipd.svg"
    gtk-update-icon-cache -q -t -f "$HOME/.local/share/icons/hicolor" 2>/dev/null || true

    # GNOME Shell extension — pushes every clipboard change to the
    # daemon so the picker shows full history. Without it the daemon
    # only ingests the current clip when the picker opens (passive
    # mode), and clips you copy without opening the picker are lost.
    EXT_DIR="$HOME/.local/share/gnome-shell/extensions/clipd@devtochukwu"
    mkdir -p "$EXT_DIR"
    "$INSTALL" -m 644 "$ROOT/extension/clipd@devtochukwu/metadata.json" \
        "$EXT_DIR/metadata.json"
    "$INSTALL" -m 644 "$ROOT/extension/clipd@devtochukwu/extension.js" \
        "$EXT_DIR/extension.js"

    systemctl --user daemon-reload
    systemctl --user enable --now clipd.service

    if [[ -n "$HOTKEY" ]]; then
        register_hotkey
    fi

    cat <<EOF

✓ clipd installed.

  binaries:        $BIN_DIR/clipd, $BIN_DIR/clipd-pick
  daemon (user):   systemctl --user status clipd
  history db:      \$XDG_DATA_HOME/clipd/history.db
  socket:          \$XDG_RUNTIME_DIR/clipd.sock
  emoji cache:     \$XDG_CACHE_HOME/clipd/emoji/3d/ (downloaded on first launch)

  hotkey:          ${HOTKEY:-<skipped>}

Press the hotkey to open the picker.  First open downloads Fluent 3D
emoji (~5 MB) in the background — until then, the system Noto Color
Emoji is used.
EOF
}

do_uninstall() {
    echo "==> stopping service"
    systemctl --user disable --now clipd.service 2>/dev/null || true
    rm -f "$UNIT_DIR/clipd.service"
    systemctl --user daemon-reload

    echo "==> removing binaries"
    rm -f "$BIN_DIR/clipd" "$BIN_DIR/clipd-pick"

    echo "==> removing GNOME hotkey (if any)"
    remove_hotkey || true

    echo "==> keeping history.db and emoji cache (delete manually if you want)"
    echo "    rm -rf ~/.local/share/clipd ~/.cache/clipd"
}

register_hotkey() {
    # Add our path to the list of custom-keybindings (idempotent).
    local cur new
    cur="$(gsettings get "$KB_ROOT" "$KB_LIST_KEY")"
    if [[ "$cur" == *"$KB_PATH"* ]]; then
        new="$cur"
    else
        if [[ "$cur" == '@as []' || "$cur" == '[]' ]]; then
            new="['$KB_PATH']"
        else
            # Strip trailing ] and append.
            new="${cur%]*}, '$KB_PATH']"
        fi
        gsettings set "$KB_ROOT" "$KB_LIST_KEY" "$new"
    fi

    gsettings set "$KB_FULL" name    'clipd: clipboard / emoji picker'
    gsettings set "$KB_FULL" command "$BIN_DIR/clipd-pick"
    gsettings set "$KB_FULL" binding "$HOTKEY"

    echo "    hotkey set: $HOTKEY → $BIN_DIR/clipd-pick"
}

remove_hotkey() {
    local cur new
    cur="$(gsettings get "$KB_ROOT" "$KB_LIST_KEY")"
    new="$(python3 -c '
import ast, sys
cur = ast.literal_eval(sys.argv[1]) if sys.argv[1] != "@as []" else []
cur = [p for p in cur if p != sys.argv[2]]
print(cur if cur else [])
' "$cur" "$KB_PATH")"
    gsettings set "$KB_ROOT" "$KB_LIST_KEY" "$new"
    gsettings reset-recursively "$KB_FULL" 2>/dev/null || true
}

"$ACTION"
