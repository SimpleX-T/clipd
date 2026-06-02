//! Wire format shared between `clipd` (daemon) and `clipd-pick` (UI).
//!
//! Length-prefixed bincode frames over a Unix socket at
//! `$XDG_RUNTIME_DIR/clipd.sock`. The UI is a short-lived client; the
//! daemon is long-lived and authoritative.

use serde::{Deserialize, Serialize};

pub const SOCKET_NAME: &str = "clipd.sock";
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// Health check; daemon replies with its version.
    Ping,
    /// Return the most recent `limit` clips, newest first.
    Recent { limit: u32 },
    /// Full-text search across clip text bodies.
    Search { query: String, limit: u32 },
    /// Promote a clip back to the system clipboard (i.e. "paste this").
    Activate { id: i64 },
    /// Pin or unpin a clip so it sticks to the top of results.
    SetPinned { id: i64, pinned: bool },
    /// Remove one clip from history.
    Delete { id: i64 },
    /// Wipe the entire history (irreversible).
    Clear,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    Pong { version: String, protocol: u32 },
    Clips(Vec<Clip>),
    Ok,
    Err(String),
}

/// A single entry in clipboard history.
///
/// `body` is the canonical representation:
///   - Text/URL/Hex/JSON/Code  → UTF-8 text
///   - Image                   → raw PNG bytes (preview thumbnail stored separately)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: i64,
    pub kind: ClipKind,
    pub body: Vec<u8>,
    /// Short display string (truncated text or "image 1280x720" etc.).
    pub preview: String,
    pub mime: String,
    pub bytes: u64,
    /// Unix seconds when this clip was captured.
    pub created_at: i64,
    pub last_used_at: i64,
    pub use_count: u32,
    pub pinned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipKind {
    Text,
    Url,
    HexColor,
    Json,
    Code,
    Image,
}

impl ClipKind {
    pub fn icon_hint(self) -> &'static str {
        match self {
            ClipKind::Text => "edit-paste-symbolic",
            ClipKind::Url => "web-browser-symbolic",
            ClipKind::HexColor => "color-select-symbolic",
            ClipKind::Json => "text-x-generic-symbolic",
            ClipKind::Code => "utilities-terminal-symbolic",
            ClipKind::Image => "image-x-generic-symbolic",
        }
    }
}
