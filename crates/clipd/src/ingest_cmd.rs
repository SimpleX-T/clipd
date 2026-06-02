//! `clipd ingest --mime <mime>` — short-lived subprocess invoked by
//! `wl-paste --watch`. Reads stdin, sends to the daemon over the socket.

use std::io::Read;

use anyhow::{Context, Result};
use tokio::net::UnixStream;

use crate::ipc::{read_frame, socket_path, write_frame, InternalRequest};

pub async fn run(args: Vec<String>) -> Result<()> {
    let mime = parse_mime(&args).context("missing --mime")?;
    let mut body = Vec::new();
    std::io::stdin()
        .read_to_end(&mut body)
        .context("read stdin")?;

    // wl-paste --no-newline doesn't strip the trailing newline on every
    // payload (depends on source), but doesn't hurt to drop one if present
    // to keep dedupe consistent.
    if body.last() == Some(&b'\n') && mime.starts_with("text/") {
        body.pop();
    }

    if body.is_empty() {
        return Ok(());
    }

    let path = socket_path()?;
    let mut stream = UnixStream::connect(&path)
        .await
        .with_context(|| format!("connect daemon socket {}", path.display()))?;

    let req = InternalRequest::Ingest { mime, body };
    write_frame(&mut stream, &req).await?;
    let _: clipd_proto::Response = read_frame(&mut stream).await?;
    Ok(())
}

fn parse_mime(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--mime" {
            return it.next().cloned();
        }
        if let Some(v) = a.strip_prefix("--mime=") {
            return Some(v.to_string());
        }
    }
    None
}
