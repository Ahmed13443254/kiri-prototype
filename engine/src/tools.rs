// The tool surface: definitions (schema drives the GUI renderer) and executors.
//
// Contract (recon + features.csv):
//   - a tool the GUI has no renderer for still renders, schema-driven
//   - read-only tools in one turn may run concurrently
//   - every result is { content: [...blocks], is_error: bool }

use anyhow::{bail, Context, Result};
use base64::Engine as _;
use regex::Regex;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::permissions::Risk;

pub const MAX_READ_BYTES: u64 = 10 * 1024 * 1024;
pub const DEFAULT_READ_LIMIT: u64 = 2000;
pub const MAX_SEARCH_RESULTS: usize = 250;
pub const GREP_CONTENT_CAP: usize = 400;

/// Tool metadata, before any per-session MCP tools are added.
pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "Read",
            "description": "Read a text file, image, notebook, or PDF from disk. Returns numbered lines for text. Defaults to the first 2000 lines; use offset/limit to page through larger files. Paths may be absolute or relative to the working directory.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "file_path": {"type": "string", "description": "Absolute path, or a path relative to the working directory."},
                    "offset": {"type": "integer", "description": "1-based line number to start reading from."},
                    "limit": {"type": "integer", "description": "Number of lines to read (default 2000)."}
                },
                "required": ["file_path"]
            },
            "family": "read"
        }),
        json!({
            "name": "Write",
            "description": "Write a file, overwriting it entirely. Content is exact: no trailing newline is added unless you include one. Use Edit for a change to part of an existing file.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"},
                    "content": {"type": "string", "description": "The complete file body."}
                },
                "required": ["file_path", "content"]
            },
            "family": "write"
        }),
        json!({
            "name": "Edit",
            "description": "Replace one exact string in a file. old_string must appear exactly once unless replace_all is true. The file is re-read for every edit; edits cannot batch.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "file_path": {"type": "string"},
                    "old_string": {"type": "string"},
                    "new_string": {"type": "string"},
                    "replace_all": {"type": "boolean", "default": false}
                },
                "required": ["file_path", "old_string", "new_string"]
            },
            "family": "edit"
        }),
        json!({
            "name": "Glob",
            "description": "List files by glob pattern, e.g. `src/**/*.ts`. Returns paths newest first, capped at 250.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string", "description": "Directory to search in. Defaults to the working directory."}
                },
                "required": ["pattern"]
            },
            "family": "read"
        }),
        json!({
            "name": "Grep",
            "description": "Search file contents by regular expression. Mode files_with_matches (default) or content (up to 250 lines). Glob and type filter the search set.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string"},
                    "path": {"type": "string"},
                    "glob": {"type": "string"},
                    "output_mode": {"type": "string", "enum": ["content", "files_with_matches", "count"]},
                    "-i": {"type": "boolean"},
                    "-n": {"type": "boolean"},
                    "-A": {"type": "integer"},
                    "-B": {"type": "integer"}
                },
                "required": ["pattern"]
            },
            "family": "read"
        }),
        json!({
            "name": "Bash",
            "description": "Run a shell command in the working directory. A timeout is enforced. background=true starts it as a task and returns its id; kill it with TaskStop.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "description": {"type": "string"},
                    "timeout": {"type": "integer", "description": "Milliseconds, default 120000, max 600000."},
                    "background": {"type": "boolean"},
                    "shell": {"type": "string", "enum": ["bash", "powershell"]}
                },
                "required": ["command"]
            },
            "family": "bash"
        }),
        json!({
            "name": "BashOutput",
            "description": "Read buffered output from a background shell, optionally filtered by a regular expression. Returns only new output since the last call.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "shell_id": {"type": "string"},
                    "pattern": {"type": "string"}
                },
                "required": ["shell_id"]
            },
            "family": "bash"
        }),
        json!({
            "name": "TaskStop",
            "description": "Stop a running background task: a background shell, a background agent, or a monitor. Takes the task id returned by the tool that started it.",
            "input_schema": {
                "type": "object",
                "properties": {"task_id": {"type": "string"}},
                "required": ["task_id"]
            },
            "family": "bash"
        }),
        json!({
            "name": "NotebookEdit",
            "description": "Edit one cell of a Jupyter notebook by cell id or cell number. replace changes source; insert adds a cell after the target; delete removes it.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "notebook_path": {"type": "string"},
                    "cell_id": {"type": "string"},
                    "cell_number": {"type": "integer"},
                    "new_source": {"type": "string"},
                    "cell_type": {"type": "string", "enum": ["code", "markdown"]},
                    "edit_mode": {"type": "string", "enum": ["replace", "insert", "delete"]}
                },
                "required": ["notebook_path", "edit_mode"]
            },
            "family": "edit"
        }),
        json!({
            "name": "WebFetch",
            "description": "Fetch a URL over HTTP(S), convert the body to markdown, and answer a prompt from it. GET only. The safety preflight refuses localhost, link-local and metadata endpoints.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "url": {"type": "string"},
                    "prompt": {"type": "string"}
                },
                "required": ["url", "prompt"]
            },
            "family": "web"
        }),
        json!({
            "name": "WebSearch",
            "description": "Search the web. Returns a list of results with title, url, and snippet. Optional domain filters restrict the result set.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "allowed_domains": {"type": "array", "items": {"type": "string"}},
                    "blocked_domains": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["query"]
            },
            "family": "web"
        }),
        json!({
            "name": "AskUserQuestion",
            "description": "Ask the user 1-4 structured questions of 2-4 options each. Also returns the user's free-form response. Blocks the loop until answered or timed out.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 4,
                        "items": {
                            "type": "object",
                            "properties": {
                                "question": {"type": "string"},
                                "header": {"type": "string"},
                                "multiSelect": {"type": "boolean", "default": false},
                                "options": {"type": "array", "minItems": 2, "maxItems": 4}
                            },
                            "required": ["question", "header", "options"]
                        }
                    }
                },
                "required": ["questions"]
            },
            "family": "question"
        }),
        json!({
            "name": "TodoWrite",
            "description": "Replace the session todo list. Send the whole list every time, with each item's status. At most one item is in_progress.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "todos": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "content": {"type": "string"},
                                "activeForm": {"type": "string"},
                                "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}
                            },
                            "required": ["content", "status"]
                        }
                    }
                },
                "required": ["todos"]
            },
            "family": "todo"
        }),
        json!({
            "name": "TaskCreate",
            "description": "Create a task in the durable task registry. Returns the task id.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "subject": {"type": "string"},
                    "description": {"type": "string"},
                    "activeForm": {"type": "string"}
                },
                "required": ["subject"]
            },
            "family": "task"
        }),
        json!({
            "name": "TaskUpdate",
            "description": "Update a task in the registry: status, owner, description, or its blocked_by/ blocks edges.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "task_id": {"type": "string"},
                    "status": {"type": "string", "enum": ["pending", "in_progress", "completed", "deleted", "failed", "stopped"]},
                    "subject": {"type": "string"},
                    "description": {"type": "string"},
                    "owner": {"type": "string"},
                    "addBlocks": {"type": "array", "items": {"type": "string"}},
                    "addBlockedBy": {"type": "array", "items": {"type": "string"}}
                },
                "required": ["task_id"]
            },
            "family": "task"
        }),
        json!({
            "name": "TaskList",
            "description": "List the tasks in the task registry with id, status, subject, owner, and edges.",
            "input_schema": {"type": "object", "properties": {}}
        }),
        json!({
            "name": "TaskGet",
            "description": "Read one task in full by id.",
            "input_schema": {
                "type": "object",
                "properties": {"task_id": {"type": "string"}},
                "required": ["task_id"]
            },
            "family": "task"
        }),
        json!({
            "name": "Agent",
            "description": "Delegate a self-contained job to a subagent with its own context window. It returns one report. Use it for search-heavy or isolated work; keep the decision in this conversation. isolation=worktree gives the agent a clean git worktree.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "description": {"type": "string"},
                    "prompt": {"type": "string"},
                    "subagent_type": {"type": "string"},
                    "isolation": {"type": "string", "enum": ["worktree", "remote"]},
                    "run_in_background": {"type": "boolean"},
                    "tools": {"type": "array", "items": {"type": "string"}},
                    "model": {"type": "string"}
                },
                "required": ["description", "prompt", "subagent_type"]
            },
            "family": "agent"
        }),
        json!({
            "name": "Skill",
            "description": "Load a skill by name. Its body is inserted into the conversation as instructions for the rest of this turn chain.",
            "input_schema": {
                "type": "object",
                "properties": {"name": {"type": "string"}},
                "required": ["name"]
            },
            "family": "read"
        }),
        json!({
            "name": "ToolSearch",
            "description": "Find tools when the tool list is large: a keyword query, or `select:Name,Name` for exact names. Deferred tools must be found before they can be called.",
            "input_schema": {
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"]
            },
            "family": "read"
        }),
        json!({
            "name": "EnterWorktree",
            "description": "Create a git worktree and move this session's file operations into it for isolated change.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "confirm": {"type": "boolean"}
                },
                "required": ["name", "confirm"]
            },
            "family": "write"
        }),
        json!({
            "name": "ExitWorktree",
            "description": "Leave the current worktree. `action: keep` leaves it on disk; `action: remove` deletes it (requires confirm).",
            "input_schema": {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["keep", "remove"]},
                    "confirm": {"type": "boolean"}
                },
                "required": ["action", "confirm"]
            },
            "family": "write"
        }),
        json!({
            "name": "EnterPlanMode",
            "description": "Switch this session to plan mode: read-only investigation until the user exits.",
            "input_schema": {"type": "object", "properties": {}}
        }),
        json!({
            "name": "ExitPlanMode",
            "description": "Leave plan mode and return to the previous permission mode.",
            "input_schema": {"type": "object", "properties": {}}
        }),
    ]
}

pub fn family(name: &str) -> &'static str {
    match name {
        "Read" | "Glob" | "Grep" | "Skill" | "ToolSearch" => "read",
        "Write" => "write",
        "Edit" | "NotebookEdit" => "edit",
        "Bash" | "BashOutput" | "TaskStop" => "bash",
        "WebFetch" | "WebSearch" => "web",
        "Agent" => "agent",
        "AskUserQuestion" => "question",
        "TodoWrite" => "todo",
        "TaskCreate" | "TaskUpdate" | "TaskList" | "TaskGet" => "task",
        "EnterWorktree" | "ExitWorktree" | "EnterPlanMode" | "ExitPlanMode" => "worktree",
        _ if name.starts_with("mcp__") => "mcp",
        _ => "unknown",
    }
}

pub fn risk(name: &str) -> Risk {
    match family(name) {
        "read" | "todo" | "task" => Risk::Read,
        "write" | "edit" | "worktree" => Risk::Write,
        "bash" => Risk::Bash,
        "web" => Risk::Web,
        "agent" => Risk::Agent,
        "mcp" => Risk::Mcp,
        _ => Risk::Read,
    }
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub content: Vec<Value>,
    pub is_error: bool,
}

impl ToolResult {
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            content: vec![json!({"type": "text", "text": s.into()})],
            is_error: false,
        }
    }
    pub fn err(s: impl Into<String>) -> Self {
        Self {
            content: vec![json!({"type": "text", "text": s.into()})],
            is_error: true,
        }
    }
    pub fn blocks(b: Vec<Value>) -> Self {
        Self {
            content: b,
            is_error: false,
        }
    }
}

/// Resolve a tool path against the session's live working directory (which a
/// worktree move may have changed) and add_dirs. Relative paths join onto
/// `cwd`; absolute paths are kept as given.
pub fn resolve_path(cwd: &Path, p: &str) -> Result<PathBuf> {
    let raw = PathBuf::from(p);
    let joined = if raw.is_absolute() {
        raw
    } else {
        cwd.join(raw)
    };
    Ok(joined)
}

/// True when `path` is inside cwd or one of the add_dirs (lexical check after
/// normalising `.` and `..` because the file may not exist yet). This is the
/// boundary the read auto-allow is gated on.
pub fn in_workspace(cwd: &Path, add_dirs: &[PathBuf], path: &Path) -> bool {
    let mut roots = vec![cwd.to_path_buf()];
    roots.extend(add_dirs.iter().cloned());
    let comps = normalise(path);
    roots.iter().any(|r| {
        let root = normalise(r);
        comps.starts_with(root.components().as_path())
    })
}

fn normalise(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn clip_lines(s: &str, max_lines: usize) -> String {
    let mut out: Vec<&str> = s.lines().take(max_lines).collect();
    let clipped = s.lines().count() > max_lines;
    if clipped {
        out.push("... (output truncated)");
    }
    out.join("\n")
}

pub fn which(cmd: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let p = dir.join(cmd);
        if p.is_file() {
            return Some(p);
        }
        #[cfg(windows)]
        {
            for ext in ["exe", "cmd", "bat", "ps1"] {
                let q = dir.join(format!("{cmd}.{ext}"));
                if q.is_file() {
                    return Some(q);
                }
            }
        }
    }
    None
}

/// Check that a fetched URL is not a local/metadata address (preflight; it
/// does not replace DNS pinning, but it stops the obvious SSRF).
pub fn url_preflight(url: &str) -> Result<()> {
    let lower = url.to_lowercase();
    if !lower.starts_with("http://") && !lower.starts_with("https://") {
        bail!("unsupported_url_scheme");
    }
    let after = lower.split("://").nth(1).unwrap_or("");
    let host = after
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("")
        .split(':')
        .next()
        .unwrap_or("");
    let bad = [
        "localhost",
        "127.0.0.1",
        "0.0.0.0",
        "::1",
        "[::1]",
        "169.254.169.254",
        "metadata.google.internal",
        "10.0.0.0",
    ];
    if bad.contains(&host) || host.starts_with("127.") || host.starts_with("10.") || host.starts_with("192.168.") {
        bail!("blocked_url_host: {host}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_path_follows_the_live_cwd() {
        let cwd = Path::new("/work/project");
        assert_eq!(
            resolve_path(cwd, "src/a.rs").unwrap(),
            PathBuf::from("/work/project/src/a.rs")
        );
        assert_eq!(
            resolve_path(cwd, "/elsewhere/a.rs").unwrap(),
            PathBuf::from("/elsewhere/a.rs")
        );
    }

    #[test]
    fn in_workspace_bounds_reads_to_cwd_and_add_dirs() {
        let cwd = PathBuf::from("/work/project");
        let add = vec![PathBuf::from("/work/shared")];
        assert!(in_workspace(&cwd, &add, Path::new("/work/project/src/a.rs")));
        assert!(in_workspace(&cwd, &add, Path::new("/work/shared/b.rs")));
        assert!(in_workspace(&cwd, &add, Path::new("/work/project/./src/../a.rs")));
        // Escapes must not auto-allow.
        assert!(!in_workspace(&cwd, &add, Path::new("/work/project/../secrets")));
        assert!(!in_workspace(&cwd, &add, Path::new("/etc/passwd")));
        assert!(!in_workspace(&cwd, &[], Path::new("/work/shared/b.rs")));
    }
}
