// Sessions and turns. Identity comes from the transcript: a session row is an
// index entry pointing at its .jsonl, never the only copy of the conversation.
//
// Idempotency (architecture.md): the transcript watcher can see the same line
// twice, so message inserts are `on conflict (id) do nothing` and permission
// decisions are keyed on the unique `request_id`.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use super::{Capability, Scope, ScopeError};

pub fn require(scope: &Scope, cap: Capability) -> Result<(), ScopeError> {
    scope.require(cap)
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Create or update the session row for a freshly spawned engine.
pub fn upsert_session(
    conn: &Connection,
    project_id: &str,
    workspace_id: Option<&str>,
    id: &str,
    title: Option<&str>,
    model: &str,
    effort: Option<&str>,
    permission_mode: &str,
    transcript_path: &str,
    pid: Option<i64>,
    fork_of: Option<&str>,
) -> Result<()> {
    conn.execute(
        "insert into sessions
           (id, project_id, workspace_id, title, model, effort, permission_mode,
            status, transcript_path, pid, fork_of, created_at, updated_at)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active', ?8, ?9, ?10, ?11, ?11)
         on conflict (id) do update set
           workspace_id    = coalesce(excluded.workspace_id, sessions.workspace_id),
           title           = coalesce(excluded.title, sessions.title),
           model           = excluded.model,
           effort          = excluded.effort,
           permission_mode = excluded.permission_mode,
           transcript_path = excluded.transcript_path,
           pid             = excluded.pid,
           updated_at      = excluded.updated_at",
        params![
            id,
            project_id,
            workspace_id,
            title,
            model,
            effort,
            permission_mode,
            transcript_path,
            pid,
            fork_of,
            now()
        ],
    )?;
    Ok(())
}

pub fn set_session_status(conn: &Connection, id: &str, status: &str, pid: Option<i64>) -> Result<()> {
    conn.execute(
        "update sessions set status = ?1, pid = ?2, updated_at = ?3,
            ended_at = case when ?1 in ('done','error','interrupted') then ?3 else ended_at end
         where id = ?4",
        params![status, pid, now(), id],
    )?;
    Ok(())
}

pub fn set_session_model(conn: &Connection, id: &str, model: &str, effort: Option<&str>) -> Result<()> {
    conn.execute(
        "update sessions set model = ?1, effort = coalesce(?2, effort), updated_at = ?3 where id = ?4",
        params![model, effort, now(), id],
    )?;
    Ok(())
}

pub fn set_session_mode(conn: &Connection, id: &str, mode: &str) -> Result<()> {
    conn.execute(
        "update sessions set permission_mode = ?1, updated_at = ?2 where id = ?3",
        params![mode, now(), id],
    )?;
    Ok(())
}

pub fn touch_session(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "update sessions set updated_at = ?1, cost_micros =
            (select coalesce(sum(cost_micros),0) from turns where session_id = sessions.id)
         where id = ?2",
        params![now(), id],
    )?;
    Ok(())
}

/// Sessions for a project, newest first. The row stays even when the transcript
/// is missing: `missing_transcript` tells the GUI to open it read-only.
pub fn list_sessions(conn: &Connection, project_id: &str,) -> Result<Vec<Value>> {
    let mut st = conn.prepare(
        "select s.id, s.title, s.model, s.effort, s.permission_mode, s.status,
                s.transcript_path, s.pid, s.fork_of, s.cost_micros,
                s.created_at, s.updated_at,
                (select count(*) from turns t where t.session_id = s.id),
                (select coalesce(min(t.prompt_text), '')
                   from turns t where t.session_id = s.id and t.ordinal = 1)
         from sessions s
         where s.project_id = ?1
         order by coalesce(s.updated_at, s.created_at) desc",
    )?;
    let rows = st.query_map([project_id], |r| {
        // Column order matters: 12 is the turn count (integer), 13 is the
        // first prompt (text). Reading these the other way round made every
        // row fail and the list render empty.
        let transcript: String = r.get(6)?;
        let first_prompt: String = r.get(13)?;
        let turn_count: i64 = r.get(12)?;
        let title: Option<String> = r.get(1)?;
        let derived = title.unwrap_or_else(|| {
            let t = first_prompt.trim().replace('\n', " ");
            if t.is_empty() {
                "(untitled)".to_string()
            } else {
                t.chars().take(60).collect()
            }
        });
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "title": derived,
            "model": r.get::<_, String>(2)?,
            "effort": r.get::<_, Option<String>>(3)?,
            "permissionMode": r.get::<_, String>(4)?,
            "status": r.get::<_, String>(5)?,
            "transcriptPath": transcript,
            "missingTranscript": !std::path::Path::new(&transcript).exists(),
            "pid": r.get::<_, Option<i64>>(7)?,
            "forkOf": r.get::<_, Option<String>>(8)?,
            "costMicros": r.get::<_, i64>(9)?,
            "createdAt": r.get::<_, String>(10)?,
            "updatedAt": r.get::<_, String>(11)?,
            "turnCount": turn_count,
        }))
    })?;
    // A malformed row must fail the call, not vanish: an empty list here is
    // indistinguishable from "this project has no sessions".
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

pub fn get_session(conn: &Connection, project_id: &str, id: &str) -> Result<Option<Value>> {
        Ok(conn
        .query_row(
            "select id, title, model, effort, permission_mode, status, transcript_path,
                    cost_micros, created_at, updated_at, pid
             from sessions where project_id = ?1 and id = ?2",
            params![project_id, id],
            |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "title": r.get::<_, Option<String>>(1)?,
                    "model": r.get::<_, String>(2)?,
                    "effort": r.get::<_, Option<String>>(3)?,
                    "permissionMode": r.get::<_, String>(4)?,
                    "status": r.get::<_, String>(5)?,
                    "transcriptPath": r.get::<_, String>(6)?,
                    "costMicros": r.get::<_, i64>(7)?,
                    "createdAt": r.get::<_, String>(8)?,
                    "updatedAt": r.get::<_, String>(9)?,
                    "pid": r.get::<_, Option<i64>>(10)?,
                }))
            },
        )
        .optional()?)
}

pub fn delete_session(conn: &Connection, scope: &Scope, id: &str) -> Result<()> {
    scope.require_write()?;
    conn.execute(
        "delete from sessions where id = ?1 and project_id = ?2",
        params![id, scope.project_id],
    )?;
    Ok(())
}

/// A new turn is the next ordinal in this session. The engine owns the
/// transcript; the turn row just bookends it.
pub fn open_turn(
    conn: &Connection,
    session_id: &str,
    prompt_text: &str,
    mode: Option<&str>,
    effort: Option<&str>,
    model: Option<&str>,
) -> Result<i64> {
    let next: i64 = conn.query_row(
        "select coalesce(max(ordinal), 0) + 1 from turns where session_id = ?1",
        [session_id],
        |r| r.get(0),
    )?;
    conn.execute(
        "insert into turns (session_id, ordinal, prompt_id, status, mode, effort, model, prompt_text, started_at)
         values (?1, ?2, ?3, 'running', ?4, ?5, ?6, ?7, ?8)",
        params![
            session_id,
            next,
            format!("p-{}", uuid::Uuid::new_v4().simple()),
            mode,
            effort,
            model,
            prompt_text,
            now()
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn close_turn(
    conn: &Connection,
    turn_id: i64,
    status: &str,
    cost_micros: i64,
) -> Result<()> {
    conn.execute(
        "update turns set status = ?1, cost_micros = ?2, ended_at = ?3 where id = ?4",
        params![status, cost_micros, now(), turn_id],
    )?;
    Ok(())
}

pub fn bump_session_cost(conn: &Connection, session_id: &str) -> Result<()> {
    conn.execute(
        "update sessions set cost_micros =
           (select coalesce(sum(cost_micros),0) from turns where session_id = ?1),
         updated_at = ?2 where id = ?1",
        params![session_id, now()],
    )?;
    Ok(())
}

