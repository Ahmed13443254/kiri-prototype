// Messages and blocks: the transcript index.
//
// Idempotent (architecture.md, "Idempotency"): every message insert is
// `insert ... on conflict (id) do nothing` keyed on the transcript UUID, and the
// watcher keeps a per-file byte offset in memory that it re-derives from the
// last parsed UUID at startup. Seeing a line twice changes nothing.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

pub fn insert_message(
    conn: &Connection,
    id: &str,
    session_id: &str,
    turn_id: Option<i64>,
    parent_uuid: Option<&str>,
    role: &str,
    message_id: Option<&str>,
    is_sidechain: bool,
    content: &str,
    created_at: &str,
) -> Result<bool> {
    let n = conn.execute(
        "insert into messages
           (id, session_id, turn_id, parent_uuid, role, message_id,
            is_sidechain, content, created_at)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         on conflict (id) do nothing",
        params![
            id,
            session_id,
            turn_id,
            parent_uuid,
            role,
            message_id,
            if is_sidechain { 1 } else { 0 },
            content,
            created_at,
        ],
    )?;
    Ok(n > 0)
}

/// Blocks are unique per (message, idx): a re-index of the same message is a
/// no-op, and a partial line never produces a second block 0.
pub fn insert_block(
    conn: &Connection,
    message_id: &str,
    idx: i64,
    kind: &str,
    text: Option<&str>,
    tool_use_id: Option<&str>,
    is_error: bool,
    json: Option<&Value>,
) -> Result<bool> {
    let n = conn.execute(
        "insert into blocks (message_id, idx, kind, text, tool_use_id, is_error, json)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         on conflict (message_id, idx) do nothing",
        params![
            message_id,
            idx,
            kind,
            text,
            tool_use_id,
            if is_error { 1 } else { 0 },
            json.map(|v| v.to_string()),
        ],
    )?;
    Ok(n > 0)
}

pub fn insert_tool_call(
    conn: &Connection,
    id: &str,
    session_id: &str,
    turn_id: Option<i64>,
    parent_tool_use_id: Option<&str>,
    agent_id: Option<&str>,
    name: &str,
    input: &Value,
    status: &str,
    created_at: &str,
) -> Result<()> {
    conn.execute(
        "insert into tool_calls
           (id, session_id, turn_id, parent_tool_use_id, agent_id, name,
            input, status, created_at)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         on conflict (id) do update set
           status = excluded.status,
           input  = coalesce(excluded.input, tool_calls.input)",
        params![
            id,
            session_id,
            turn_id,
            parent_tool_use_id,
            agent_id,
            name,
            input.to_string(),
            status,
            created_at,
        ],
    )?;
    Ok(())
}

pub fn finish_tool_call(
    conn: &Connection,
    id: &str,
    result: &Value,
    status: &str,
    duration_ms: Option<i64>,
) -> Result<()> {
    conn.execute(
        "update tool_calls set result = ?1, status = ?2,
           duration_ms = coalesce(?3, duration_ms),
           ended_at = ?4
         where id = ?5",
        params![
            result.to_string(),
            status,
            duration_ms,
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            id
        ],
    )?;
    Ok(())
}

/// Transcript page for the GUI. Ordered by rowid so two messages written in the
/// same millisecond keep their write order.
pub fn read_messages(conn: &Connection, session_id: &str, limit: i64, before_rowid: Option<i64>) -> Result<Vec<Value>> {
    let mut st = match before_rowid {
        Some(_) => conn.prepare(
            "select rowid, id, turn_id, parent_uuid, role, message_id, is_sidechain, content, created_at
             from messages where session_id = ?1 and rowid < ?2
             order by rowid desc limit ?3",
        )?,
        None => conn.prepare(
            "select rowid, id, turn_id, parent_uuid, role, message_id, is_sidechain, content, created_at
             from messages where session_id = ?1
             order by rowid desc limit ?2",
        )?,
    };

    let mut rows = Vec::new();
    if let Some(before) = before_rowid {
        let mut q = st.query(params![session_id, before, limit])?;
        while let Some(r) = q.next()? {
            rows.push(row_message(conn, r)?);
        }
    } else {
        let mut q = st.query(params![session_id, limit])?;
        while let Some(r) = q.next()? {
            rows.push(row_message(conn, r)?);
        }
    }
    rows.reverse();
    Ok(rows)
}

fn row_message(conn: &Connection, r: &rusqlite::Row) -> Result<Value> {
    let id: String = r.get(1)?;
    let blocks = read_blocks(conn, &id)?;
    Ok(json!({
        "rowid": r.get::<_, i64>(0)?,
        "id": id,
        "turnId": r.get::<_, Option<i64>>(2)?,
        "parentUuid": r.get::<_, Option<String>>(3)?,
        "role": r.get::<_, String>(4)?,
        "messageId": r.get::<_, Option<String>>(5)?,
        "isSidechain": r.get::<_, i64>(6)? != 0,
        "createdAt": r.get::<_, String>(8)?,
        "blocks": blocks,
    }))
}

pub fn read_blocks(conn: &Connection, message_id: &str) -> Result<Vec<Value>> {
    let mut st = conn.prepare(
        "select idx, kind, text, tool_use_id, is_error, json
         from blocks where message_id = ?1 order by idx asc",
    )?;
    let rows = st.query_map([message_id], |r| {
        Ok(json!({
            "idx": r.get::<_, i64>(0)?,
            "kind": r.get::<_, String>(1)?,
            "text": r.get::<_, Option<String>>(2)?,
            "toolUseId": r.get::<_, Option<String>>(3)?,
            "isError": r.get::<_, i64>(4)? != 0,
            "json": r.get::<_, Option<String>>(5)?
                .and_then(|s| serde_json::from_str::<Value>(&s).ok()),
        }))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn count_messages(conn: &Connection, session_id: &str) -> Result<i64> {
    Ok(conn.query_row(
        "select count(*) from messages where session_id = ?1",
        [session_id],
        |r| r.get(0),
    )?)
}
