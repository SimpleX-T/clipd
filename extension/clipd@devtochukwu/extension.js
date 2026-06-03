// clipd — GNOME Shell extension
//
// Runs inside the Shell so we can hook into Mutter's clipboard owner-
// changed signal (background apps on Wayland can't observe clipboard
// changes — the protocols ext_data_control_v1 and wlr_data_control_v1
// are not exposed by Mutter). On every change we shell out to the
// clipd daemon's `ingest` subcommand and pipe the bytes through stdin;
// that subcommand opens a Unix socket to the daemon and posts an
// Ingest IPC frame.
//
// Why a subprocess and not a long-lived socket from JS: bincode framing
// is painful from GJS, and the `clipd ingest` subcommand already speaks
// the daemon protocol. Spawning is ~30 ms per copy — invisible at
// human cadence and far cheaper than polling.

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import St from 'gi://St';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

// MIME types we ask St.Clipboard for, in priority order. Text first
// because it's the common case and reading text auto-decodes it; image
// second so screenshot copy events are captured too.
const TEXT_MIME = 'text/plain;charset=utf-8';
const IMAGE_MIME = 'image/png';

// Resolve the clipd daemon binary. PPA installs land at /usr/bin/clipd;
// running install.sh from source lands at ~/.local/bin/clipd. We try
// PATH first (which usually covers both), then fall back to well-known
// install locations in case the GNOME Shell session was started with a
// stripped PATH.
function resolveClipdBin() {
    const onPath = GLib.find_program_in_path('clipd');
    if (onPath) return onPath;
    const candidates = [
        GLib.build_filenamev([GLib.get_home_dir(), '.local', 'bin', 'clipd']),
        '/usr/local/bin/clipd',
        '/usr/bin/clipd',
    ];
    for (const p of candidates) {
        if (GLib.file_test(p, GLib.FileTest.IS_EXECUTABLE)) return p;
    }
    return null;
}

export default class ClipdExtension extends Extension {
    enable() {
        this._clipdBin = resolveClipdBin();
        if (!this._clipdBin) {
            console.warn(
                'clipd: daemon binary not found in PATH or known install ' +
                'locations. Install via `sudo apt install clipd` (PPA) or ' +
                'run install.sh from source. The extension is enabled but ' +
                'ingestion will silently no-op until clipd is installed.'
            );
        }

        // Dedup by content hash so back-to-back identical reads (e.g.
        // the same Ctrl-C fires both PRIMARY and CLIPBOARD selections,
        // or apps that re-set the clipboard on focus) don't spam the
        // daemon. The daemon dedupes too, but skipping the spawn is
        // cheaper.
        this._lastTextHash = '';
        this._lastImageHash = '';

        const selection = global.display.get_selection();
        this._ownerChangedId = selection.connect(
            'owner-changed',
            this._onOwnerChanged.bind(this)
        );
        console.log('clipd: extension enabled');
    }

    disable() {
        if (this._ownerChangedId) {
            global.display.get_selection().disconnect(this._ownerChangedId);
            this._ownerChangedId = null;
        }
        this._lastTextHash = '';
        this._lastImageHash = '';
        this._clipdBin = null;
        console.log('clipd: extension disabled');
    }

    _onOwnerChanged(_sel, selectionType, _source) {
        // We only care about the CLIPBOARD (Ctrl+C target), not the
        // X11-style PRIMARY (middle-click select) or DND.
        if (selectionType !== Meta.SelectionType.SELECTION_CLIPBOARD) {
            return;
        }

        const clipboard = St.Clipboard.get_default();

        clipboard.get_text(St.ClipboardType.CLIPBOARD, (_cb, text) => {
            if (text && text.length > 0) {
                const hash = this._cheapHash(text);
                if (hash !== this._lastTextHash) {
                    this._lastTextHash = hash;
                    this._sendToDaemon(TEXT_MIME, text);
                }
                return;
            }
            // No text — try image.
            clipboard.get_content(
                St.ClipboardType.CLIPBOARD,
                IMAGE_MIME,
                (_cb2, bytes) => {
                    if (!bytes || bytes.get_size() === 0) {
                        return;
                    }
                    const data = bytes.get_data();
                    const hash = this._cheapHash(data);
                    if (hash !== this._lastImageHash) {
                        this._lastImageHash = hash;
                        this._sendToDaemon(IMAGE_MIME, bytes);
                    }
                }
            );
        });
    }

    _sendToDaemon(mime, payload) {
        // Re-resolve in case clipd was installed since enable() ran —
        // means users don't have to log out / log back in after
        // `apt install clipd` for the extension to start working.
        if (!this._clipdBin) {
            this._clipdBin = resolveClipdBin();
            if (!this._clipdBin) return;
        }
        try {
            const proc = Gio.Subprocess.new(
                [this._clipdBin, 'ingest', '--mime', mime],
                Gio.SubprocessFlags.STDIN_PIPE |
                    Gio.SubprocessFlags.STDOUT_SILENCE |
                    Gio.SubprocessFlags.STDERR_SILENCE
            );
            const stdin = proc.get_stdin_pipe();
            if (typeof payload === 'string') {
                const enc = new TextEncoder();
                stdin.write_bytes(
                    GLib.Bytes.new_take(enc.encode(payload)),
                    null
                );
            } else {
                // payload is a GLib.Bytes (from St.Clipboard.get_content)
                stdin.write_bytes(payload, null);
            }
            stdin.close(null);
            proc.wait_async(null, (p, res) => {
                try {
                    p.wait_finish(res);
                } catch (e) {
                    console.warn(`clipd: ingest wait failed: ${e}`);
                }
            });
        } catch (e) {
            console.warn(`clipd: ingest spawn failed: ${e}`);
        }
    }

    // Quick FNV-1a 32 — good enough for collision-rate dedupe on
    // sub-millisecond hot paths. JavaScript number == 32-bit hash.
    _cheapHash(input) {
        let bytes;
        if (typeof input === 'string') {
            bytes = new TextEncoder().encode(input);
        } else if (input instanceof Uint8Array) {
            bytes = input;
        } else {
            return '';
        }
        let h = 0x811c9dc5;
        for (let i = 0; i < bytes.length; i++) {
            h ^= bytes[i];
            h = Math.imul(h, 0x01000193);
        }
        return String(h >>> 0);
    }
}
