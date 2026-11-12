// Permission requests and rules.
//
// Races (architecture.md): "the GUI answers a permission prompt at the same
// moment the engine times out the turn -> resolved rows are written where
// status = 'pending', and zero rows updated means 'too late', shown as such".
// resolve() therefore returns whether it won, and the caller must believe it.

use anyhow::Result;
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use super::{Capability, Scope};

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// Record wiped incoming prompt. Unique on request_id: a reattach that replays
/// the same request just finds the existing row.
pub fn record_request(
    conn: &Connection,
    session_id: &str,
    turn_id: Option<i64>,
    tool_call_id: &str,
    request_id: &str,
    tool_name: &str,
    input: &Value,
    suggestions: Option<&Value>,
    blocked_path: Option<&str>,
    default_no: bool,
    agent_id: Option<&str>,
) -> Result<()> {
    conn.execute(
        "insert into permission_requests
           (id, request_id, session_id, turn_id, tool_call_id, tool_name, input,
            suggestions, blocked_path, default_no, agent_id, status, created_at)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'pending', ?12)
         on conflict (request_id) do nothing",
        params![
            format!("pr-{}", uuid::Uuid::new_v4().simple()),
            request_id,
            session_id,
            turn_id,
            tool_call_id,
            tool_name,
            input.to_string(),
            suggestions.map(|v| v.to_string()),
            blocked_path,
            if default_no { 1 } else { 0 },
            agent_id,
            now()
        ],
    )?;
    Ok(())
}

/// Returns false when another writer already resolved this request ("too late").
pub fn resolve(
    conn: &Connection,
    request_id: &str,
    decision: &str,
    reason: Option<&str>,
) -> Result<bool> {
    let n = conn.execute(
        "update permission_requests
            set status = ?1, decision_reason = ?2, resolved_at = ?3
          where request_id = ?4 and status = 'pending'",
        params![decision, reason, now(), request_id],
    )?;
    Ok(n == 1)
}

pub fn mark_cancelled(conn: &Connection, id: &str) -> Result<u64> {
    Ok(conn.execute(
        "update permission_requests
            set status = 'cancelled', resolved_at = ?1
          where session_id = ?2 and status = 'pending'",
        params![now(), id],
    )? as u64)
}

pub fn pending_for_session(conn: &Connection, session_id: &str) -> Result<Vec<Value>> {
    let mut st = conn.prepare(
        "select request_id, tool_name, input, suggestions, blocked_path, default_no, agent_id, created_at
           from permission_requests
          where session_id = ?1 and status = 'pending'
          order by created_at asc",
    )?;
    let rows = st.query_map([session_id], |r| {
        Ok(json!({
            "request_id": r.get::<_, String>(0)?,
            "tool_name": r.get::<_, String>(1)?,
            "input": r.get::<_, String>(2)?
                .parse::<Value>().ok().unwrap_or(Value::Null),
            "suggestions": r.get::<_, Option<String>>(3)?
                .and_then(|s| serde_json::from_str::<Value>(&s).ok()),
            "blocked_path": r.get::<_, Option<String>>(4)?,
            "default_no": r.get::<_, i64>(5)? != 0,
            "agent_id": r.get::<_, Option<String>>(6)?,
            "created_at": r.get::<_, String>(7)?,
            "status": "pending",
        }))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub struct RuleInput {
    pub rule_type: String,
    pub tool: String,
    pub pattern: Option<String>,
    pub scope: String,
    pub origin: String,
    pub enabled: bool,
}

/// Upsert a rule; the same (project, type, tool, pattern, scope) updates instead
/// of duplicating, which is what the schema's unique key promises.
pub fn upsert_rule(conn: &Connection, project_id: &str, r: &RuleInput) -> Result<()> {
    conn.execute(
        "insert into permission_rules
           (id, project_id, rule_type, tool, pattern, scope, origin, enabled, updated_at)
         values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         on conflict (project_id, rule_type, tool, pattern, scope) do update set
           origin = excluded.origin,
           enabled = excluded.enabled,
           updated_at = excluded.updated_at",
        params![
            format!("rule-{}", uuid::Uuid::new_v4().simple()),
            project_id,
            r.rule_type,
            r.tool,
            r.pattern,
            r.scope,
            r.origin,
            if r.enabled { 1 } else { 0 },
            now()
        ],
    )?;
    Ok(())
}

pub fn list_rules(conn: &Connection, project_id: &str) -> Result<Vec<Value>> {
    let mut st = conn.prepare(
        "select id, rule_type, tool, pattern, scope, origin, enabled, created_at
           from permission_rules where project_id = ?1 order by created_at asc",
    )?;
    let rows = st.query_map([project_id], |r| {
        Ok(json!({
            "id": r.get::<_, String>(0)?,
            "ruleType": r.get::<_, String>(1)?,
            "tool": r.get::<_, String>(2)?,
            "pattern": r.get::<_, Option<String>>(3)?,
            "scope": r.get::<_, String>(4)?,
            "origin": r.get::<_, String>(5)?,
            "enabled": r.get::<_, i64>(6)? != 0,
            "createdAt": r.get::<_, String>(7)?,
        }))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn delete_rule(conn: &Connection, scope: &Scope, id: &str) -> Result<()> {
    scope.require_write()?;
    conn.execute("delete from permission_rules where id = ?1", [id])?;
    Ok(())
}

pub fn require_decisions(scope: &Scope) -> Result<(), super::ScopeError> {
    scope.require(Capability::Decisions)
}

/// Record denominated question asked by the AskUserQuestion tool: the payload the
/// question card renders, keyed on its unique request_id so a replay is a no-op.
pub fn record_question(
    conn: &Connection,
    session_id: &str,
    request_id: &str,
    payload: &Value,
) -> Result<()> {
    conn.execute(
        "insert into questions (id, request_id, session_id, tool_name, payload, created_at)
         values (?1, ?2, ?3, 'AskUserQuestion', ?4, ?5)
         on conflict (request_id) do nothing",
        params![
            format!("q-{}", uuid::Uuid::new_v4().simple()),
            request_id,
            session_id,
            payload.to_string(),
            now()
        ],
    )?;
    Ok(())
}

/// Persist the answer the GUI returned, keyed on the same request_id.
pub fn answer_question(conn: &Connection, request_id: &str, answers: &Value) -> Result<bool> {
    let n = conn.execute(
        "update questions set answers = ?1, status = 'answered', answered_at = ?2
          where request_id = ?3 and status = 'pending'",
        params![answers.to_string(), now(), request_id],
    )?;
    Ok(n == 1)
}
