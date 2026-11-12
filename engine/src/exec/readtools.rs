use crate::exec::{arg_bool, arg_str, arg_u64, skip_dir};
use crate::runtime::Runtime;
use crate::tools::{resolve_path, ToolResult, DEFAULT_READ_LIMIT, MAX_READ_BYTES, MAX_SEARCH_RESULTS};
use base64::Engine as _;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;

async fn read_text(rt: &Arc<Runtime>, input: &Value, key: &str) -> ToolResult {
    let Some(path) = arg_str(input, key) else {
        return ToolResult::err(format!("Read: {key} is required"));
    };
    let p = match resolve_path(&rt.cwd(), &path) {
        Ok(p) => p,
        Err(e) => return ToolResult::err(e.to_string()),
    };
    let meta = match std::fs::metadata(&p) {
        Ok(m) => m,
        Err(e) => return ToolResult::err(format!("Read: {}: {e}", p.display())),
    };
    if !meta.is_file() {
        return ToolResult::err(format!("Read: not a file: {}", p.display()));
    }
    if meta.len() > MAX_READ_BYTES {
        return ToolResult::err(format!(
            "Read: file is {} bytes; the ceiling is {} (use offset/limit on a smaller slice)",
            meta.len(),
            MAX_READ_BYTES
        ));
    }
    let ext = p
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp") {
        return read_image(&p, &ext);
    }
    if ext == "pdf" {
        return read_pdf(&p);
    }
    let text = match std::fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) => return ToolResult::err(format!("Read: {e}")),
    };
    if ext == "ipynb" {
        return read_notebook(&text, &p);
    }
    let offset = arg_u64(input, "offset").unwrap_or(1).max(1) as usize;
    let limit = arg_u64(input, "limit").unwrap_or(DEFAULT_READ_LIMIT) as usize;
    let total = text.lines().count();
    let body = text
        .lines()
        .skip(offset - 1)
        .take(limit)
        .enumerate()
        .map(|(i, l)| format!("{:>6}\t{}", offset + i, l))
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = body;
    if offset + limit <= total {
        out.push_str(&format!(
            "\n\n[{} lines total; showing {}-{}. Call Read again with offset={}]",
            total,
            offset,
            offset + limit - 1,
            offset + limit
        ));
    }
    ToolResult::text(out)
}

fn read_image(p: &Path, ext: &str) -> ToolResult {
    let bytes = match std::fs::read(p) {
        Ok(b) => b,
        Err(e) => return ToolResult::err(format!("Read: {e}")),
    };
    let media = match ext {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/jpeg",
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    ToolResult::blocks(vec![
        json!({"type": "image", "source": {"type": "base64", "media_type": media, "data": b64}}),
        json!({"type": "text", "text": format!("Read image: {}", p.display())}),
    ])
}

fn read_pdf(p: &Path) -> ToolResult {
    let bytes = match std::fs::read(p) {
        Ok(b) => b,
        Err(e) => return ToolResult::err(format!("Read: {e}")),
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    ToolResult::blocks(vec![
        json!({
            "type": "document",
            "source": {"type": "base64", "media_type": "application/pdf", "data": b64}
        }),
        json!({"type": "text", "text": format!("Read PDF: {}", p.display())}),
    ])
}

fn read_notebook(raw: &str, p: &Path) -> ToolResult {
    let nb: Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(e) => return ToolResult::err(format!("Read: bad notebook JSON: {e}")),
    };
    let cells = nb
        .get("cells")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = format!(
        "Notebook: {}\n
{} cells\n
",
        p.display(),
        cells.len()
    );
    for (i, c) in cells.iter().enumerate() {
        let ty = c.get("cell_type").and_then(Value::as_str).unwrap_or("?");
        let id = c.get("id").and_then(Value::as_str).unwrap_or("");
        out.push_str(&format!("\n
--- cell {} - {} - id={} ---\n
", i, ty, id));
        let joined = match c.get("source") {
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(""),
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };
        out.push_str(&joined);
        out.push('\n');
    }
    ToolResult::text(out)
}

pub async fn read(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    read_text(rt, input, "file_path").await
}

pub async fn write(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let Some(path) = arg_str(input, "file_path") else {
        return ToolResult::err("Write: file_path is required");
    };
    let Some(content) = arg_str(input, "content") else {
        return ToolResult::err("Write: content is required");
    };
    let p = match resolve_path(&rt.cwd(), &path) {
        Ok(p) => p,
        Err(e) => return ToolResult::err(e.to_string()),
    };
    if let Some(dir) = p.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            return ToolResult::err(format!("Write: {e}"));
        }
    }
    let existed = p.exists();
    // A checkpoint is the body as it was BEFORE this tool ran, so restoring a
    // turn can undo the write. The transcript is the source of truth; the host
    // indexes this line and S08 restores from it.
    let pre = std::fs::read_to_string(&p).ok();
    register_checkpoint(rt, &p, pre.as_deref(), existed);
    if let Err(e) = std::fs::write(&p, content.as_bytes()) {
        return ToolResult::err(format!("Write: {e}"));
    }
    ToolResult::text(format!(
        "{} {} ({} bytes)",
        if existed { "Overwrote" } else { "Created" },
        p.display(),
        content.len()
    ))
}

pub async fn edit(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let (Some(path), Some(old), Some(new)) = (
        arg_str(input, "file_path"),
        arg_str(input, "old_string"),
        arg_str(input, "new_string"),
    ) else {
        return ToolResult::err("Edit: file_path, old_string and new_string are required");
    };
    let replace_all = arg_bool(input, "replace_all").unwrap_or(false);
    let p = match resolve_path(&rt.cwd(), &path) {
        Ok(p) => p,
        Err(e) => return ToolResult::err(e.to_string()),
    };
    let cur = match std::fs::read_to_string(&p) {
        Ok(c) => c,
        Err(e) => return ToolResult::err(format!("Edit: {}: {e}", p.display())),
    };
    // Record the body before the edit lands; only when the edit will change it.
    register_checkpoint(rt, &p, Some(&cur), true);
    if old.is_empty() {
        return ToolResult::err("Edit: old_string is empty");
    }
    let count = cur.matches(&old).count();
    if count == 0 {
        return ToolResult::err(format!(
            "Edit: old_string not found in {}. Read the file again; whitespace and quoting must match exactly.",
            p.display()
        ));
    }
    if count > 1 && !replace_all {
        return ToolResult::err(format!(
            "Edit: old_string appears {count} times in {}. Add surrounding context, or set replace_all.",
            p.display()
        ));
    }
    let bytes = if replace_all {
        cur.as_bytes().len() - old.len() * count + new.len() * count
    } else {
        cur.len() - old.len() + new.len()
    };
    let next = if replace_all {
        cur.replace(&old, &new)
    } else {
        cur.replacen(&old, &new, 1)
    };
    if let Err(e) = std::fs::write(&p, next) {
        return ToolResult::err(format!("Edit: {e}"));
    }
    ToolResult::text(if replace_all {
        format!("Edited {} ({} replacements, {} bytes)", p.display(), count, bytes)
    } else {
        format!("Edited {}", p.display())
    })
}

pub async fn glob(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let Some(pattern) = arg_str(input, "pattern") else {
        return ToolResult::err("Glob: pattern is required");
    };
    let base = input
        .get("path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| rt.cwd());
    let joined = if Path::new(&pattern).is_absolute() {
        pattern.clone()
    } else {
        base.join(&pattern).to_string_lossy().to_string()
    };
    let glob = match globset::GlobBuilder::new(&joined)
        .literal_separator(false)
        .build()
    {
        Ok(g) => g.compile_matcher(),
        Err(e) => return ToolResult::err(format!("Glob: bad pattern: {e}")),
    };

    let mut hits: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    for entry in walkdir::WalkDir::new(&base)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let n = e.file_name().to_string_lossy();
            !(e.file_type().is_dir() && skip_dir(&n))
        })
        .flatten()
    {
        if !entry.file_type().is_file() || !glob.is_match(entry.path()) {
            continue;
        }
        if let Ok(m) = entry.metadata() {
            if let Ok(t) = m.modified() {
                hits.push((t, entry.path().to_path_buf()));
            }
        }
        if hits.len() > 5000 {
            break;
        }
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0));
    if hits.is_empty() {
        return ToolResult::text("No files found.");
    }
    let total = hits.len();
    let shown: Vec<String> = hits
        .into_iter()
        .take(MAX_SEARCH_RESULTS)
        .map(|(_, p)| p.to_string_lossy().to_string())
        .collect();
    let mut out = shown.join("\n\n
");
    if total > MAX_SEARCH_RESULTS {
        out.push_str(&format!(
            "\n
\n
[{} files matched; showing first {}]",
            total, MAX_SEARCH_RESULTS
        ));
    }
    ToolResult::text(out)
}

/// Record a file's body before a write/edit lands. The host indexes
/// `file_checkpoint` lines and S08 restores from them; a missing transcript (a
/// non-persisting session) simply drops the record, it must not fail the tool.
fn register_checkpoint(rt: &Arc<Runtime>, p: &Path, body: Option<&str>, existed: bool) {
    if rt.transcript_path().is_none() {
        return;
    }
    let body_b64 = body.map(|b| {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(b.as_bytes())
    });
    let _ = rt.record(json!({
        "type": "file_checkpoint",
        "sessionId": rt.session_id(),
        "path": p.to_string_lossy(),
        "existedPre": existed,
        "bytes": body.map(|b| b.len()).unwrap_or(0),
        "body_base64": body_b64,
        "uuid": uuid::Uuid::new_v4().to_string(),
        "timestamp": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    }));
}

pub async fn run(rt: &Arc<Runtime>, name: &str, input: &Value) -> ToolResult {
    match name {
        "Read" => read(rt, input).await,
        "Write" => write(rt, input).await,
        "Edit" => edit(rt, input).await,
        "Glob" => glob(rt, input).await,
        other => ToolResult::err(format!("unknown_tool: {other}")),
    }
}
