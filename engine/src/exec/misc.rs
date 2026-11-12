// Notebook editing, web fetch/search, todo list, task registry, skills-spawn,
// tool search, and worktree/plan-mode controls.

use crate::runtime::Runtime;
use crate::tools::{clip_lines, url_preflight, ToolResult, MAX_SEARCH_RESULTS};
use serde_json::{json, Value};
use crate::exec::arg_str;
use std::sync::Arc;

pub async fn notebook_edit(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let Some(path) = arg_str(input, "notebook_path") else {
        return ToolResult::err("NotebookEdit: notebook_path is required");
    };
    let mode = input
        .get("edit_mode")
        .and_then(Value::as_str)
        .unwrap_or("replace");
    let p = match crate::tools::resolve_path(&rt.cwd(), &path) {
        Ok(p) => p,
        Err(e) => return ToolResult::err(e.to_string()),
    };
    let raw = match std::fs::read_to_string(&p) {
        Ok(r) => r,
        Err(e) => return ToolResult::err(format!("NotebookEdit: {e}")),
    };
    let mut nb: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => return ToolResult::err(format!("NotebookEdit: bad notebook JSON: {e}")),
    };
    let cells = match nb.get_mut("cells").and_then(Value::as_array_mut) {
        Some(c) => c,
        None => return ToolResult::err("NotebookEdit: notebook has no cells array"),
    };

    let source: Vec<Value> = arg_str(input, "new_source")
        .unwrap_or_default()
        .lines()
        .map(|l| Value::String(l.to_string()))
        .collect();

    let target_id = arg_str(input, "cell_id");
    let target_num = input.get("cell_number").and_then(Value::as_u64);

    match mode {
        "delete" => {
            let before = cells.len();
            cells.retain(|c| {
                let id = c.get("id").and_then(Value::as_str).unwrap_or("");
                !target_id.as_deref().map(|t| t == id).unwrap_or(false)
            });
            if before == cells.len() {
                return ToolResult::err("NotebookEdit: no cell matched cell_id");
            }
        }
        "insert" => {
            let cell_type = input
                .get("cell_type")
                .and_then(Value::as_str)
                .unwrap_or("code");
            let cell = json!({
                "cell_type": cell_type,
                "id": format!("kiri-{}", uuid::Uuid::new_v4().simple()),
                "metadata": {},
                "source": source,
                "outputs": [],
                "execution_count": Value::Null,
            });
            let at = target_num.unwrap_or(cells.len() as u64) as usize;
            let at = at.min(cells.len());
            cells.insert(at, cell);
        }
        _ => {
            let idx = if let Some(id) = &target_id {
                cells.iter().position(|c| {
                    c.get("id")
                        .and_then(Value::as_str)
                        .map(|x| x == id)
                        .unwrap_or(false)
                })
            } else {
                target_num.map(|n| n as usize)
            };
            let Some(idx) = idx else {
                return ToolResult::err("NotebookEdit: no cell matched cell_id/cell_number");
            };
            if idx >= cells.len() {
                return ToolResult::err("NotebookEdit: cell index out of range");
            }
            cells[idx]["source"] = Value::Array(source);
        }
    }

    let pretty = serde_json::to_string_pretty(&nb).unwrap_or_default();
    match std::fs::write(&p, pretty) {
        Ok(()) => ToolResult::text(format!("NotebookEdit: {} ({mode})", p.display())),
        Err(e) => ToolResult::err(format!("NotebookEdit: {e}")),
    }
}

// Appended executors for misc.rs: web, todo, task registry, skill, tool search.

pub async fn web_fetch(_rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let Some(url) = arg_str(input, "url") else {
        return ToolResult::err("WebFetch: url is required");
    };
    let prompt = arg_str(input, "prompt").unwrap_or_default();
    if let Err(e) = url_preflight(&url) {
        return ToolResult::err(format!("WebFetch: {e}"));
    }
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("kiri-engine/0.1")
        .build()
    {
        Ok(c) => c,
        Err(e) => return ToolResult::err(format!("WebFetch: {e}")),
    };
    let resp = match client.get(&url).send().await {
        Ok(r) => r,
        Err(e) => return ToolResult::err(format!("WebFetch: {e}")),
    };
    if !resp.status().is_success() {
        return ToolResult::err(format!("WebFetch: HTTP {}", resp.status()));
    }
    let body = match resp.text().await {
        Ok(t) => t,
        Err(e) => return ToolResult::err(format!("WebFetch: {e}")),
    };
    let md = strip_html(&body);
    ToolResult::text(format!(
        "WebFetch {url}\nrequest: {prompt}\n\n{}\n\n[{} chars total]",
        clip_lines(&md, 400),
        md.len()
    ))
}

pub async fn web_search(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let Some(query) = arg_str(input, "query") else {
        return ToolResult::err("WebSearch: query is required");
    };
    let Some(base) = rt.setting_str("searchBaseURL") else {
        return ToolResult::err(
            "WebSearch: no search endpoint is configured. Set searchBaseURL in settings; the engine is a client and ships no index.",
        );
    };
    let url = format!("{}/search?q={}", base.trim_end_matches('/'), urlencode(&query));
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .build()
    {
        Ok(c) => c,
        Err(e) => return ToolResult::err(format!("WebSearch: {e}")),
    };
    match client.get(&url).send().await {
        Ok(r) if r.status().is_success() => match r.text().await {
            Ok(t) => ToolResult::text(clip_lines(&t, 200)),
            Err(e) => ToolResult::err(format!("WebSearch: {e}")),
        },
        Ok(r) => ToolResult::err(format!("WebSearch: HTTP {}", r.status())),
        Err(e) => ToolResult::err(format!("WebSearch: {e}")),
    }
}

pub fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    let mut last_space = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if in_tag => {}
            _ => {
                let is_ws = ch.is_whitespace();
                if is_ws && last_space {
                    continue;
                }
                last_space = is_ws;
                out.push(ch);
            }
        }
    }
    out
}

pub async fn todo_write(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let todos = input
        .get("todos")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let in_progress = todos
        .iter()
        .filter(|t| t.get("status").and_then(Value::as_str) == Some("in_progress"))
        .count();
    if in_progress > 1 {
        return ToolResult::err("TodoWrite: at most one item may be in_progress");
    }
    if let Ok(mut g) = rt.todos.lock() {
        *g = todos.clone();
    }
    ToolResult::text(format!("Todo list updated ({} items).", todos.len()))
}

pub async fn task_create(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let Some(subject) = arg_str(input, "subject") else {
        return ToolResult::err("TaskCreate: subject is required");
    };
    let id = format!("task-{}", uuid::Uuid::new_v4().simple());
    let now = chrono::Utc::now().to_rfc3339();
    let v = json!({
        "id": id,
        "subject": subject,
        "description": input.get("description").cloned().unwrap_or(Value::Null),
        "activeForm": input.get("activeForm").cloned().unwrap_or(Value::Null),
        "status": "pending",
        "owner": Value::Null,
        "blocks": [],
        "blockedBy": [],
        "createdAt": now,
        "updatedAt": now,
    });
    if let Ok(mut m) = rt.task_store.lock() {
        m.insert(id.clone(), v);
    }
    ToolResult::text(id)
}

pub async fn task_update(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let Some(id) = arg_str(input, "task_id") else {
        return ToolResult::err("TaskUpdate: task_id is required");
    };
    let Ok(mut m) = rt.task_store.lock() else {
        return ToolResult::err("TaskUpdate: task store unavailable");
    };
    let Some(task) = m.get_mut(&id) else {
        return ToolResult::err(format!("TaskUpdate: no such task: {id}"));
    };
    for key in ["status", "subject", "description", "owner"] {
        if let Some(val) = input.get(key) {
            task[key] = val.clone();
        }
    }
    for (key, target) in [("addBlocks", "blocks"), ("addBlockedBy", "blockedBy")] {
        if let Some(list) = input.get(key).and_then(Value::as_array) {
            let mut cur = task[target].as_array().cloned().unwrap_or_default();
            cur.extend(list.iter().cloned());
            task[target] = Value::Array(cur);
        }
    }
    task["updatedAt"] = Value::String(chrono::Utc::now().to_rfc3339());
    ToolResult::text(format!("TaskUpdate: {id}"))
}

pub async fn task_list(rt: &Arc<Runtime>, _input: &Value) -> ToolResult {
    let Ok(m) = rt.task_store.lock() else {
        return ToolResult::text("[]");
    };
    let mut rows: Vec<Value> = m.values().cloned().collect();
    rows.sort_by(|a, b| {
        a["createdAt"]
            .as_str()
            .unwrap_or("")
            .cmp(b["createdAt"].as_str().unwrap_or(""))
    });
    ToolResult::text(serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".into()))
}

pub async fn task_get(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let Some(id) = arg_str(input, "task_id") else {
        return ToolResult::err("TaskGet: task_id is required");
    };
    let Ok(m) = rt.task_store.lock() else {
        return ToolResult::err("TaskGet: task store unavailable");
    };
    match m.get(&id) {
        Some(v) => ToolResult::text(serde_json::to_string_pretty(v).unwrap_or_default()),
        None => ToolResult::err(format!("TaskGet: no such task: {id}")),
    }
}

pub async fn skill(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let Some(name) = arg_str(input, "name") else {
        return ToolResult::err("Skill: name is required");
    };
    let skills = rt.skills();
    match crate::skills::load_by_name(&skills, &name) {
        Some(s) => ToolResult::text(format!(
            "# Skill: {}\n\n{}\n\nAllowed tools: {}",
            s.name,
            s.body,
            if s.allowed_tools.is_empty() {
                "(all)".to_string()
            } else {
                s.allowed_tools.join(", ")
            }
        )),
        None => {
            let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
            ToolResult::err(format!(
                "Skill: no skill named {name}. Available: {}",
                if names.is_empty() {
                    "(none)".to_string()
                } else {
                    names.join(", ")
                }
            ))
        }
    }
}

pub async fn tool_search(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let Some(query) = arg_str(input, "query") else {
        return ToolResult::err("ToolSearch: query is required");
    };
    let deferred = rt
        .deferred_tools
        .lock()
        .map(|g| g.clone())
        .unwrap_or_default();

    if let Some(sel) = query.strip_prefix("select:") {
        let names: Vec<String> = sel
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let mut found = Vec::new();
        for n in &names {
            if let Some(def) = deferred.get(n) {
                found.push(def.clone());
                if let Ok(mut l) = rt.loaded_deferred.lock() {
                    if !l.contains(n) {
                        l.push(n.clone());
                    }
                }
            } else {
                return ToolResult::err(format!("ToolSearch: no such deferred tool: {n}"));
            }
        }
        if found.is_empty() {
            return ToolResult::err(format!("ToolSearch: no such tools: {}", names.join(", ")));
        }
        return ToolResult::text(
            serde_json::to_string_pretty(&json!({"matched": found})).unwrap_or_default(),
        );
    }

    let q = query.to_lowercase();
    let mut matched = Vec::new();
    for (name, def) in &deferred {
        let desc = def
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        if name.to_lowercase().contains(&q) || desc.contains(&q) {
            matched.push(json!({"name": name, "description": desc}));
            if let Ok(mut l) = rt.loaded_deferred.lock() {
                if !l.contains(name) {
                    l.push(name.clone());
                }
            }
        }
        if matched.len() >= MAX_SEARCH_RESULTS.min(25) {
            break;
        }
    }
    if matched.is_empty() {
        return ToolResult::text("No tools matched.");
    }
    ToolResult::text(serde_json::to_string_pretty(&matched).unwrap_or_default())
}

pub async fn enter_worktree(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let name = arg_str(input, "name").unwrap_or_else(|| "session".into());
    let confirm = input
        .get("confirm")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !confirm {
        return ToolResult::err("EnterWorktree: requires confirm=true");
    }
    match crate::workspaces::create(&rt.cwd(), &name) {
        Ok((wt, path)) => {
            rt.set_cwd(path.clone());
            if let Ok(mut g) = rt.worktree.lock() {
                *g = Some(wt);
            }
            ToolResult::text(format!("Entered worktree {}", path.display()))
        }
        Err(e) => ToolResult::err(format!("EnterWorktree: {e}")),
    }
}

pub async fn exit_worktree(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let action = input
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("keep");
    let confirm = input
        .get("confirm")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if action == "remove" && !confirm {
        return ToolResult::err("ExitWorktree: remove requires confirm=true");
    }
    let wt = rt.worktree.lock().ok().and_then(|mut g| g.take());
    let Some(wt) = wt else {
        return ToolResult::err("ExitWorktree: not in a worktree.");
    };
    rt.set_cwd(wt.repo.clone());
    if action == "remove" {
        if let Err(e) = crate::workspaces::remove(&wt) {
            return ToolResult::err(format!("ExitWorktree: {e}"));
        }
    }
    ToolResult::text(format!("Left worktree {} ({action})", wt.path.display()))
}

pub async fn enter_plan_mode(rt: &Arc<Runtime>, _input: &Value) -> ToolResult {
    let prev = rt.mode();
    if let Ok(mut g) = rt.pre_plan_mode.lock() {
        *g = Some(prev);
    }
    rt.set_mode(crate::permissions::Mode::Plan);
    ToolResult::text("Entered plan mode (read-only).")
}

pub async fn exit_plan_mode(rt: &Arc<Runtime>, _input: &Value) -> ToolResult {
    let prev = rt
        .pre_plan_mode
        .lock()
        .ok()
        .and_then(|mut g| g.take())
        .unwrap_or(crate::permissions::Mode::Default);
    rt.set_mode(prev);
    ToolResult::text(format!("Plan mode off; mode is {}", prev.as_str()))
}

pub async fn run(rt: &Arc<Runtime>, name: &str, input: &Value) -> ToolResult {
    match name {
        "NotebookEdit" => notebook_edit(rt, input).await,
        "WebFetch" => web_fetch(rt, input).await,
        "WebSearch" => web_search(rt, input).await,
        "TodoWrite" => todo_write(rt, input).await,
        "TaskCreate" => task_create(rt, input).await,
        "TaskUpdate" => task_update(rt, input).await,
        "TaskList" => task_list(rt, input).await,
        "TaskGet" => task_get(rt, input).await,
        "Skill" => skill(rt, input).await,
        "ToolSearch" => tool_search(rt, input).await,
        "EnterWorktree" => enter_worktree(rt, input).await,
        "ExitWorktree" => exit_worktree(rt, input).await,
        "EnterPlanMode" => enter_plan_mode(rt, input).await,
        "ExitPlanMode" => exit_plan_mode(rt, input).await,
        "AskUserQuestion" => crate::agent::ask_user_question(rt, input).await,
        "Agent" => crate::agent::spawn_agent(rt, input).await,
        other => ToolResult::err(format!("unknown_tool: {other}")),
    }
}

pub async fn dispatch(rt: &Arc<Runtime>, name: &str, input: &Value) -> ToolResult {
    match name {
        "Read" | "Write" | "Edit" | "Glob" => crate::exec::readtools::run(rt, name, input).await,
        "Grep" | "Bash" | "BashOutput" | "TaskStop" => crate::exec::shell::run(rt, name, input).await,
        other => run(rt, other, input).await,
    }
}
