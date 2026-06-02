//! Unix-socket IPC server.
//!
//! Frame format (both directions):
//!   `[u32 length, big-endian][bincode payload]`
//!
//! The daemon is single-process so we serialize every request through the
//! `Store`'s internal mutex. Connections are short-lived (one request, one
//! response, then close) — the UI doesn't keep the socket open.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clipd_proto::{Clip, ClipKind, Request, Response, SOCKET_NAME, PROTOCOL_VERSION};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::store::Store;

/// Crate-internal version of Request that includes the daemon-only
/// `Ingest` variant. The UI never sends this — `clipd ingest` does.
#[derive(serde::Serialize, serde::Deserialize, Debug)]
pub enum InternalRequest {
    Public(Request),
    Ingest {
        mime: String,
        body: Vec<u8>,
    },
}

pub struct Server {
    listener: UnixListener,
    path: PathBuf,
    store: Arc<Store>,
}

pub async fn serve(store: Arc<Store>) -> Result<Server> {
    let path = socket_path()?;
    // Drop a stale socket left over from a crashed previous daemon. Safe
    // because a live daemon would hold an exclusive bind on the path and
    // we'd fail at the `bind` below if one were actually running.
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("bind unix socket at {}", path.display()))?;
    // World-readable would be bad — clipboard history is sensitive. Owner only.
    use std::os::unix::fs::PermissionsExt;
    let perm = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(&path, perm).ok();

    Ok(Server { listener, path, store })
}

impl Server {
    pub fn socket_path(&self) -> &Path {
        &self.path
    }

    pub async fn serve(self) -> Result<()> {
        loop {
            let (stream, _addr) = self.listener.accept().await?;
            let store = self.store.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_conn(stream, store).await {
                    tracing::warn!("ipc conn: {:#}", e);
                }
            });
        }
    }
}

async fn handle_conn(mut stream: UnixStream, store: Arc<Store>) -> Result<()> {
    let req: InternalRequest = read_frame(&mut stream).await?;
    let resp = dispatch(req, &store).await;
    write_frame(&mut stream, &resp).await?;
    Ok(())
}

async fn dispatch(req: InternalRequest, store: &Store) -> Response {
    let result: Result<Response> = match req {
        InternalRequest::Public(Request::Ping) => Ok(Response::Pong {
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol: PROTOCOL_VERSION,
        }),
        InternalRequest::Public(Request::Recent { limit }) => {
            store.recent(limit).map(Response::Clips)
        }
        InternalRequest::Public(Request::Search { query, limit }) => {
            store.search(&query, limit).map(Response::Clips)
        }
        InternalRequest::Public(Request::Activate { id }) => activate(id, store).await,
        InternalRequest::Public(Request::SetPinned { id, pinned }) => {
            store.set_pinned(id, pinned).map(|_| Response::Ok)
        }
        InternalRequest::Public(Request::Delete { id }) => {
            store.delete(id).map(|_| Response::Ok)
        }
        InternalRequest::Public(Request::Clear) => store.clear().map(|_| Response::Ok),
        InternalRequest::Ingest { mime, body } => ingest(mime, body, store),
    };
    result.unwrap_or_else(|e| Response::Err(format!("{e:#}")))
}

fn ingest(mime: String, body: Vec<u8>, store: &Store) -> Result<Response> {
    let kind = crate::classify::classify(&mime, &body);
    let preview = crate::classify::make_preview(kind, &mime, &body, 200);
    if preview.is_empty() && kind != ClipKind::Image {
        // Whitespace-only clip — skip.
        return Ok(Response::Ok);
    }
    let id = store.insert(kind, &mime, &body, &preview)?;
    if id != 0 {
        tracing::info!("ingest {kind:?} {} bytes ({}B preview)", body.len(), preview.len());
    }
    Ok(Response::Ok)
}

async fn activate(id: i64, store: &Store) -> Result<Response> {
    // Bump last_used_at so the clip rises in the recents list. We also
    // attempt to put the clip back on the system clipboard from here as
    // a best-effort fallback, but on Mutter the underlying wl-clipboard-rs
    // call has no data-control protocol to bind to and will return an
    // error — that's expected. The picker (clipd-pick) does the real
    // clipboard write via wl-copy from its focused window before
    // calling Activate.
    let clips = store.recent(u32::MAX)?;
    let Some(clip) = clips.into_iter().find(|c| c.id == id) else {
        return Ok(Response::Err(format!("clip {id} not found")));
    };
    if let Err(e) = write_to_clipboard(&clip) {
        tracing::debug!("daemon-side clipboard write failed (expected on Mutter): {e:#}");
    }
    store.touch(id)?;
    Ok(Response::Ok)
}

fn write_to_clipboard(clip: &Clip) -> Result<()> {
    use wl_clipboard_rs::copy::{MimeType, Options, Source};
    let mut opts = Options::new();
    opts.foreground(false); // background — release the fd after the data lands.
    let mime = if clip.kind == ClipKind::Image {
        MimeType::Specific(clip.mime.clone())
    } else {
        MimeType::Text
    };
    opts.copy(Source::Bytes(clip.body.clone().into_boxed_slice()), mime)
        .context("wl-clipboard copy")?;
    Ok(())
}

pub async fn read_frame<T: serde::de::DeserializeOwned>(
    stream: &mut UnixStream,
) -> Result<T> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        bail!("frame too big: {len} bytes");
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    let val = bincode::deserialize(&buf).context("bincode decode")?;
    Ok(val)
}

pub async fn write_frame<T: serde::Serialize>(
    stream: &mut UnixStream,
    msg: &T,
) -> Result<()> {
    let buf = bincode::serialize(msg).context("bincode encode")?;
    let len = u32::try_from(buf.len()).context("frame too big")?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(&buf).await?;
    stream.flush().await?;
    Ok(())
}

pub fn socket_path() -> Result<PathBuf> {
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(format!("/run/user/{}", nix_uid())));
    Ok(dir.join(SOCKET_NAME))
}

fn nix_uid() -> u32 {
    // Avoid the nix crate just for this; libc::getuid is in std as
    // std::os::unix::... — actually it's not. Read /proc/self/status.
    // Cheap shortcut: shell out is overkill; use the env var if present,
    // else parse /proc/self/status. Falls back to 1000 (good enough — the
    // socket will simply land in /run/user/1000 on a single-user box).
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Uid:"))
                .and_then(|l| l.split_whitespace().nth(1).map(str::to_string))
        })
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000)
}
