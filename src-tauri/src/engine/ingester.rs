// Transcript ingest: the engine writes the .jsonl, the host tails it into the
// index. The transcript stays the source of truth (recon), the DB is derived.
//
// Idempotency is the whole contract here: every insert is keyed on the
// transcript UUID and does nothing on conflict, and the byte offset is
// re-derived from the last parsed UUID at startup rather than trusted.

use anyhow::Result;
use rusqlite::Connection;
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::db::projects_dir;
use crate::db::{Db, Scope};
use std::sync::Arc;

pub fn slug(path: &Path) -> String {
    // Normalise the way the engine does (it canonicalises its cwd). On Windows
    // canonicalize yields a verbatim path, and its slug begins with the extra
    // prefix that produces. Slugs must be identical on both sides or the host
    // tails one directory while the engine writes another.
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical
        .to_string_lossy()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Locate a project's transcript for a session id.
pub fn transcript_path(cwd: &Path, session_id: &str) -> PathBuf {
    projects_dir().join(slug(cwd)).join(format!("{session_id}.jsonl"))
}

/// The in-memory read cursor for one file. It is not a table: recon forbids a
/// second source of truth for stream position, and it is rebuilt from the
/// file on startup.
#[derive(Default, Clone, Copy)]
pub struct Offset {
    pub bytes: u64,
}

/// Re-derive the byte offset by scanning for the last entry whose uuid is
/// already in `messages`. Returns the offset just past that line.
pub fn recover_offset(conn: &Connection, path: &Path, session_id: &str) -> Result<Offset> {
    let f = std::fs::File::open(path)?;
    let mut reader = BufReader::new(f);
    let mut pos: u64 = 0;
    let mut last: u64 = 0;
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        pos += n as u64;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            last = pos;
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            last = pos;
            continue;
        };
        let Some(uuid) = v.get("uuid").and_then(Value::as_str) else {
            last = pos;
            continue;
        };
        if entry_exists(conn, session_id, uuid)? {
            last = pos;
        } else {
            break;
        }
    }

    Ok(Offset { bytes: last })
}

fn entry_exists(conn: &Connection, session_id: &str, uuid: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "select count(*) from messages where id = ?1 and session_id = ?2",
        [uuid, session_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Index one live line the host just read off the engine's stdout.
///
/// The transcript file is written by the engine, but the GUI must not wait for
/// a file-system tick to show a message: the same line arrives on the pipe
/// first and is indexed here. `insert_message` is idempotent on the UUID, so
/// the tailer re-reading that line later changes nothing.
pub async fn index_live_line(
    db: &Arc<Db>,
    project_id: &str,
    session_id: &str,
    v: &Value,
) -> Result<bool> {
    let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
    match ty {
        "user" | "assistant" => {
            let scope = Scope::write_all(Scope::read(project_id.to_string()));
            crate::engine::watch::index_line(db, &scope, session_id, v).await
        }
        _ => Ok(false),
    }
}

/// Record the bytes a file had before a turn is allowed to edit it (S08).
/// Content-addressed: the same body written in ten turns is one blob.
pub fn record_checkpoint(
    conn: &Connection,
    session_id: &str,
    turn_id: Option<i64>,
    path: &str,
    body: Option<&[u8]>,
) -> Result<()> {
    use rusqlite::params;

    let id = format!("ck-{}", uuid::Uuid::new_v4().simple());
    let sha: Option<String> = match body {
        Some(bytes) => {
            let sha = crate::db::checkpoints::sha256_bytes(bytes);
            let text = String::from_utf8(bytes.to_vec()).ok();
            conn.execute(
                "insert into file_blobs (sha, size, binary, body)
                 values (?1, ?2, ?3, ?4)
                 on conflict (sha) do nothing",
                params![
                    sha,
                    bytes.len() as i64,
                    if text.is_none() { 1 } else { 0 },
                    text,
                ],
            )?;
            Some(sha)
        }
        None => None,
    };
    conn.execute(
        "insert into file_checkpoints (id, session_id, turn_id, path, blob_sha, existed_pre, created_at)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            id,
            session_id,
            turn_id,
            path,
            sha,
            if body.is_some() { 1 } else { 0 },
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        ],
    )?;
    Ok(())
}

/// Every transcript on disk for a project path, newest first. The resume picker
/// lists these; the index may not have a row for a transcript yet, and a file
/// the engine wrote is still resumable without one.
pub fn list_transcripts(cwd: &std::path::Path) -> Vec<serde_json::Value> {
    let dir = projects_dir().join(slug(cwd));
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<(std::time::SystemTime, serde_json::Value)> = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(id) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let meta = match std::fs::metadata(&p) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let when = chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339();
        out.push((
            modified,
            serde_json::json!({
                "sessionId": id,
                "path": p.to_string_lossy(),
                "bytes": meta.len(),
                "updatedAt": when,
            }),
        ));
    }
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().map(|(_, v)| v).collect()
}

/// Live turn bookkeeping for one session.
///
/// A turn is opened the moment the host writes a prompt to the engine's stdin,
/// and closed by the `result` that follows it. The row is created here rather
/// than reconstructed from the transcript file, so a crash between the write
/// and the first flushed line still leaves the prompt recorded.
pub struct LiveIndex {
    db: Arc<Db>,
    project_id: String,
    session_id: String,
    /// Turns opened but not yet closed, in FIFO order. A `result` closes the
    /// oldest one: prompts run in send order, so pairing must not assume the
    /// queue only ever held the last prompt.
    open: std::sync::Mutex<std::collections::VecDeque<i64>>,
}

impl LiveIndex {
    pub fn new(db: Arc<Db>, project_id: &str, session_id: &str) -> Arc<Self> {
        Arc::new(Self {
            db,
            project_id: project_id.to_string(),
            session_id: session_id.to_string(),
            open: std::sync::Mutex::new(std::collections::VecDeque::new()),
        })
    }

    fn scope(&self) -> Scope {
        Scope::write_all(Scope::read(self.project_id.clone()))
    }

    /// One prompt was written to the engine: open its turn row.
    pub fn prompt_sent(&self, text: &str, mode: &str, effort: &str, model: &str) {
        let scope = self.scope();
        let id = self.db.write(&scope, |tx| {
            Ok(crate::db::history::open_turn(
                tx,
                &self.session_id,
                text,
                if mode.is_empty() { None } else { Some(mode) },
                if effort.is_empty() { None } else { Some(effort) },
                if model.is_empty() { None } else { Some(model) },
            )?)
        });
        if let Ok(id) = id {
            if let Ok(mut q) = self.open.lock() {
                q.push_back(id);
            }
        }
    }

    /// A line from the engine's stdout. Message-shaped lines are indexed
    /// immediately; the tailer sees the same UUID later and does nothing.
    pub async fn line(&self, v: &Value) {
        let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
        let scope = self.scope();
        if matches!(ty, "user" | "assistant") {
            let _ = crate::engine::watch::index_line(&self.db, &scope, &self.session_id, v).await;
        }
        if matches!(ty, "permission_request" | "question" | "file_checkpoint") {
            let turn_id = self.open.lock().ok().and_then(|q| q.front().copied());
            crate::engine::watch::index_side_record(&self.db, &scope, &self.session_id, ty, v, turn_id);
        }
        if ty == "system" && v.get("subtype").and_then(Value::as_str) == Some("informational") {
            if let Some(agent_id) = v.get("agent_id").and_then(Value::as_str) {
                let status = v.get("status").and_then(Value::as_str);
                let _ = self.db.write(&scope, |tx| {
                    if let Some(status) = status {
                        crate::db::accounting::finish_subagent(
                            tx,
                            agent_id,
                            status,
                            v.get("result").and_then(Value::as_str),
                            v.get("cost_micros").and_then(Value::as_i64).unwrap_or(0),
                        )?;
                    } else {
                        crate::db::accounting::insert_subagent(
                            tx,
                            agent_id,
                            &self.session_id,
                            v.get("parent_tool_use_id").and_then(Value::as_str),
                            v.get("agent_type").and_then(Value::as_str).unwrap_or("general-purpose"),
                            v.get("agent_name").and_then(Value::as_str),
                            v.get("message").and_then(Value::as_str).unwrap_or(""),
                            v.get("model").and_then(Value::as_str),
                            v.get("isolation").and_then(Value::as_str),
                            v.get("worktree").and_then(Value::as_str),
                        )?;
                    }
                    Ok(())
                });
            }
        }
        if ty == "result" {
            self.close_turn(v);
        }
    }

    fn close_turn(&self, r: &Value) {
        let Some(id) = self.open.lock().ok().and_then(|mut q| q.pop_front()) else {
            return;
        };
        let subtype = r.get("subtype").and_then(Value::as_str).unwrap_or("success");
        let status = if subtype == "success" { "done" } else { "error" };
        let micros = r
            .get("total_cost_micros")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let scope = self.scope();
        let _ = self.db.write(&scope, |tx| {
            crate::db::history::close_turn(tx, id, status, micros)?;
            let usage = r.get("usage").cloned().unwrap_or_else(|| serde_json::json!({}));
            let model = r
                .get("model")
                .and_then(Value::as_str)
                .filter(|m| !m.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| "unknown".to_string());
            let row = crate::db::accounting::UsageRow {
                model,
                input_tokens: usage.get("input_tokens").and_then(Value::as_i64).unwrap_or(0),
                cache_write_tokens: usage
                    .get("cache_creation_input_tokens")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                cache_read_tokens: usage
                    .get("cache_read_input_tokens")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                output_tokens: usage.get("output_tokens").and_then(Value::as_i64).unwrap_or(0),
                cost_micros: micros,
            };
            crate::db::accounting::insert_usage(tx, Some(&self.session_id), Some(id), &row)?;
            crate::db::history::touch_session(tx, &self.session_id)?;
            Ok(())
        });
    }
}
