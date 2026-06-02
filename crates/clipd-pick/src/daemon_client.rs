//! Blocking client for the clipd daemon.
//!
//! We run on the GTK main thread; calls are issued via `glib::spawn_future_local`
//! + a background thread that owns its own short-lived UnixStream. Each call
//! is one request, one response, then closes the stream.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clipd_proto::{Clip, Request, Response, SOCKET_NAME};

pub struct Client {
    socket: PathBuf,
}

impl Client {
    pub fn new() -> Result<Self> {
        let dir = std::env::var("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(format!("/run/user/{}", uid())));
        Ok(Self { socket: dir.join(SOCKET_NAME) })
    }

    /// Push raw bytes + MIME to the daemon's history. Called by the
    /// window once it has read the system clipboard via GTK4 (which
    /// uses the standard `wl_data_device_manager` — works because the
    /// picker has focus when it asks).
    ///
    /// Why we don't read the clipboard here in the client and instead
    /// take pre-read bytes: `wl-clipboard-rs` also requires the
    /// `wlr_data_control_v1` / `ext_data_control_v1` Wayland protocols
    /// which Mutter does NOT advertise (verified by raw registry
    /// probe). So even from a focused client, wl-clipboard-rs fails on
    /// GNOME. GTK4's clipboard API does work because it uses the
    /// standard focus-gated data device. We let the window do the read
    /// and just plumb the bytes through here.
    pub fn ingest(&self, mime: String, body: Vec<u8>) -> Result<()> {
        let stream = UnixStream::connect(&self.socket)
            .with_context(|| format!("connect {}", self.socket.display()))?;
        let wire = InternalRequest::Ingest { mime, body };
        let buf = bincode::serialize(&wire).context("encode")?;
        write_frame(&stream, &buf)?;
        let resp_buf = read_frame(&stream)?;
        let resp: Response = bincode::deserialize(&resp_buf).context("decode")?;
        match resp {
            Response::Ok => Ok(()),
            Response::Err(e) => bail!("daemon: {e}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    pub fn recent(&self, limit: u32) -> Result<Vec<Clip>> {
        match self.call(&Request::Recent { limit })? {
            Response::Clips(c) => Ok(c),
            Response::Err(e) => bail!("daemon: {e}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<Clip>> {
        match self.call(&Request::Search { query: query.into(), limit })? {
            Response::Clips(c) => Ok(c),
            Response::Err(e) => bail!("daemon: {e}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    pub fn activate(&self, id: i64) -> Result<()> {
        match self.call(&Request::Activate { id })? {
            Response::Ok => Ok(()),
            Response::Err(e) => bail!("daemon: {e}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<()> {
        match self.call(&Request::SetPinned { id, pinned })? {
            Response::Ok => Ok(()),
            Response::Err(e) => bail!("daemon: {e}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        match self.call(&Request::Delete { id })? {
            Response::Ok => Ok(()),
            Response::Err(e) => bail!("daemon: {e}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    fn call(&self, req: &Request) -> Result<Response> {
        let stream = UnixStream::connect(&self.socket)
            .with_context(|| format!("connect {}", self.socket.display()))?;
        // Wrap in the daemon-side InternalRequest::Public variant by hand —
        // we keep a small copy of that enum here so we don't depend on the
        // daemon crate just for one type.
        let wire = InternalRequest::Public(req.clone());
        let buf = bincode::serialize(&wire).context("encode")?;
        write_frame(&stream, &buf)?;
        let resp_buf = read_frame(&stream)?;
        let resp: Response = bincode::deserialize(&resp_buf).context("decode")?;
        Ok(resp)
    }
}

/// Mirror of `clipd::ipc::InternalRequest`. Both variants are now used:
/// `Public` for normal queries (Recent, Search, Activate, …) and
/// `Ingest` for the focus-time clipboard snapshot the picker performs
/// on every open.
#[derive(serde::Serialize)]
enum InternalRequest {
    Public(Request),
    Ingest { mime: String, body: Vec<u8> },
}

pub(crate) fn write_frame(mut stream: &UnixStream, buf: &[u8]) -> Result<()> {
    let len = u32::try_from(buf.len()).context("frame too big")?;
    stream.write_all(&len.to_be_bytes())?;
    stream.write_all(buf)?;
    stream.flush()?;
    Ok(())
}

pub(crate) fn read_frame(mut stream: &UnixStream) -> Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        bail!("frame too big: {len}");
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}

fn uid() -> u32 {
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
