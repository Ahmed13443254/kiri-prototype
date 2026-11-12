// Accounting and subagents.
//
// Money is integer micro-USD, never a float (architecture.md). A turn can cost
// a fraction of a cent; cents would round it to zero, which is the one number
// this app must never lie about.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub struct UsageRow {
    pub model: String,
    pub input_tokens: i64,
    pub cache_write_tokens: i64,
    pub cache_read_tokens: i64,
    pub output_tokens: i64,
    pub cost_micros: i64,
}

pub fn insert_usage(
    conn: &Connection,
    session_id: Option<&str>,
    turn_id: Option<i64>,
    u: &UsageRow,
) -> Result<()> {
    conn.execute(
        "insert into usage
           (session_id, turn_id, model, input_tokens, cache_write_tokens,
            cache_read_tokens, output_tokens, cost_micros, created_at)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            session_id,
            turn_id,
            u.model,
            u.input_tokens,
            u.cache_write_tokens,
            u.cache_read_tokens,
            u.output_tokens,
            u.cost_micros,
            now()
        ],
    )?;
    Ok(())
}

/// Per-turn, per-session, per-project rollup. The GUI's cost panel reads this,
/// never a client-side sum of floats.
pub fn cost_rollup(conn: &Connection, project_id: &str, since: Option<&str>) -> Result<Value> {
    let (per_turn_sql, per_session_sql, project_sql) = if since.is_some() {
        (
            "select u.turn_id, coalesce(sum(u.cost_micros),0) as micros
               from usage u join sessions s on s.id = u.session_id
              where s.project_id = ?1 and u.created_at >= ?2 group by u.turn_id",
            "select u.session_id, coalesce(sum(u.cost_micros),0) as micros
               from usage u join sessions s on s.id = u.session_id
              where s.project_id = ?1 and u.created_at >= ?2 group by u.session_id",
            "select coalesce(sum(u.cost_micros),0) as micros
               from usage u join sessions s on s.id = u.session_id
              where s.project_id = ?1 and u.created_at >= ?2",
        )
    } else {
        (
            "select u.turn_id, coalesce(sum(u.cost_micros),0) as micros
               from usage u join sessions s on s.id = u.session_id
              where s.project_id = ?1 group by u.turn_id",
            "select u.session_id, coalesce(sum(u.cost_micros),0) as micros
               from usage u join sessions s on s.id = u.session_id
              where s.project_id = ?1 group by u.session_id",
            "select coalesce(sum(u.cost_micros),0) as micros
               from usage u join sessions s on s.id = u.session_id
              where s.project_id = ?1",
        )
    };

    let mut turns = Vec::new();
    {
        let mut st = conn.prepare(per_turn_sql)?;
        let mut q = match since {
            Some(s) => st.query(params![project_id, s])?,
            None => st.query(params![project_id])?,
        };
        while let Some(r) = q.next()? {
            turns.push(json!({
                "turnId": r.get::<_, Option<i64>>(0)?,
                "costMicros": r.get::<_, i64>(1)?,
            }));
        }
    }

    let mut sessions = Vec::new();
    {
        let mut st = conn.prepare(per_session_sql)?;
        let mut q = match since {
            Some(s) => st.query(params![project_id, s])?,
            None => st.query(params![project_id])?,
        };
        while let Some(r) = q.next()? {
            sessions.push(json!({
                "sessionId": r.get::<_, String>(0)?,
                "costMicros": r.get::<_, i64>(1)?,
            }));
        }
    }

    let project: i64 = match since {
        Some(s) => conn.query_row(project_sql, params![project_id, s], |r| r.get(0))?,
        None => conn.query_row(project_sql, params![project_id], |r| r.get(0))?,
    };

    Ok(json!({
        "projectMicros": project,
        "sessions": sessions,
        "turns": turns,
    }))
}

pub fn insert_subagent(
    conn: &Connection,
    id: &str,
    session_id: &str,
    parent_call_id: Option<&str>,
    agent_type: &str,
    agent_name: Option<&str>,
    prompt: &str,
    model: Option<&str>,
    isolation: Option<&str>,
    worktree: Option<&str>,
) -> Result<()> {
    conn.execute(
        "insert into subagents
           (id, session_id, parent_call_id, agent_type, agent_name, prompt,
            model, isolation, status, worktree, created_at)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'running', ?9, ?10)
         on conflict (id) do nothing",
        params![
            id,
            session_id,
            parent_call_id,
            agent_type,
            agent_name,
            prompt,
            model,
            isolation,
            worktree,
            now()
        ],
    )?;
    Ok(())
}

pub fn finish_subagent(
    conn: &Connection,
    id: &str,
    status: &str,
    result: Option<&str>,
    cost_micros: i64,
) -> Result<()> {
    conn.execute(
        "update subagents set status = ?1, result = ?2, cost_micros = ?3, ended_at = ?4 where id = ?5",
        params![status, result, cost_micros, now(), id],
    )?;
    Ok(())
}

pub fn list_subagents(conn: &Connection, session_id: &str) -> Result<Vec<Value>> {
    let mut st = conn.prepare(
        "select id, parent_call_id, agent_type, agent_name, model, isolation, status,
                result, worktree, cost_micros, created_at, ended_at
           from subagents where session_id = ?1 order by created_at asc",
    )?;
    let rows = st.query_map([session_id], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "parentCallId": r.get::<_, Option<String>>(1)?,
            "agentType": r.get::<_, String>(2)?,
            "agentName": r.get::<_, Option<String>>(3)?,
            "model": r.get::<_, Option<String>>(4)?,
            "isolation": r.get::<_, Option<String>>(5)?,
            "status": r.get::<_, String>(6)?,
            "result": r.get::<_, Option<String>>(7)?,
            "worktree": r.get::<_, Option<String>>(8)?,
            "costMicros": r.get::<_, i64>(9)?,
            "createdAt": r.get::<_, String>(10)?,
            "endedAt": r.get::<_, Option<String>>(11)?,
        }))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
