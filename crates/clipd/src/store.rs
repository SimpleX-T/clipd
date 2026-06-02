//! SQLite history store.
//!
//! Schema:
//!   `clips(id, kind, mime, body, preview, hash, bytes,
//!          created_at, last_used_at, use_count, pinned)`
//!   `clips_fts` virtual FTS5 table mirrors `preview` for fast search.
//!
//! `hash` is BLAKE-style — actually a fast 64-bit xxhash of `body` — used
//! to dedupe back-to-back identical clips. We coalesce by promoting the
//! existing row's `last_used_at` + `use_count` instead of inserting again.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clipd_proto::{Clip, ClipKind};
use rusqlite::{params, Connection, OptionalExtension};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS clips (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    kind          INTEGER NOT NULL,
    mime          TEXT    NOT NULL,
    body          BLOB    NOT NULL,
    preview       TEXT    NOT NULL,
    hash          INTEGER NOT NULL,
    bytes         INTEGER NOT NULL,
    created_at    INTEGER NOT NULL,
    last_used_at  INTEGER NOT NULL,
    use_count     INTEGER NOT NULL DEFAULT 1,
    pinned        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS clips_hash    ON clips(hash);
CREATE INDEX IF NOT EXISTS clips_recent  ON clips(last_used_at DESC);
CREATE INDEX IF NOT EXISTS clips_pinned  ON clips(pinned DESC, last_used_at DESC);

CREATE VIRTUAL TABLE IF NOT EXISTS clips_fts
USING fts5(preview, content='clips', content_rowid='id', tokenize='unicode61');

CREATE TRIGGER IF NOT EXISTS clips_ai AFTER INSERT ON clips BEGIN
    INSERT INTO clips_fts(rowid, preview) VALUES (new.id, new.preview);
END;
CREATE TRIGGER IF NOT EXISTS clips_ad AFTER DELETE ON clips BEGIN
    INSERT INTO clips_fts(clips_fts, rowid, preview) VALUES ('delete', old.id, old.preview);
END;
CREATE TRIGGER IF NOT EXISTS clips_au AFTER UPDATE ON clips BEGIN
    INSERT INTO clips_fts(clips_fts, rowid, preview) VALUES ('delete', old.id, old.preview);
    INSERT INTO clips_fts(rowid, preview) VALUES (new.id, new.preview);
END;
"#;

/// Anything text-typed longer than this in bytes is too big to keep — we
/// almost certainly grabbed someone's pasted file or screenshot-as-text.
const MAX_TEXT_BYTES: usize = 1024 * 1024; // 1 MiB
/// Images can be larger; cap at 8 MiB so a single huge screenshot doesn't
/// blow up the db.
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;
/// Trim history beyond this to keep the picker snappy.
const MAX_HISTORY_ROWS: i64 = 2000;
/// Unpinned clips older than this get dropped on daemon startup.
const TTL_SECONDS: i64 = 86_400; // 24h

pub struct Store {
    db_path: PathBuf,
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open() -> Result<Self> {
        let db_path = data_dir()?.join("history.db");
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create data dir {}", parent.display()))?;
        }
        let conn = Connection::open(&db_path)
            .with_context(|| format!("open sqlite {}", db_path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA).context("apply schema")?;
        Ok(Self { db_path, conn: Mutex::new(conn) })
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// Insert a new clip OR coalesce with the most recent identical one.
    /// Returns the row id of the (possibly-existing) clip.
    pub fn insert(
        &self,
        kind: ClipKind,
        mime: &str,
        body: &[u8],
        preview: &str,
    ) -> Result<i64> {
        let max = if kind == ClipKind::Image { MAX_IMAGE_BYTES } else { MAX_TEXT_BYTES };
        if body.len() > max {
            tracing::debug!(
                "skip oversized clip: kind={:?} bytes={} max={}",
                kind, body.len(), max
            );
            // Return a sentinel — caller can ignore.
            return Ok(0);
        }
        let hash = xxhash(body);
        let now = unix_now();

        let conn = self.conn.lock().unwrap();

        // Try to coalesce with the most recent clip of same hash + kind.
        let existing: Option<i64> = conn
            .query_row(
                "SELECT id FROM clips WHERE hash = ?1 AND kind = ?2
                 ORDER BY last_used_at DESC LIMIT 1",
                params![hash as i64, kind as u8 as i64],
                |r| r.get(0),
            )
            .optional()?;

        if let Some(id) = existing {
            conn.execute(
                "UPDATE clips SET last_used_at = ?1, use_count = use_count + 1 WHERE id = ?2",
                params![now, id],
            )?;
            return Ok(id);
        }

        conn.execute(
            "INSERT INTO clips
               (kind, mime, body, preview, hash, bytes, created_at, last_used_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                kind as u8 as i64,
                mime,
                body,
                preview,
                hash as i64,
                body.len() as i64,
                now,
            ],
        )?;
        let id = conn.last_insert_rowid();

        // Trim history. Pinned rows are spared.
        conn.execute(
            "DELETE FROM clips WHERE id IN (
                SELECT id FROM clips WHERE pinned = 0
                ORDER BY last_used_at DESC LIMIT -1 OFFSET ?1
             )",
            params![MAX_HISTORY_ROWS],
        )?;

        Ok(id)
    }

    pub fn recent(&self, limit: u32) -> Result<Vec<Clip>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, kind, mime, body, preview, bytes, created_at,
                    last_used_at, use_count, pinned
               FROM clips
              ORDER BY pinned DESC, last_used_at DESC
              LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], row_to_clip)?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<Clip>> {
        let conn = self.conn.lock().unwrap();
        // FTS5 'NEAR' / prefix query — append '*' for cheap prefix match.
        let q = fts_query(query);
        let mut stmt = conn.prepare(
            "SELECT c.id, c.kind, c.mime, c.body, c.preview, c.bytes,
                    c.created_at, c.last_used_at, c.use_count, c.pinned
               FROM clips c
               JOIN clips_fts f ON f.rowid = c.id
              WHERE clips_fts MATCH ?1
              ORDER BY c.pinned DESC, bm25(clips_fts), c.last_used_at DESC
              LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![q, limit as i64], row_to_clip)?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE clips SET pinned = ?1 WHERE id = ?2",
            params![if pinned { 1 } else { 0 }, id],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM clips WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn clear(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM clips", [])?;
        Ok(())
    }

    /// Delete unpinned clips whose `last_used_at` is older than the TTL.
    /// Called once at daemon startup. Pinned clips are spared regardless
    /// of age — that's the entire point of pinning.
    pub fn cleanup_expired(&self) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let cutoff = unix_now() - TTL_SECONDS;
        let n = conn.execute(
            "DELETE FROM clips WHERE pinned = 0 AND last_used_at < ?1",
            params![cutoff],
        )?;
        Ok(n)
    }

    pub fn touch(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE clips SET last_used_at = ?1, use_count = use_count + 1 WHERE id = ?2",
            params![unix_now(), id],
        )?;
        Ok(())
    }
}

fn row_to_clip(r: &rusqlite::Row<'_>) -> rusqlite::Result<Clip> {
    let kind_int: i64 = r.get(1)?;
    let kind = match kind_int {
        x if x == ClipKind::Text as i64 => ClipKind::Text,
        x if x == ClipKind::Url as i64 => ClipKind::Url,
        x if x == ClipKind::HexColor as i64 => ClipKind::HexColor,
        x if x == ClipKind::Json as i64 => ClipKind::Json,
        x if x == ClipKind::Code as i64 => ClipKind::Code,
        x if x == ClipKind::Image as i64 => ClipKind::Image,
        _ => ClipKind::Text,
    };
    Ok(Clip {
        id: r.get(0)?,
        kind,
        mime: r.get(2)?,
        body: r.get(3)?,
        preview: r.get(4)?,
        bytes: r.get::<_, i64>(5)? as u64,
        created_at: r.get(6)?,
        last_used_at: r.get(7)?,
        use_count: r.get::<_, i64>(8)? as u32,
        pinned: r.get::<_, i64>(9)? != 0,
    })
}

/// Convert free-text user query into an FTS5 MATCH expression with prefix
/// terms. Filters out characters FTS5 treats as operators so users can paste
/// arbitrary strings without "no such column" errors.
fn fts_query(input: &str) -> String {
    let mut out = String::new();
    for tok in input.split_whitespace() {
        let cleaned: String = tok
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        if cleaned.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push('"');
        out.push_str(&cleaned);
        out.push_str("\"*");
    }
    if out.is_empty() {
        // FTS5 MATCH can't be empty; this matches everything via prefix on a space.
        "\"\"*".to_string()
    } else {
        out
    }
}

fn data_dir() -> Result<PathBuf> {
    let base = dirs::data_dir().context("no XDG_DATA_HOME / ~/.local/share")?;
    Ok(base.join("clipd"))
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// xxhash64-style mixer. Cheap, good enough for dedup keying — we don't
/// need cryptographic strength here.
fn xxhash(data: &[u8]) -> u64 {
    // FNV-1a 64. Tiny code, zero deps. Collisions are not a correctness
    // issue (we also key on kind), only a "two identical-hashing clips
    // collapse into one" risk, which is vanishingly unlikely at our scale.
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}
