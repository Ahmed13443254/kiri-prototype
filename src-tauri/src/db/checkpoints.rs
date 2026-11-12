// Checkpoints: the pre-edit body of a file, content-addressed by sha256.
//
// architecture.md: `recordCheckpoint` stores the pre-edit body by content hash;
// `restoreCheckpoint` returns the file as it was before a turn. File blobs are
// deduplicated, so the same body written in ten turns is one row.

use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde_json::{json, Value};
use sha2::Digest;

use rusqlite::Connection;

pub fn sha256_bytes(bytes: &[u8]) -> String {
    let mut h = sha2::Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Store a body once under its hash. Returns the hash.
pub fn put_blob(conn: &Connection, body: &[u8]) -> Result<String> {
    let sha = sha256_bytes(body);
    let text = String::from_utf8(body.to_vec()).ok();
    conn.execute(
        "insert into file_blobs (sha, size, binary, body)
         values (?1, ?2, ?3, ?4)
         on conflict (sha) do nothing",
        params![sha, body.len() as i64, if text.is_none() { 1 } else { 0 }, text],
    )?;
    Ok(sha)
}

/// Record the file as it was BEFORE a turn touched it. `existed_pre = false`
/// means the turn created it, so restore is a delete.
pub fn record_checkpoint(
    conn: &Connection,
    id: &str,
    session_id: &str,
    turn_id: Option<i64>,
    path: &str,
    blob_sha: Option<&str>,
    existed_pre: bool,
) -> Result<()> {
    conn.execute(
        "insert into file_checkpoints (id, session_id, turn_id, path, blob_sha, existed_pre, created_at)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            session_id,
            turn_id,
            path,
            blob_sha,
            if existed_pre { 1 } else { 0 },
            now()
        ],
    )?;
    Ok(())
}

/// The last checkpoint of `path` at or before `turn_id`; restore means "as the
/// file was when that turn began".
pub fn checkpoint_before(
    conn: &Connection,
    session_id: &str,
    path: &str,
    turn_id: Option<i64>,
) -> Result<Option<Value>> {
    let row: Option<(String, Option<String>, i64, Option<i64>, String)> = conn
        .query_row(
            "select id, blob_sha, existed_pre, turn_id, created_at
               from file_checkpoints
              where session_id = ?1 and path = ?2
                and (?3 is null or turn_id is null or turn_id <= ?3)
              order by created_at desc limit 1",
            params![session_id, path, turn_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;

    Ok(row.map(|(id, sha, existed, turn, created)| {
        json!({
            "id": id,
            "path": path,
            "blobSha": sha,
            "existedPre": existed != 0,
            "turnId": turn,
            "createdAt": created,
        })
    }))
}

/// The bytes to write back. `None` with `existedPre = false` means delete.
pub fn body_for(conn: &Connection, sha: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("select body from file_blobs where sha = ?1", [sha], |r| {
            r.get::<_, Option<String>>(0)
        })
        .optional()?
        .flatten())
}

/// Drop blobs no checkpoint references and that are older than `age_days`.
pub fn gc_blobs(conn: &Connection, age_days: i64, max_rows: i64) -> Result<usize> {
    let n = conn.execute(
        "delete from file_blobs
          where sha in (
            select b.sha from file_blobs b
             where not exists (select 1 from file_checkpoints c where c.blob_sha = b.sha)
               and b.created_at < datetime('now', ?1)
             limit ?2
          )",
        params![format!("-{age_days} days"), max_rows],
    )?;
    Ok(n)
}

/// Checkpoints for a session, newest first. The diff screen reads this to show
/// what a turn changed.
pub fn list_checkpoints(conn: &Connection, session_id: &str) -> Result<Vec<Value>> {
    let mut st = conn.prepare(
        "select id, path, blob_sha, existed_pre, turn_id, created_at
           from file_checkpoints where session_id = ?1
          order by created_at desc, id desc",
    )?;
    let rows = st.query_map([session_id], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "path": r.get::<_, String>(1)?,
            "blobSha": r.get::<_, Option<String>>(2)?,
            "existedPre": r.get::<_, i64>(3)? != 0,
            "turnId": r.get::<_, Option<i64>>(4)?,
            "createdAt": r.get::<_, String>(5)?,
        }))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Every checkpoint a turn recorded, newest first: (path, blob_sha, existed_pre).
pub fn for_turn(
    conn: &Connection,
    session_id: &str,
    turn_id: i64,
) -> Result<Vec<(String, Option<String>, bool)>> {
    let mut st = conn.prepare(
        "select path, blob_sha, existed_pre from file_checkpoints
          where session_id = ?1 and turn_id = ?2
          order by created_at desc, id desc",
    )?;
    let rows = st.query_map(params![session_id, turn_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, i64>(2)? != 0,
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
