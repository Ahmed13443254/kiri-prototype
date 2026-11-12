// Skills: markdown files with YAML frontmatter, listed by name + description,
// loaded in full only when the model asks (`--plugin-dir` / `.kiri/skills`).

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::session_state::Settings;

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    pub allowed_tools: Vec<String>,
    pub source_path: PathBuf,
}

pub fn load_all(cfg: &crate::session_state::SessionConfig, _settings: &Settings) -> Vec<Skill> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    // user skills
    if let Some(d) = dirs::home_dir() {
        dirs.push(d.join(".kiri").join("skills"));
    }
    // project skills
    dirs.push(cfg.cwd.join(".kiri").join("skills"));
    // plugin dirs
    for p in &cfg.plugin_dirs {
        dirs.push(p.join("skills"));
    }

    let mut out = Vec::new();
    for d in dirs {
        walk(&d, &mut out);
    }
    out
}

fn walk(dir: &Path, out: &mut Vec<Skill>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        }
        let is_skill_file = p
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("SKILL.md"))
            .unwrap_or(false);
        if !is_skill_file {
            continue;
        }
        if let Some(s) = parse_skill(&p) {
            out.push(s);
        }
    }
}

fn parse_skill(path: &Path) -> Option<Skill> {
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
        .or_else(|| {
            path.parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .map(str::to_string)
        })?;
    let description = fm
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let allowed_tools = fm
        .get("allowed-tools")
        .or_else(|| fm.get("allowed_tools"))
        .map(|v| match v {
            Value::String(s) => s.split(',').map(|x| x.trim().to_string()).collect(),
            Value::Array(a) => a
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        })
        .unwrap_or_default();

    Some(Skill {
        name,
        description,
        body,
        allowed_tools,
        source_path: path.to_path_buf(),
    })
}

fn split_frontmatter(raw: &str) -> Option<(String, String)> {
    let raw = raw.trim_start_matches('\u{feff}');
    if !raw.starts_with("---") {
        return Some((String::new(), raw.to_string()));
    }
    let rest = &raw[3..];
    let end = rest.find("\n---")?;
    let front = rest[..end].trim().to_string();
    let body = rest[end + 4..].trim_start_matches(['\r', '\n']).to_string();
    Some((front, body))
}

/// The listing block: names and descriptions only, never full bodies. This
/// is what `skillListingMaxDescChars` caps.
pub fn listing(skills: &[Skill], max_desc_chars: usize) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n\n<skills>\n");
    for sk in skills {
        let desc: String = sk.description.chars().take(max_desc_chars).collect();
        s.push_str(&format!("- {}: {}\n", sk.name, desc));
    }
    s.push_str("</skills>");
    s
}

pub fn load_by_name<'a>(skills: &'a [Skill], name: &str) -> Option<&'a Skill> {
    skills.iter().find(|s| s.name == name)
}

pub fn to_json(skills: &[Skill]) -> Vec<Value> {
    skills
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "description": s.description,
                "allowedTools": s.allowed_tools,
                "path": s.source_path.to_string_lossy(),
            })
        })
        .collect()
}
