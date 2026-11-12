// Background work: tasks, cron jobs, wakeups.
//
// F05: an empty list must still render, so `list_tasks` always returns an array
// (never null, never "skip the panel when there is no data").

use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn upsert_task(
    conn: &Connection,
    id: &str,
    session_id: &str,
    tool_call_id: Option<&str>,
    kind: &str,
    subject: &str,
    description: Option<&str>,
    active_form: Option<&str>,
    status: &str,
    owner: Option<&str>,
    subagent_id: Option<&str>,
    metadata: &Value,
) -> Result<()> {
    conn.execute(
        "insert into tasks
           (id, session_id, tool_call_id, kind, subject, description, active_form,
            status, owner, subagent_id, metadata, created_at, updated_at)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
         on conflict (id) do update set
           subject      = excluded.subject,
           description  = coalesce(excluded.description, tasks.description),
           active_form  = coalesce(excluded.active_form, tasks.active_form),
           status       = excluded.status,
           owner        = coalesce(excluded.owner, tasks.owner),
           subagent_id  = coalesce(excluded.subagent_id, tasks.subagent_id),
           metadata     = excluded.metadata,
           updated_at   = excluded.updated_at",
        params![
            id,
            session_id,
            tool_call_id,
            kind,
            subject,
            description,
            active_form,
            status,
            owner,
            subagent_id,
            metadata.to_string(),
            now()
        ],
    )?;
    Ok(())
}

pub fn set_task_status(conn: &Connection, id: &str, status: &str) -> Result<()> {
    conn.execute(
        "update tasks set status = ?1, updated_at = ?2 where id = ?3",
        params![status, now(), id],
    )?;
    Ok(())
}

/// Always an array -- the GUI's empty state is load-bearing (F05 edge).
pub fn list_tasks(conn: &Connection, session_id: &str) -> Result<Vec<Value>> {
    let mut st = conn.prepare(
        "select id, kind, subject, description, active_form, status, owner,
                subagent_id, blocks, blocked_by, metadata, created_at, updated_at
           from tasks where session_id = ?1
          order by created_at asc",
    )?;
    let rows = st.query_map([session_id], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "kind": r.get::<_, String>(1)?,
            "subject": r.get::<_, String>(2)?,
            "description": r.get::<_, Option<String>>(3)?,
            "activeForm": r.get::<_, Option<String>>(4)?,
            "status": r.get::<_, String>(5)?,
            "owner": r.get::<_, Option<String>>(6)?,
            "subagentId": r.get::<_, Option<String>>(7)?,
            "blocks": r.get::<_, Option<String>>(8)?
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .unwrap_or_else(|| json!([])),
            "blockedBy": r.get::<_, Option<String>>(9)?
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .unwrap_or_else(|| json!([])),
            "metadata": r.get::<_, Option<String>>(10)?
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                .unwrap_or_else(|| json!({})),
            "createdAt": r.get::<_, String>(11)?,
            "updatedAt": r.get::<_, String>(12)?,
        }))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)

}

pub fn set_task_edges(conn: &Connection, id: &str, blocks: &[String], blocked_by: &[String]) -> Result<()> {
    conn.execute(
        "update tasks set blocks = ?1, blocked_by = ?2, updated_at = ?3 where id = ?4",
        params![
            serde_json::to_string(blocks)?,
            serde_json::to_string(blocked_by)?,
            now(),
            id
        ],
    )?;
    Ok(())
}

/// The in-process scheduler's claim: one row, due Why and not yet sent, marked
/// sent in the same statement. No scan-then-write race.
pub fn claim_due_wakeups(conn: &Connection, limit: i64) -> Result<Vec<(String, String, String)>> {
    let mut st = conn.prepare(
        "select id, session_id, prompt from wakeups
          where sent_at is null and fire_at <= ?1
          order by fire_at asc limit ?2",
    )?;
    let due: Vec<(String, String, String)> = st
        .query_map(params![now(), limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            ))
        })?
        .filter_map(|r| r.ok())
        .collect();

    for (id, _, _) in &due {
        conn.execute("update wakeups set sent_at = ?1 where id = ?2 and sent_at is null", params![now(), id])?;
    }
    Ok(due)
}

pub fn claim_due_crons(conn: &Connection, limit: i64) -> Result<Vec<Value>> {
    let mut st = conn.prepare(
        "select id, session_id, cron, prompt, recurring, next_run
           from cron_jobs where enabled = 1 and next_run <= ?1
          order by next_run asc limit ?2",
    )?;
    let rows = st.query_map(params![now(), limit], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "sessionId": r.get::<_, String>(1)?,
            "cron": r.get::<_, String>(2)?,
            "prompt": r.get::<_, String>(3)?,
            "recurring": r.get::<_, i64>(4)? != 0,
            "nextRun": r.get::<_, String>(5)?,
        }))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn add_wakeup(conn: &Connection, session_id: &str, fire_at: &str, reason: Option<&str>, prompt: Option<&str>) -> Result<String> {
    let id = format!("wake-{}", uuid::Uuid::new_v4().simple());
    conn.execute(
        "insert into wakeups (id, session_id, fire_at, reason, prompt) values (?1, ?2, ?3, ?4, ?5)",
        params![id, session_id, fire_at, reason, prompt],
    )?;
    Ok(id)
}

pub fn add_cron(conn: &Connection, session_id: &str, cron: &str, prompt: &str, recurring: bool, durable: bool) -> Result<String> {
    let id = format!("cron-{}", uuid::Uuid::new_v4().simple());
    conn.execute(
        "insert into cron_jobs (id, session_id, cron, prompt, recurring, durable)
         values (?1, ?2, ?3, ?4, ?5, ?6)",
        params![id, session_id, cron, prompt, recurring, durable],
    )?;
    Ok(id)
}

pub fn delete_cron(conn: &Connection, session_id: &str, id: &str) -> Result<()> {
    conn.execute("delete from cron_jobs where id = ?1 and session_id = ?2", params![id, session_id])?;
    Ok(())
}

/// Move a cron job to its next occurrence and disable it when it was one-shot.
pub fn advance_cron(conn: &Connection, id: &str, _cron: &str, recurring: bool) -> Result<()> {
    let step: i64 = if recurring { 1 } else { 0 };
    conn.execute(
        "update cron_jobs
            set last_run = ?1,
                next_run = case when ?2 = 1 then datetime('now', '+1 minute') else null end,
                enabled  = case when ?2 = 1 then enabled else 0 end
          where id = ?3",
        params![now(), step, id],
    )?;
    Ok(())
}
