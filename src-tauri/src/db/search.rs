// F10/F11: export a transcript to markdown with attachments, and the
// cross-session search that runs on SQLite FTS5 over blocks.text -- never on
// `like '%...%'`.
//
use rusqlite::params;

/// Rebuild the shadow index from `blocks` when a transcript was re-imported or
/// the index was dropped.
pub fn rebuild(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "insert into blocks_fts(blocks_fts) values ('rebuild');",
    )?;
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Hit {
    pub session_id: String,
    pub message_id: String,
    pub block_id: i64,
    pub kind: String,
    pub snippet: String,
    pub session_title: Option<String>,
    pub created_at: String,
}

/// One line per matching block, with the session and turn for context.
pub fn search(
    conn: &rusqlite::Connection,
    project_id: &str,
    query: &str,
    limit: i64,
) -> anyhow::Result<Vec<Hit>> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    let safe: String = query
        .chars()
        .map(|c| if c.is_alphanumeric() || c.is_whitespace() || c == '_' { c } else { ' ' })
        .collect();
    let tokens: Vec<&str> = safe.split_whitespace().filter(|w| !w.is_empty()).collect();
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    let escaped = tokens
        .iter()
        .map(|w| format!("\"{w}\"*"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let mut st = conn.prepare(
        "select m.session_id, m.id, b.id, b.kind,
                snippet(blocks_fts, 0, '[', ']', '...', 24),
                s.title, m.created_at
           from blocks_fts f
           join blocks b on b.id = f.rowid
           join messages m on m.id = b.message_id
           join sessions s on s.id = m.session_id
          where blocks_fts match ?1 and s.project_id = ?2
          order by rank limit ?3",
    )?;
    let rows = st.query_map(params![escaped.as_str(), project_id, limit], |r| {
        Ok(Hit {
            session_id: r.get(0)?,
            message_id: r.get(1)?,
            block_id: r.get(2)?,
            kind: r.get(3)?,
            snippet: r.get(4)?,
            session_title: r.get(5)?,
            created_at: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn export_markdown(
    conn: &rusqlite::Connection,
    project_id: &str,
    session_id: &str,
) -> anyhow::Result<String> {
    let mut out = String::new();
    let (title, model, created): (Option<String>, String, String) = conn.query_row(
        "select title, model, created_at from sessions where id = ?1 and project_id = ?2",
        [session_id, project_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    out.push_str(&format!("# {}\n\n", title.as_deref().unwrap_or("kiri session")));
    out.push_str(&format!("- model: `{model}`\n- started: {created}\n\n"));

    let mut st = conn.prepare(
        "select id, role, created_at from messages
          where session_id = ?1 order by rowid asc",
    )?;
    let msgs: Vec<(String, String, String)> = st
        .query_map([session_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .filter_map(|r| r.ok())
        .collect();

    for (id, role, at) in msgs {
        out.push_str(&format!("### {} - {}\n\n", role, at));
        let mut bst = conn.prepare(
            "select kind, text, tool_use_id, json from blocks where message_id = ?1 order by idx asc",
        )?;
        let blocks: Vec<(String, Option<String>, Option<String>, Option<String>)> = bst
            .query_map([&id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .filter_map(|r| r.ok())
            .collect();
        for (kind, text, tool_use_id, json) in blocks {
            match kind.as_str() {
                "text" => out.push_str(&format!("{}\n\n", text.unwrap_or_default())),
                "thinking" => {
                    out.push_str(&format!("<details><summary>thinking</summary>\n\n{}\n\n</details>\n\n", text.unwrap_or_default()))
                }
                "tool_use" => out.push_str(&format!(
                    "```json\n{}\n```\n\n",
                    json.unwrap_or_default()
                )),
                "tool_result" => out.push_str(&format!(
                    "- result `{}`: {}\n\n",
                    tool_use_id.unwrap_or_default(),
                    text.unwrap_or_default()
                )),
                _ => {}
            }
        }
    }
    Ok(out)
}
