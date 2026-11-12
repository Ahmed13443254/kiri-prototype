// transcript.watch: tail the engine's .jsonl transcripts into the index.
//
// The engine writes the file and flushes each line; the host reads it. The
// transcript is the source of truth, so this job is recoverable from scratch:
// on startup it re-derives each byte offset from the last UUID already indexed
// and continues from there. There is no cursor table.
//
// A 1 s poll is the debounce (architecture.md). A burst of appends is one pass.

use std::collections::HashMap;
use std::io::{BufRead, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::db::history::now;
use crate::db::{transcript, Db, Scope};

pub struct Watcher {
    files: Mutex<HashMap<PathBuf, Cursor>>,
}

pub struct Cursor {
    pub offset: u64,
    pub session_id: String,
    pub project_id: String,
}

impl Watcher {
    pub fn new() -> Self {
        Self {
            files: Mutex::new(HashMap::new()),
        }
    }

    pub async fn register(&self, path: PathBuf, session_id: String, project_id: String, offset: u64) {
        self.files.lock().await.insert(
            path,
            Cursor { offset, session_id, project_id },
        );
    }

    pub async fn scan(&self, db: &Arc<Db>) -> Result<()> {
        let root = crate::db::projects_dir();
        let dirs = match std::fs::read_dir(&root) {
            Ok(d) => d,
            Err(_) => return Ok(()),
        };
        let scope = Scope::root();
        let mut discovered: Vec<(PathBuf, String, String, u64)> = Vec::new();

        for dir in dirs.flatten() {
            if !dir.path().is_dir() {
                continue;
            }
            let Ok(inner) = std::fs::read_dir(dir.path()) else {
                continue;
            };
            for f in inner.flatten() {
                let p = f.path();
                if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                    continue;
                }
                let session_id = p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();
                let project_id = db.read(&scope, |c| {
                    Ok(c.query_row(
                        "select project_id from sessions where id = ?1",
                        [&session_id],
                        |r| r.get::<_, String>(0),
                    )
                    .ok())
                })?;
                if let Some(pid) = project_id {
                    let bytes = db.read(&scope, |c| {
                        Ok(crate::engine::ingester::recover_offset(c, &p, &session_id)
                            .map(|o| o.bytes)
                            .unwrap_or(0))
                    })?;
                    discovered.push((p, session_id, pid, bytes));
                }
            }
        }

        let mut files = self.files.lock().await;
        for (p, s, pid, bytes) in discovered {
            files.insert(
                p,
                Cursor {
                    offset: bytes,
                    session_id: s,
                    project_id: pid,
                },
            );
        }
        Ok(())
    }

    pub async fn tick(&self, db: &Arc<Db>) -> Result<usize> {
        let snapshot: Vec<(PathBuf, String, String, u64)> = {
            let f = self.files.lock().await;
            f.iter()
                .map(|(p, c)| (p.clone(), c.session_id.clone(), c.project_id.clone(), c.offset))
                .collect()
        };

        let mut added = 0usize;
        let mut advances: Vec<(PathBuf, u64)> = Vec::new();

        for (path, session_id, project_id, offset) in snapshot {
            if !path.exists() {
                continue;
            }
            let (n, new_offset) = index_file(db, &path, &session_id, &project_id, offset).await?;
            added += n;
            if new_offset != offset {
                advances.push((path, new_offset));
            }
        }

        let mut f = self.files.lock().await;
        for (path, off) in advances {
            if let Some(c) = f.get_mut(&path) {
                c.offset = off;
            }
        }
        Ok(added)
    }
}


/// Read lines past `offset` and insert them. An insert that hits a row the
/// index already has is a no-op, which is what makes a double read harmless.
pub async fn index_file(
    db: &Arc<Db>,
    path: &Path,
    session_id: &str,
    project_id: &str,
    offset: u64,
) -> Result<(usize, u64)> {
    let f = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(f);
    reader.seek(SeekFrom::Start(offset))?;

    let scope = Scope::write_all(Scope::read(project_id.to_string()));
    let mut added = 0usize;
    let mut pos = offset;
    let mut line = String::new();

    while reader.read_line(&mut line)? > 0 {
        pos += line.len() as u64;
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
                if index_line(db, &scope, session_id, &v).await? {
                    added += 1;
                }
            }
        }
        line.clear();
    }
    Ok((added, pos))
}

pub async fn index_line(db: &Arc<Db>, scope: &Scope, session_id: &str, v: &Value) -> Result<bool> {
    let ty = v.get("type").and_then(Value::as_str).unwrap_or("");
    // Not only conversation entries: the engine's side records are what makes
    // S06, S08 and S10 show the truth instead of an empty table. Each branch is
    // keyed so a line seen twice is still one row.
    match ty {
        "permission_request" | "question" | "file_checkpoint" => {
            index_side_record(db, scope, session_id, ty, v, None);
            return Ok(false);
        }
        "user" | "assistant" => {}
        _ => return Ok(false),
    }
    let uuid = v
        .get("uuid")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if uuid.is_empty() {
        return Ok(false);
    }
    let created = v
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let msg = v.get("message").cloned().unwrap_or(Value::Null);
    let role = msg
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or(ty)
        .to_string();
    let message_id = msg.get("id").and_then(Value::as_str).map(str::to_string);
    let parent = v.get("parentUuid").and_then(Value::as_str).map(str::to_string);
    let sidechain = v
        .get("isSidechain")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let content_json = msg.to_string();
    let blocks: Vec<Value> = msg
        .get("content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let uuid2 = uuid.clone();
    let session2 = session_id.to_string();
    let created2 = created.clone();

    db.write(scope, |tx| {
        let inserted = transcript::insert_message(
            tx,
            &uuid2,
            &session2,
            None,
            parent.as_deref(),
            &role,
            message_id.as_deref(),
            sidechain,
            &content_json,
            &created2,
        )?;
        if !inserted {
            return Ok(false);
        }

        for (i, block) in blocks.iter().enumerate() {
            let kind = block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("other");
            let text = block
                .get("text")
                .or_else(|| block.get("thinking"))
                .and_then(Value::as_str);
            let tool_use_id = block
                .get("id")
                .or_else(|| block.get("tool_use_id"))
                .and_then(Value::as_str);
            let is_error = block
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            transcript::insert_block(
                tx,
                &uuid2,
                i as i64,
                kind,
                text,
                tool_use_id,
                is_error,
                Some(block),
            )?;
        }
        Ok(true)
    })
}

/// Start the 1 s transcript tail. Discovery runs once at startup; a tick that
/// sees a transcript for a live-but-unknown session adds it then.
pub fn spawn_watch(host: Arc<crate::host::Host>) {
    tauri::async_runtime::spawn(async move {
        let watcher = host.watcher.clone();
        if let Err(e) = watcher.scan(&host.db).await {
            eprintln!("[kiri] transcript scan: {e}");
        }

        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tick.tick().await;

        loop {
            tick.tick().await;
            if let Err(e) = watcher.tick(&host.db).await {
                eprintln!("[kiri] transcript watch: {e}");
            }
        }
    });
}

/// Engine side records -> index rows. These are not conversation messages, but
/// they are what the permission, diff and agents screens read, so they must be
/// persisted as the pipe yields them. Idempotent by their own unique keys.
pub(crate) fn index_side_record(db: &Arc<Db>, scope: &Scope, session_id: &str, ty: &str, v: &Value, turn_id: Option<i64>) {
    let _ = db.write(scope, |tx| {
        let turn_id = turn_id.or_else(|| {
            tx.query_row(
                "select id from turns where session_id = ?1 and status = 'running' order by ordinal desc limit 1",
                [session_id],
                |r| r.get::<_, i64>(0),
            ).ok()
        });
        match ty {
            "permission_request" => {
                let request_id = v.get("requestId").and_then(Value::as_str).unwrap_or("");
                let tool = v.get("toolName").and_then(Value::as_str).unwrap_or("tool");
                let input = v.get("input").cloned().unwrap_or_else(|| json!({}));
                if !request_id.is_empty() {
                    // There is no tool_calls row yet at prompt time: the call is
                    // recorded when its result arrives. The schema wants a
                    // non-null tool_call_id, so the request owns a synthetic one
                    // keyed to the same request_id.
                    let call_id = format!("call-{request_id}");
                    let _ = transcript::insert_tool_call(
                        tx,
                        &call_id,
                        session_id,
                        None,
                        None,
                        None,
                        tool,
                        &input,
                        "pending",
                        &now(),
                    );
                    let _ = crate::db::decisions::record_request(
                        tx,
                        session_id,
                        turn_id,
                        &call_id,
                        request_id,
                        tool,
                        &input,
                        None,
                        None,
                        false,
                        None,
                    );
                }
            }
            "question" => {
                let request_id = v.get("requestId").and_then(Value::as_str).unwrap_or("");
                let payload = v.get("payload").cloned().unwrap_or(Value::Null);
                if !request_id.is_empty() {
                    let _ = crate::db::decisions::record_question(tx, session_id, request_id, &payload);
                }
            }
            "file_checkpoint" => {
                let path = v.get("path").and_then(Value::as_str).unwrap_or("");
                let body = v.get("body_base64").and_then(Value::as_str);
                if !path.is_empty() {
                    let bytes = body.and_then(|b| {
                        use base64::Engine as _;
                        base64::engine::general_purpose::STANDARD.decode(b).ok()
                    });
                    let _ = crate::engine::ingester::record_checkpoint(
                        tx,
                        session_id,
                        turn_id,
                        path,
                        bytes.as_deref(),
                    );
                }
            }
            _ => {}
        }
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{history, projects};

    /// The fixture is a real transcript captured from a live engine session
    /// (tests/probe_side.py, session `side-records-1`). Indexing it must land
    /// one row in each side table — the data S05, S07 and S08 read — and a
    /// second tick from the returned offset must change nothing.
    #[tokio::test]
    async fn side_records_reach_the_index() {
        let dir = std::env::temp_dir().join("kiri-side-proof");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = dir.join("index.sqlite3");
        let db = Db::open(&db_path).expect("open + migrate");

        let root = Scope::root();
        let project = db
            .read(&root, |c| {
                Ok(projects::open_project(c, "C:/kiri-side-proof", "kiri-side-proof")?)
            })
            .expect("project");
        let scope = Scope::write_all(Scope::read(project.id.clone()));

        let transcript = dir.join("side-records-1.jsonl");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/side-records-1.jsonl"),
            &transcript,
        )
        .expect("fixture copy");

        db.write(&scope, |tx| {
            history::upsert_session(
                tx,
                &project.id,
                None,
                "side-records-1",
                None,
                "fake",
                Some("high"),
                "default",
                transcript.to_str().unwrap(),
                None,
                None,
            )
        })
        .expect("session row");

        let (added, offset) = index_file(&db, &transcript, "side-records-1", &project.id, 0)
            .await
            .expect("index pass 1");
        assert_eq!(added, 6, "six message lines expected");

        let (added2, offset2) =
            index_file(&db, &transcript, "side-records-1", &project.id, offset)
                .await
                .expect("index pass 2");
        assert_eq!(added2, 0, "a re-tick must add nothing");
        assert_eq!(offset2, offset);

        let (perm, question, checkpoint, messages) = db
            .read(&scope, |c| {
                let perm: (String, String, String, String) = c.query_row(
                    "select request_id, tool_name, status, tool_call_id from permission_requests",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )?;
                let question: (String, String) = c.query_row(
                    "select request_id, payload from questions",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let checkpoint: (String, i64, Option<String>) = c.query_row(
                    "select path, existed_pre, blob_sha from file_checkpoints",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )?;
                let messages: i64 =
                    c.query_row("select count(*) from messages", [], |r| r.get(0))?;
                Ok((perm, question, checkpoint, messages))
            })
            .expect("each side table must hold its row");

        assert_eq!(perm.0, "perm-6341e432811f4417abc4e90186df9162");
        assert_eq!(perm.1, "Write");
        assert_eq!(perm.2, "pending");
        assert_eq!(perm.3, format!("call-{}", perm.0));
        assert_eq!(question.0, "q-8c7afdab555a444aadef915b5372725a");
        assert!(question.1.contains("which?"), "payload: {}", question.1);
        assert!(checkpoint.0.ends_with("n.txt"), "path: {}", checkpoint.0);
        assert_eq!(checkpoint.1, 0, "file did not exist before the write");
        assert_eq!(checkpoint.2, None, "no body means no blob");
        assert_eq!(messages, 6);

        db.read(&scope, |c| {
            let n: i64 = c.query_row(
                "select count(*) from tool_calls where id = ?1",
                [&perm.3],
                |r| r.get(0),
            )?;
            assert_eq!(n, 1, "the request's synthetic tool call must exist");
            Ok(())
        })
        .unwrap();

        db.checkpoint().ok();
        println!("proof db kept at: {}", db_path.display());
    }
}
