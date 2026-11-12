// Tauri commands. These are the renderer's only door to the host: no HTTP route
// exists anywhere in the product, by construction.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::db::{history, projects, transcript, Scope};
use crate::engine::child::EngineChild;
use crate::host::Host;

#[tauri::command]
pub async fn open_project(
    path: String,
    name: Option<String>,
    host: State<'_, Arc<Host>>,
) -> Result<Value, String> {
    let canonical = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
    let path_str = canonical.to_string_lossy().to_string();
    let name = name.unwrap_or_else(|| {
        canonical
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path_str.clone())
    });

    let scope = Scope::root();
    let p = host
        .db
        .read(&scope, |conn| Ok(projects::open_project(conn, &path_str, &name)?))
        .map_err(|e| e.to_string())?;

    Ok(json!({"id": p.id, "path": p.path, "name": p.name}))
}

#[tauri::command]
pub async fn list_projects(host: State<'_, Arc<Host>>) -> Result<Value, String> {
    let scope = Scope::root();
    let rows = host
        .db
        .read(&scope, |conn| Ok(projects::list_projects(conn)?))
        .map_err(|e| e.to_string())?;
    Ok(Value::Array(rows))
}

#[tauri::command]
pub async fn list_sessions(
    project_id: String,
    host: State<'_, Arc<Host>>,
) -> Result<Value, String> {
    let scope = Scope::read(project_id.clone());
    let rows = host
        .db
        .read(&scope, |conn| Ok(history::list_sessions(conn, &project_id)?))
        .map_err(|e| e.to_string())?;
    Ok(Value::Array(rows))
}

#[tauri::command]
pub async fn get_session(
    project_id: String,
    session_id: String,
    host: State<'_, Arc<Host>>,
) -> Result<Value, String> {
    let scope = Scope::read(project_id.clone());
    let row = host
        .db
        .read(&scope, |conn| {
            Ok(history::get_session(conn, &project_id, &session_id)?)
        })
        .map_err(|e| e.to_string())?;
    Ok(row.unwrap_or(Value::Null))
}

#[tauri::command]
pub async fn delete_session(
    project_id: String,
    session_id: String,
    host: State<'_, Arc<Host>>,
) -> Result<(), String> {
    if let Some(child) = host.child(&session_id).await {
        child.shutdown().await.map_err(|e| e.to_string())?;
        host.remove_child(&session_id).await;
    }
    let scope = Scope::write_all(Scope::read(project_id));
    host.db
        .write(&scope, |tx| {
            history::delete_session(tx, &scope, &session_id)?;
            Ok(())
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn read_messages(
    project_id: String,
    session_id: String,
    limit: Option<i64>,
    before_rowid: Option<i64>,
    host: State<'_, Arc<Host>>,
) -> Result<Value, String> {
    let scope = Scope::read(project_id);
    let rows = host
        .db
        .read(&scope, |conn| {
            Ok(transcript::read_messages(
                conn,
                &session_id,
                limit.unwrap_or(200),
                before_rowid,
            )?)
        })
        .map_err(|e| e.to_string())?;
    Ok(Value::Array(rows))
}

#[tauri::command]
pub async fn list_rules(project_id: String, host: State<'_, Arc<Host>>) -> Result<Value, String> {
    let scope = Scope::read(project_id);
    let rows = host
        .db
        .read(&scope, |conn| {
            Ok(crate::db::decisions::list_rules(conn, &scope.project_id)?)
        })
        .map_err(|e| e.to_string())?;
    Ok(Value::Array(rows))
}

#[tauri::command]
pub async fn upsert_rule(
    project_id: String,
    rule_type: String,
    tool: String,
    pattern: Option<String>,
    scope_name: Option<String>,
    origin: Option<String>,
    enabled: Option<bool>,
    host: State<'_, Arc<Host>>,
) -> Result<(), String> {
    let scope = Scope::write_all(Scope::read(&project_id));
    let input = crate::db::decisions::RuleInput {
        rule_type,
        tool,
        pattern,
        scope: scope_name.unwrap_or_else(|| "local".to_string()),
        origin: origin.unwrap_or_else(|| "user".to_string()),
        enabled: enabled.unwrap_or(true),
    };
    host.db
        .write(&scope, |tx| {
            crate::db::decisions::upsert_rule(tx, &scope.project_id, &input)?;
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    sync_rules_to_engine(&host, &project_id).await;
    Ok(())
}

#[tauri::command]
pub async fn delete_rule(
    project_id: String,
    id: String,
    host: State<'_, Arc<Host>>,
) -> Result<(), String> {
    let scope = Scope::write_all(Scope::read(project_id.clone()));
    host.db
        .write(&scope, |tx| {
            crate::db::decisions::delete_rule(tx, &scope, &id)?;
            Ok(())
        })
        .map_err(|e| e.to_string())?;
    sync_rules_to_engine(&host, &project_id).await;
    Ok(())
}

async fn sync_rules_to_engine(host: &Arc<Host>, project_id: &str) {
    let scope = Scope::read(project_id.to_string());
    let Ok(rules) = host.db.read(&scope, |c| Ok(crate::db::decisions::list_rules(c, project_id)?)) else { return };
    let Ok(path) = host.db.read(&scope, |c| {
        Ok(c.query_row("select path from projects where id = ?1", [project_id], |r| r.get::<_, String>(0)).ok())
    }) else { return };
    let Some(path) = path else { return };
    let settings_path = std::path::Path::new(&path).join(".kiri").join("settings.local.json");
    let mut root = std::fs::read_to_string(&settings_path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or_else(|| json!({}));
    let mut allow = Vec::new();
    let mut deny = Vec::new();
    let mut ask = Vec::new();
    for r in rules {
        if !r.get("enabled").and_then(Value::as_bool).unwrap_or(false) { continue; }
        let tool = r.get("tool").and_then(Value::as_str).unwrap_or("");
        let pattern = r.get("pattern").and_then(Value::as_str).unwrap_or("*");
        let text = format!("{tool}({pattern})");
        match r.get("ruleType").and_then(Value::as_str).unwrap_or("") {
            "allow" => allow.push(text),
            "deny" => deny.push(text),
            "ask" => ask.push(text),
            _ => {}
        }
    }
    let Some(obj) = root.as_object_mut() else { return };
    let perms = obj.entry("permissions".to_string()).or_insert_with(|| json!({}));
    if let Some(p) = perms.as_object_mut() {
        p.insert("allow".into(), json!(allow));
        p.insert("deny".into(), json!(deny));
        p.insert("ask".into(), json!(ask));
    }
    if let Some(dir) = settings_path.parent() { let _ = std::fs::create_dir_all(dir); }
    let _ = std::fs::write(&settings_path, serde_json::to_string_pretty(&root).unwrap_or_default());

    let Ok(ids) = host.db.read(&Scope::root(), |c| {
        let mut st = c.prepare("select id from sessions where project_id = ?1")?;
        let rows = st.query_map([project_id], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }) else { return };
    for id in ids {
        if let Some(child) = host.child(&id).await {
            let _ = child.request("reinitialize", json!({}), Duration::from_secs(5)).await;
        }
    }
}

#[tauri::command]
pub async fn spawn_session(
    app: AppHandle,
    project_id: String,
    workspace_id: Option<String>,
    session_id: Option<String>,
    cwd: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    permission_mode: Option<String>,
    resume: Option<String>,
    fork_of: Option<String>,
    host: State<'_, Arc<Host>>,
) -> Result<Value, String> {
    let db = host.db.clone();
    let project = {
        let scope = Scope::root();
        db.read(&scope, |c| {
            Ok(crate::db::projects::project_for_path(c, cwd.as_deref().unwrap_or(""))?)
        })
        .map_err(|e| e.to_string())?
    };

    let workdir = cwd.unwrap_or_else(|| {
        project
            .as_ref()
            .map(|p| p.path.clone())
            .unwrap_or_else(|| ".".to_string())
    });
    let resume_stem = resume.as_deref().map(|t| {
        let p = std::path::Path::new(t);
        p.file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| t.to_string())
    });
    let id = if fork_of.is_some() {
        session_id.clone().unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    } else {
        resume_stem
            .clone()
            .or_else(|| session_id.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    };
    let transcript = crate::db::projects_dir()
        .join(crate::engine::ingester::slug(std::path::Path::new(&workdir)))
        .join(format!("{id}.jsonl"));

    let mut extra: Vec<String> = Vec::new();
    if let Some(m) = &model {
        extra.push("--model".into());
        extra.push(m.clone());
    }
    if let Some(e) = &effort {
        extra.push("--effort".into());
        extra.push(e.clone());
    }
    if let Some(m) = &permission_mode {
        extra.push("--permission-mode".into());
        extra.push(m.clone());
    }
    if let Some(r) = &resume {
        extra.push("--resume".into());
        extra.push(r.clone());
        // A fork must be told to fork: `--resume` alone continues the source
        // transcript, which is the opposite of what the picker's button says.
        if fork_of.is_some() {
            extra.push("--fork-session".into());
        }
    }
    // The host owns the transcript path it writes to `sessions`, so a fresh
    // session must use the id the host just minted. A resumed one does
    // not: the engine derives the id from `--resume`, and the row above points
    // at that same file. Sending `--session-id` there would override the
    // resume and fork silently.
    if resume.is_none() {
        extra.push("--session-id".into());
        extra.push(id.clone());
    }

    let resource_dir = app.path().resource_dir().ok();
    let child = EngineChild::spawn(&id, &workdir, &extra, resource_dir.as_deref())
        .map_err(|e| e.to_string())?;

    // A fresh subscription for this host's pump: the engine stream reaches the
    // renderer as soon as the child exists, `system:init` included. The same
    // reader feeds the live index, so a message is in SQLite the moment it is
    // on the wire -- the file tailer only backfills what this process missed.
    let live = crate::engine::ingester::LiveIndex::new(db.clone(), &project_id, &id);
    if let Ok(mut l) = host.live.lock() {
        l.insert(id.clone(), live.clone());
    }
    let mut rx = child.events.subscribe();
    let tx = host.events.clone();
    tauri::async_runtime::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            live.line(&ev.line).await;
            if tx.send(ev).is_err() {
                break;
            }
        }
    });

    host.insert_child(&id, child.clone()).await;

    // The session row is written before the first prompt, so a GUI crash during
    // spawn still leaves a session that can be reopened.
    let scope = Scope::root();
    let pid = child.pid.map(|p| p as i64).unwrap_or(0);
    db.write(&scope, |tx| {
        crate::db::history::upsert_session(
            tx,
            &project_id,
            workspace_id.as_deref(),
            &id,
            None,
            model.as_deref().unwrap_or("claude-opus-5-5"),
            effort.as_deref(),
            permission_mode.as_deref().unwrap_or("default"),
            &transcript.to_string_lossy(),
            Some(pid),
            fork_of.as_deref(),
        )?;
        Ok(())
    })
    .map_err(|e| e.to_string())?;

    host.watcher
        .register(transcript.clone(), id.clone(), project_id.clone(), 0)
        .await;

    let init_child = child.clone();
    let events = host.events.clone();
    let sid = id.clone();
    let mode = permission_mode.clone().unwrap_or_else(|| "default".to_string());
    tauri::async_runtime::spawn(async move {
        match init_child
            .request(
                "initialize",
                json!({
                    "clientInfo": {"name": "kiri", "version": env!("CARGO_PKG_VERSION")},
                    "permissionMode": mode,
                }),
                Duration::from_secs(5),
            )
            .await
        {
            Ok(reply) => {
                let inner = reply.get("response").cloned().unwrap_or(reply);
                let mut obj = inner.as_object().cloned().unwrap_or_default();
                obj.insert("type".into(), json!("system"));
                obj.insert("subtype".into(), json!("init"));
                obj.insert("session_id".into(), json!(sid));
                let _ = events.send(crate::engine::child::EngineEvent {
                    session_id: sid.clone(),
                    line: Value::Object(obj),
                });
            }
            Err(e) => eprintln!("[kiri] initialize: {e}"),
        }
    });

    let _ = app.emit(
        "engine://spawned",
        json!({"sessionId": id, "pid": pid, "transcript": transcript}),
    );

    Ok(json!({"sessionId": id, "pid": pid}))
}

#[tauri::command]
pub async fn send_prompt(
    session_id: String,
    text: String,
    host: State<'_, Arc<Host>>,
) -> Result<(), String> {
    let child = host
        .child(&session_id)
        .await
        .ok_or_else(|| format!("no live engine for session {session_id}"))?;

    // The turn row opens before the write, so a crash in between still leaves
    // the prompt in the index to reopen. Mode/effort/model are read from
    // the session row, not guessed: the turn must attribute what actually ran.
    // The lookup is by session id, so it carries the root scope (no project
    // bound); a scope built from the session id would fail registration and
    // silently return blank attribution.
    let ctx = {
        let scope = Scope::root();
        host.db
            .read(&scope, |c| {
                Ok(c.query_row(
                    "select model, coalesce(effort, ''), permission_mode
                       from sessions where id = ?1",
                    [&session_id],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                        ))
                    },
                )
                .ok())
            })
            .ok()
            .flatten()
    };
    if let Ok(live) = host.live.lock() {
        if let Some(l) = live.get(&session_id) {
            if let Some((model, effort, mode)) = ctx {
                l.prompt_sent(&text, &mode, &effort, &model);
            } else {
                l.prompt_sent(&text, "", "", "");
            }
        }
    }

    child.send_user_prompt(&text).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn interrupt_session(
    session_id: String,
    cancel_queued: Option<bool>,
    host: State<'_, Arc<Host>>,
) -> Result<Value, String> {
    let child = host
        .child(&session_id)
        .await
        .ok_or_else(|| format!("no live engine for session {session_id}"))?;
    let reply = child
        .request(
            "interrupt",
            json!({"cancel_queued": cancel_queued.unwrap_or(false)}),
            Duration::from_secs(10),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(reply.get("response").cloned().unwrap_or(reply))
}

#[tauri::command]
pub async fn answer_permission(
    session_id: String,
    request_id: String,
    behavior: String,
    message: Option<String>,
    updated_input: Option<Value>,
    destination: Option<String>,
    rule: Option<Value>,
    answers: Option<Value>,
    response: Option<String>,
    host: State<'_, Arc<Host>>,
) -> Result<(), String> {
    let child = host
        .child(&session_id)
        .await
        .ok_or_else(|| format!("no live engine for session {session_id}"))?;
    let mut extra = json!({});
    if let Some(m) = message.clone() {
        extra["message"] = Value::String(m);
    }
    if let Some(u) = updated_input {
        extra["updated_input"] = u;
    }
    if let Some(d) = destination.clone() {
        extra["destination"] = Value::String(d);
    }
    if let Some(r) = rule.clone() {
        extra["rule"] = r;
    }
    if let Some(a) = answers.clone() {
        extra["answers"] = a;
    }
    if let Some(r) = response.clone() {
        extra["response"] = Value::String(r);
    }
    child
        .answer_permission(&request_id, &behavior, extra)
        .await
        .map_err(|e| e.to_string())?;

    if behavior == "allow" {
        if let Some(r) = rule.clone() {
            let text = r.get("rule").and_then(Value::as_str).or_else(|| r.as_str()).unwrap_or("");
            if !text.is_empty() {
                let (tool, pattern) = match text.split_once('(') {
                    Some((t, rest)) => (t.to_string(), Some(rest.trim_end_matches(')').to_string()).filter(|p| p != "*")),
                    None => (text.to_string(), None),
                };
                let scope_name = match destination.as_deref() {
                    Some("userSettings") => "user",
                    Some("projectSettings") => "project",
                    _ => "local",
                };
                let _ = host.db.write(&Scope::root(), |tx| {
                    let project_id: Option<String> = tx.query_row(
                        "select project_id from sessions where id = ?1",
                        [&session_id],
                        |row| row.get(0),
                    ).ok();
                    if let Some(pid) = project_id {
                        crate::db::decisions::upsert_rule(tx, &pid, &crate::db::decisions::RuleInput {
                            rule_type: "allow".into(),
                            tool,
                            pattern,
                            scope: scope_name.into(),
                            origin: "always_allow".into(),
                            enabled: true,
                        })?;
                    }
                    Ok(())
                });
            }
        }
    }

    let scope = Scope::root();
    if answers.is_some() || response.is_some() {
        let payload = json!({"answers": answers.unwrap_or(Value::Null), "response": response});
        host.db
            .write(&scope, |tx| {
                let won = crate::db::decisions::answer_question(tx, &request_id, &payload)?;
                if !won {
                    return Err(anyhow::anyhow!("too_late"));
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
    } else if matches!(behavior.as_str(), "allow" | "deny") {
        host.db
            .write(&scope, |tx| {
                let won = crate::db::decisions::resolve(
                    tx,
                    &request_id,
                    if behavior == "allow" { "allowed" } else { "denied" },
                    message.as_deref(),
                )?;
                if !won {
                    return Err(anyhow::anyhow!("too_late"));
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn set_permission_mode(
    session_id: String,
    mode: String,
    host: State<'_, Arc<Host>>,
) -> Result<(), String> {
    let child = host
        .child(&session_id)
        .await
        .ok_or_else(|| format!("no live engine for session {session_id}"))?;
    let request_id = format!("mode-{}", uuid::Uuid::new_v4().simple());
    child
        .control(&request_id, "setPermissionMode", json!({"mode": mode}))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_model(
    session_id: String,
    model: String,
    effort: Option<String>,
    host: State<'_, Arc<Host>>,
) -> Result<(), String> {
    let child = host
        .child(&session_id)
        .await
        .ok_or_else(|| format!("no live engine for session {session_id}"))?;
    let request_id = format!("model-{}", uuid::Uuid::new_v4().simple());
    child
        .control(
            &request_id,
            "setModel",
            json!({"model": model, "effort": effort}),
        )
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn context_usage(session_id: String, host: State<'_, Arc<Host>>) -> Result<Value, String> {
    let child = host
        .child(&session_id)
        .await
        .ok_or_else(|| format!("no live engine for session {session_id}"))?;
    let reply = child
        .request("getContextUsage", json!({}), Duration::from_secs(15))
        .await
        .map_err(|e| e.to_string())?;
    Ok(reply.get("response").cloned().unwrap_or(reply))
}

#[tauri::command]
pub async fn compact_session(session_id: String, host: State<'_, Arc<Host>>) -> Result<Value, String> {
    let child = host
        .child(&session_id)
        .await
        .ok_or_else(|| format!("no live engine for session {session_id}"))?;
    let reply = child
        .request("compact", json!({}), Duration::from_secs(60))
        .await
        .map_err(|e| e.to_string())?;
    Ok(reply.get("response").cloned().unwrap_or(reply))
}

#[tauri::command]
pub async fn list_tasks(session_id: String, host: State<'_, Arc<Host>>) -> Result<Value, String> {
    if let Some(child) = host.child(&session_id).await {
        if let Ok(reply) = child.request("listTasks", json!({}), Duration::from_secs(5)).await {
            let inner = reply.get("response").cloned().unwrap_or(reply);
            let tasks = inner.get("tasks").and_then(Value::as_array).cloned()
                .or_else(|| inner.as_array().cloned())
                .unwrap_or_default();
            let scope = Scope::root();
            let _ = host.db.write(&scope, |tx| {
                for t in tasks {
                    let id = t.get("id").and_then(Value::as_str).unwrap_or_default();
                    if id.is_empty() { continue; }
                    let kind = t.get("kind").and_then(Value::as_str).unwrap_or("bash");
                    let subject = t.get("subject").or_else(|| t.get("description")).and_then(Value::as_str).unwrap_or(id);
                    let status = match t.get("status").and_then(Value::as_str).unwrap_or("running") {
                        "running" => "in_progress",
                        "done" => "completed",
                        "error" => "failed",
                        other => other,
                    };
                    crate::db::background::upsert_task(
                        tx, id, &session_id, None, kind, subject,
                        t.get("description").and_then(Value::as_str),
                        t.get("activeForm").and_then(Value::as_str),
                        status, None,
                        t.get("subagentId").and_then(Value::as_str),
                        &t,
                    )?;
                }
                Ok(())
            });
        }
    }
    let scope = Scope::root();
    let rows = host
        .db
        .read(&scope, |c| Ok(crate::db::background::list_tasks(c, &session_id)?))
        .map_err(|e| e.to_string())?;
    Ok(Value::Array(rows))
}

#[tauri::command]
pub async fn stop_task(session_id: String, task_id: String, host: State<'_, Arc<Host>>) -> Result<Value, String> {
    let child = host
        .child(&session_id)
        .await
        .ok_or_else(|| format!("no live engine for session {session_id}"))?;
    let reply = child
        .request("stopTask", json!({"path": task_id, "task_id": task_id}), Duration::from_secs(10))
        .await
        .map_err(|e| e.to_string())?;
    let scope = Scope::root();
    let _ = host.db.write(&scope, |tx| {
        crate::db::background::set_task_status(tx, &task_id, "stopped")?;
        Ok(())
    });
    Ok(reply.get("response").cloned().unwrap_or(reply))
}

#[tauri::command]
pub async fn search_blocks(
    project_id: String,
    query: String,
    limit: Option<i64>,
    host: State<'_, Arc<Host>>,
) -> Result<Value, String> {
    let scope = Scope::read(project_id);
    let hits = host
        .db
        .read(&scope, |c| {
            Ok(crate::db::search::search(
                c,
                &scope.project_id,
                &query,
                limit.unwrap_or(50),
            )?)
        })
        .map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(hits).unwrap_or(Value::Null))
}

#[tauri::command]
pub async fn export_session(
    project_id: String,
    session_id: String,
    host: State<'_, Arc<Host>>,
) -> Result<String, String> {
    let scope = Scope::read(project_id);
    host.db
        .read(&scope, |c| {
            Ok(crate::db::search::export_markdown(
                c,
                &scope.project_id,
                &session_id,
            )?)
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn write_export(path: String, content: String) -> Result<(), String> {
    std::fs::write(&path, content).map_err(|e| e.to_string())
}

/// `readFile` through the engine's own path rules: attachments and the diff
/// viewer must not read what the engine would refuse. The engine owns the
/// ceiling (10 MB) and the refusal text; this is a thin pass-through.
#[tauri::command]
pub async fn read_file(
    session_id: String,
    path: String,
    offset: Option<u64>,
    limit: Option<u64>,
    host: State<'_, Arc<Host>>,
) -> Result<Value, String> {
    let child = host
        .child(&session_id)
        .await
        .ok_or_else(|| format!("no live engine for session {session_id}"))?;
    child
        .request(
            "readFile",
            json!({"path": path, "offset": offset, "limit": limit}),
            Duration::from_secs(30),
        )
        .await
        .map_err(|e| e.to_string())
}

// ---- settings console (S14), task/MCP panel (S10), resume & fork picker (S09) ----

#[tauri::command]
pub async fn list_settings(project_id: String, host: State<'_, Arc<Host>>) -> Result<Value, String> {
    let scope = Scope::read(project_id);
    host.db
        .read(&scope, |c| {
            let mut st = c.prepare(
                "select key, value, source, file_path, updated_at
                   from settings where project_id = ?1 order by key asc, source asc",
            )?;
            let rows = st
                .query_map([scope.project_id().unwrap_or_default()], |r| {
                    Ok(json!({
                        "key": r.get::<_, String>(0)?,
                        "value": r.get::<_, String>(1)?,
                        "source": r.get::<_, String>(2)?,
                        "filePath": r.get::<_, Option<String>>(3)?,
                        "updatedAt": r.get::<_, String>(4)?,
                    }))
                })?
                .filter_map(|r| r.ok())
                .collect::<Vec<_>>();
            Ok(Value::Array(rows))
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn set_setting(
    project_id: String,
    key: String,
    value: String,
    host: State<'_, Arc<Host>>,
) -> Result<(), String> {
    let scope = Scope::write_all(Scope::read(project_id));
    host.db
        .write(&scope, |tx| {
            tx.execute(
                "insert into settings (project_id, key, value, source, updated_at)
                 values (?1, ?2, ?3, 'project', strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                 on conflict (project_id, key, source) do update set
                   value = excluded.value, updated_at = excluded.updated_at",
                rusqlite::params![scope.project_id().unwrap_or_default(), key, value],
            )?;
            Ok(())
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_memory(project_id: String, host: State<'_, Arc<Host>>) -> Result<Value, String> {
    let scope = Scope::read(project_id.clone());
    let path = host
        .db
        .read(&scope, |c| {
            Ok(c.query_row("select path from projects where id = ?1", [&project_id], |r| r.get::<_, String>(0)).ok())
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    if let Some(path) = path {
        let mut dir = std::path::PathBuf::from(path);
        loop {
            for name in ["CLAUDE.md", "KIRI.md", "AGENTS.md"] {
                let p = dir.join(name);
                if p.is_file() {
                    out.push(json!({"path": p.to_string_lossy(), "scope": "project"}));
                }
            }
            if !dir.pop() { break; }
        }
    }
    if let Some(home) = dirs::home_dir() {
        for p in [home.join(".kiri").join("CLAUDE.md"), home.join("CLAUDE.md")] {
            if p.is_file() {
                out.push(json!({"path": p.to_string_lossy(), "scope": "user"}));
            }
        }
    }
    Ok(Value::Array(out))
}

/// The API key never crosses the IPC boundary in either direction. `has_key`
/// is the only question the renderer may ask, and setting one is write-only.
#[tauri::command]
pub async fn vault_status(host: State<'_, Arc<Host>>) -> Result<Value, String> {
    Ok(json!({
        "present": host.vault.is_present(),
        "backend": host.vault.backend(),
        "hasEnvKey": std::env::var("ANTHROPIC_API_KEY").map(|s| !s.trim().is_empty()).unwrap_or(false),
    }))
}

#[tauri::command]
pub async fn vault_set_key(key: String, host: State<'_, Arc<Host>>) -> Result<Value, String> {
    host.vault.set(&key)?;
    Ok(json!({"present": host.vault.is_present()}))
}

#[tauri::command]
pub async fn vault_clear(host: State<'_, Arc<Host>>) -> Result<Value, String> {
    host.vault.clear()?;
    Ok(json!({"present": false}))
}

#[tauri::command]
pub async fn list_subagents(session_id: String, host: State<'_, Arc<Host>>) -> Result<Value, String> {
    let scope = Scope::root();
    let rows = host
        .db
        .read(&scope, |c| Ok(crate::db::accounting::list_subagents(c, &session_id)?))
        .map_err(|e| e.to_string())?;
    Ok(Value::Array(rows))
}

#[tauri::command]
pub async fn cost_rollup(
    project_id: String,
    since: Option<String>,
    host: State<'_, Arc<Host>>,
) -> Result<Value, String> {
    let scope = Scope::read(project_id);
    host.db
        .read(&scope, |c| {
            Ok(crate::db::accounting::cost_rollup(
                c,
                scope.project_id().unwrap_or_default(),
                since.as_deref(),
            )?)
        })
        .map_err(|e| e.to_string())
}

/// Resume / fork picker data: every transcript the engine has on disk for this
/// project, whether or not the index has a session row yet. The transcript is
/// the source of truth, so a file the index never saw is still resumable.
#[tauri::command]
pub async fn list_transcripts(project_id: String, host: State<'_, Arc<Host>>) -> Result<Value, String> {
    let scope = Scope::read(project_id);
    let project_path: Option<String> = host
        .db
        .read(&scope, |c| {
            Ok(c.query_row(
                "select path from projects where id = ?1",
                [scope.project_id().unwrap_or_default()],
                |r| r.get::<_, String>(0),
            )
            .ok())
        })
        .map_err(|e| e.to_string())?;
    let Some(path) = project_path else {
        return Ok(Value::Array(Vec::new()));
    };
    Ok(Value::Array(crate::engine::ingester::list_transcripts(std::path::Path::new(&path))))
}

#[tauri::command]
pub async fn list_checkpoints(
    project_id: String,
    session_id: String,
    host: State<'_, Arc<Host>>,
) -> Result<Value, String> {
    let scope = Scope::read(project_id);
    host.db
        .read(&scope, |c| {
            Ok(crate::db::checkpoints::list_checkpoints(c, &session_id)?)
        })
        .map(|rows| Value::Array(rows))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn checkpoint_diff(
    project_id: String,
    session_id: String,
    checkpoint_id: String,
    host: State<'_, Arc<Host>>,
) -> Result<Value, String> {
    let scope = Scope::read(project_id);
    host.db
        .read(&scope, |c| {
            let row = c.query_row(
                "select c.path, c.existed_pre, b.body
                   from file_checkpoints c
                   left join file_blobs b on b.sha = c.blob_sha
                  where c.id = ?1 and c.session_id = ?2",
                rusqlite::params![checkpoint_id, session_id],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)? != 0,
                        r.get::<_, Option<String>>(2)?,
                    ))
                },
            ).map_err(|e| anyhow::anyhow!(e.to_string()))?;
            let (path, existed_pre, old_text) = row;
            let new_text = std::fs::read_to_string(&path).ok();
            Ok(json!({
                "path": path,
                "existedPre": existed_pre,
                "oldText": old_text,
                "newText": new_text,
            }))
        })
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn restore_checkpoint_file(
    project_id: String,
    session_id: String,
    checkpoint_id: String,
    host: State<'_, Arc<Host>>,
) -> Result<Value, String> {
    let scope = Scope::write_all(Scope::read(project_id));
    host.db
        .write(&scope, |tx| {
            let (path, existed_pre, body) = tx.query_row(
                "select c.path, c.existed_pre, b.body
                   from file_checkpoints c
                   left join file_blobs b on b.sha = c.blob_sha
                  where c.id = ?1 and c.session_id = ?2",
                rusqlite::params![checkpoint_id, session_id],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0, r.get::<_, Option<String>>(2)?)),
            ).map_err(|e| anyhow::anyhow!(e.to_string()))?;
            if !existed_pre {
                let _ = std::fs::remove_file(&path);
                return Ok(json!({"path": path, "deleted": true}));
            }
            let Some(body) = body else {
                return Err(anyhow::anyhow!("binary checkpoint restore is not supported"));
            };
            std::fs::write(&path, body).map_err(|e| anyhow::anyhow!(e.to_string()))?;
            Ok(json!({"path": path, "deleted": false}))
        })
        .map_err(|e| e.to_string())
}

/// Undo every checkpoint a turn recorded, newest path first. `existed_pre`
/// false means the turn created the file, so restore deletes it. This is the
/// one write the host performs on the project tree outside the engine; it is
/// offered as an explicit, named action (S08), never silently.
#[tauri::command]
pub async fn restore_checkpoint(
    project_id: String,
    session_id: String,
    turn_id: i64,
    host: State<'_, Arc<Host>>,
) -> Result<Value, String> {
    let scope = Scope::write_all(Scope::read(project_id));
    host.db
        .write(&scope, |tx| {
            let rows = crate::db::checkpoints::for_turn(tx, &session_id, turn_id)?;
            let mut written: Vec<String> = Vec::new();
            let mut deleted: Vec<String> = Vec::new();
            for (path, sha, existed_pre) in rows {
                if !existed_pre {
                    let _ = std::fs::remove_file(&path);
                    deleted.push(path);
                    continue;
                }
                let Some(sha) = sha else { continue };
                if let Some(body) = crate::db::checkpoints::body_for(tx, &sha)? {
                    if let Some(dir) = std::path::Path::new(&path).parent() {
                        let _ = std::fs::create_dir_all(dir);
                    }
                    std::fs::write(&path, body.as_bytes()).map_err(|e| anyhow::anyhow!(e))?;
                    written.push(path);
                }
            }
            Ok(json!({"written": written, "deleted": deleted}))
        })
        .map_err(|e| e.to_string())
}
