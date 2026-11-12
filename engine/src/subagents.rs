// Subagents: a self-contained job delegated by the Agent tool.
//
// One subagent gets its own context window; the report is the only thing that
// crosses back. Discovery reads .kiri/agents/<name>.md in the project and in
// the user home, frontmatter first.

use serde_json::{json, Value};
use std::path::Path;

pub struct AgentDef {
    pub name: String,
    pub description: String,
    pub prompt: String,
    pub tools: Option<Vec<String>>,
    pub model: Option<String>,
    pub isolation: Option<String>,
}

pub fn load_all(cfg: &crate::session_state::SessionConfig) -> Vec<AgentDef> {
    let mut dirs = Vec::new();
    if let Some(h) = dirs::home_dir() {
        dirs.push(h.join(".kiri").join("agents"));
    }
    dirs.push(cfg.cwd.join(".kiri").join("agents"));
    for p in &cfg.plugin_dirs {
        dirs.push(p.join("agents"));
    }

    let mut out = Vec::new();
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            if let Some(a) = parse(&p) {
                out.push(a);
            }
        }
    }
    out
}

fn parse(path: &Path) -> Option<AgentDef> {
    let raw = std::fs::read_to_string(path).ok()?;
    let (front, body) = split_frontmatter(&raw)?;
    let fm: Value = if front.trim().is_empty() {
        json!({})
    } else {
        serde_yaml::from_str(&front).ok()?
    };
    let name = fm
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| path.file_stem().and_then(|s| s.to_str()).map(str::to_string))?;
    let description = fm
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let tools = fm.get("tools").map(|t| match t {
        Value::String(s) => s
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect(),
        Value::Array(a) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    });
    Some(AgentDef {
        name,
        description,
        prompt: body,
        tools,
        model: fm.get("model").and_then(Value::as_str).map(str::to_string),
        isolation: fm
            .get("isolation")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

fn split_frontmatter(raw: &str) -> Option<(String, String)> {
    let raw = raw.trim_start_matches('\u{feff}');
    if !raw.starts_with("---") {
        return Some((String::new(), raw.to_string()));
    }
    let rest = &raw[3..];
    let end = rest.find("\n---")?;
    Some((
        rest[..end].trim().to_string(),
        rest[end + 4..].trim().to_string(),
    ))
}

pub async fn list(rt: &std::sync::Arc<crate::runtime::Runtime>) -> Vec<Value> {
    let defs = load_all(&rt.cfg);
    let mut out = vec![json!({
        "name": "general-purpose",
        "description": "general purpose agent",
        "builtin": true
    })];
    out.extend(defs.into_iter().map(|a| {
        json!({
            "name": a.name,
            "description": a.description,
            "tools": a.tools,
            "model": a.model,
        })
    }));
    out
}

/// The definition behind a `subagent_type`, if the user configured one.
/// `general-purpose` is the built-in and has no definition file.
pub fn find_def(cfg: &crate::session_state::SessionConfig, name: &str) -> Option<AgentDef> {
    if name == "general-purpose" {
        return None;
    }
    load_all(cfg).into_iter().find(|a| a.name == name)
}
