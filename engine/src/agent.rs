// The agent loop.
//
// Transport: newline-delimited JSON on stdin/stdout. A prompt arrives as a
// `user` message; the engine streams assistant blocks and tool results back;
// the loop closes the turn with a `result`. Control requests arrive as
// `control_request` and are answered with the nested `control_response`
// envelope the GUI expects.

use crate::api::Delta;
use crate::exec;
use crate::hooks;
use crate::memory;
use crate::out;
use crate::permissions::{self, Decision, Mode, RuleType};
use crate::protocol::{control_response, Incoming};
use crate::runtime::Runtime;
use crate::session_state::{self, SessionConfig};
use crate::session_state::settings::{parse_rule_string, persist_rule};
use crate::session_state::Settings;
use crate::skills;
use crate::tools;
use crate::tools::ToolResult;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

pub async fn run(cfg: SessionConfig) -> Result<()> {
    session_state::ensure_dirs()?;

    let mut cfg = cfg;
    let settings = Settings::load(&cfg);
    // Consume the settings surface for the model endpoint too: env keeps
    // precedence (the host injects the vault key as ANTHROPIC_API_KEY), then
    // the settings file, then the built-in default. The key is never logged.
    cfg.api_key = settings.api_key();
    cfg.base_url = settings.base_url();
    let hook_defs = if cfg.no_hooks {
        Vec::new()
    } else {
        hooks::parse_settings_hooks(&settings.hooks())
    };
    let skills = skills::load_all(&cfg, &settings);
    let memory = memory::discover(&cfg, &settings);

    let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
    let rt = Runtime::new(
        cfg.clone(),
        settings.clone(),
        tx,
        hook_defs,
        skills.clone(),
        memory.clone(),
        cfg.mode.clone(),
    );

    let writer = tokio::spawn(async move {
        while let Some(v) = rx.recv().await {
            let _ = out::emit(&v).await;
        }
    });

    init_transcript(&rt, &cfg)?;

    // Setup runs once per session, before SessionStart: repo/session bootstrap
    // hooks the user configured.
    let setup_payload = json!({
        "event": "Setup",
        "session_id": cfg.session_id,
        "cwd": cfg.cwd.to_string_lossy(),
    });
    let setup = hooks::run(&rt.hooks(), "Setup", &cfg.cwd, None, &setup_payload).await;
    if let Some(text) = setup.append {
        rt.system("informational", json!({"message": text, "source": "Setup"}));
    }

    let start_payload = json!({
        "session_id": cfg.session_id,
        "cwd": cfg.cwd.to_string_lossy(),
    });
    let start = hooks::run(&rt.hooks(), "SessionStart", &cfg.cwd, None, &start_payload).await;

    // Discover stdio MCP servers once at init; the namespaced schemas are
    // injected into every tool list from this result. A server that fails
    // reports its error here and contributes no tools.
    let mcp_servers = crate::mcp::list(&rt).await;
    rt.set_mcp_servers(mcp_servers.clone());

    rt.system(
        "init",
        json!({
            "cwd": cfg.cwd.to_string_lossy(),
            "model": cfg.model,
            "effort": cfg.effort,
            "permissionMode": cfg.mode.as_str(),
            "permission_modes": Mode::all(),
            "tools": tools::definitions(),
            "mcp_servers": mcp_servers,
            "capabilities": {
                "streaming": cfg.partial_messages,
                "control_channel": true,
                "output_styles": ["default", "concise", "explanatory"],
                "slash_commands": slash_commands(),
            },
            "output_style": "default",
            "available_output_styles": ["default", "concise", "explanatory"],
            "account": {"kind": "api_key", "key_present": !cfg.api_key.is_empty()},
            "pid": std::process::id(),
            "settings": settings.snapshot(),
            "memory_files": memory
                .iter()
                .map(|m| json!({"path": m.path.to_string_lossy(), "scope": m.scope}))
                .collect::<Vec<_>>(),
            "skills": skills::to_json(&skills),
            "hook_events": hooks::EVENTS,
            "session_start_context": start.append,
        }),
    );

    if let Some(prompt) = cfg.one_shot_prompt.clone() {
        let user = json!({
            "type": "user",
            "message": {"role": "user", "content": [{"type": "text", "text": prompt}]}
        });
        // One-shot print mode: one prompt, one turn, then exit. A failure is
        // still a `result` -- the same envelope the streaming mode sends -- so
        // a reader never has to special-case why the loop stopped.
        let outcome = turn(&rt, &user).await;
        fire_stop_hook(&rt, &outcome).await;
        match outcome {
            Ok(()) => {
                let envelope = completion_envelope(&rt);
                let _ = rt.record(envelope.clone());
                rt.emit(envelope);
            }
            Err(e) => emit_error_result(&rt, &e.to_string()),
        }
        drop(rt);
        let _ = writer.await;
        return Ok(());
    }

    control_loop(&rt).await;

    // stdin closed: the session is ending. SessionEnd hooks run here, once,
    // with the same event stream the GUI has been reading, and their stdout
    // becomes an `informational` notice the renderer shows before the socket
    // (the pipe) goes quiet.
    let end_payload = json!({
        "event": "SessionEnd",
        "session_id": rt.session_id(),
        "reason": "stdin_eof",
    });
    let end = hooks::run(&rt.hooks(), "SessionEnd", &rt.cwd(), None, &end_payload).await;
    if let Some(text) = end.append {
        rt.system("informational", json!({"message": text, "source": "SessionEnd"}));
    }

    rt.system("worker_shutting_down", json!({}));
    drop(rt);
    let _ = writer.await;
    Ok(())
}

/// The Stop lifecycle hook fires when the main agent finishes a turn, whether
/// it answered or failed. Subagent turns have their own SubagentStop event.
async fn fire_stop_hook(rt: &Arc<Runtime>, outcome: &Result<()>) {
    if rt.is_subagent {
        return;
    }
    let payload = json!({
        "event": "Stop",
        "session_id": rt.session_id(),
        "ok": outcome.is_ok(),
        "error": outcome.as_ref().err().map(|e| e.to_string()),
    });
    let _ = hooks::run(&rt.hooks(), "Stop", &rt.cwd(), None, &payload).await;
}

fn init_transcript(rt: &Arc<Runtime>, cfg: &SessionConfig) -> Result<()> {
    if !cfg.persist {
        return Ok(());
    }
    let path = session_state::transcript_path(&cfg.cwd, &cfg.session_id);
    if let Some(target) = &cfg.resume {
        let src = session_state::find_transcript(target)?;
        let history = session_state::read_transcript(&src)?;
        rt.set_transcript(Some(path.clone()));
        rt.replay(&history, cfg.fork_session)?;
        // Recon: fork = "new session id, same history". `replay` only builds the
        // model's in-memory context, so the fork's own transcript must be seeded
        // with the source's entries here, before the first append, or opening the
        // fork shows a blank conversation. A resume appends to the same file and
        // needs no copy.
        if cfg.fork_session {
            if let Err(e) = session_state::copy_transcript(&src, &path) {
                rt.system(
                    "informational",
                    json!({
                        "level": "warning",
                        "message": format!("fork transcript not seeded: {e}"),
                        "resumed_from": src.to_string_lossy(),
                    }),
                );
            }
        }
        rt.system(
            "informational",
            json!({
                "message": format!("{} {}", if cfg.fork_session { "forked" } else { "resumed" }, src.display()),
                "resumed_from": src.to_string_lossy(),
                "fork": cfg.fork_session,
            }),
        );
        return Ok(());
    }
    if cfg.continue_session {
        if let Some(src) = session_state::most_recent_transcript(&cfg.cwd) {
            let history = session_state::read_transcript(&src)?;
            rt.set_transcript(Some(path.clone()));
            rt.replay(&history, cfg.fork_session)?;
            rt.system(
                "informational",
                json!({
                    "message": format!("continued {}", src.display()),
                    "resumed_from": src.to_string_lossy(),
                }),
            );
            return Ok(());
        }
    }
    rt.set_transcript(Some(path));
    Ok(())
}

/// Read stdin line by line and dispatch.
///
/// The reader never runs a turn itself: a `user` line is queued and the worker
/// task drains the queue. Control requests are answered inline, on the reader,
/// precisely so that `interrupt`, a permission reply and `setPermissionMode`
/// reach the session while its turn is still streaming. Queuing them behind the
/// model would make the stop button wait for the very answer it is meant to stop.
pub async fn control_loop(rt: &Arc<Runtime>) {
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();

    let worker = {
        let rt = rt.clone();
        tokio::spawn(async move { worker_loop(rt).await })
    };

    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        handle_line(rt, &line).await;
    }

    // EOF on stdin is the GUI's clean end of session. The worker finishes the
    // prompt already in flight, then exits once the queue is empty; it is never
    // abandoned mid-tool, so a turn cannot be half-written to the transcript.
    // The signal must be set before the wake-up, or the worker re-parks and the
    // join below hangs.
    rt.stdin_closed.store(true, Ordering::SeqCst);
    rt.prompt_ready.notify_waiters();
    let _ = worker.await;
}

async fn worker_loop(rt: Arc<Runtime>) {
    while let Some(prompt) = rt.take_prompt().await {
        // Every turn ends in exactly one `result` -- success or failure. A
        // turn that produced an answer used to end in silence, which left the
        // GUI's turn row open forever and (per recon) is not the contract.
        let outcome = turn(&rt, &prompt).await;
        fire_stop_hook(&rt, &outcome).await;
        match outcome {
            Ok(()) => {
                let envelope = completion_envelope(&rt);
                let _ = rt.record(envelope.clone());
                rt.emit(envelope);
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("interrupted") {
                    let _ = rt.record(json!({
                        "type": "system",
                        "subtype": "interrupted",
                        "sessionId": rt.session_id(),
                        "uuid": uuid::Uuid::new_v4().to_string(),
                        "timestamp": chrono::Utc::now()
                            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    }));
                }
                emit_error_result(&rt, &msg);
            }
        }
    }
}

async fn handle_line(rt: &Arc<Runtime>, line: &str) {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            rt.system("informational", json!({
                "level": "error",
                "message": format!("bad ndjson line: {e}"),
            }));
            return;
        }
    };

    match serde_json::from_value::<Incoming>(value.clone()) {
        Ok(Incoming::User(user)) => {
            rt.enqueue_prompt(json!({
                "type": "user",
                "message": user.message,
                "parent_tool_use_id": user.parent_tool_use_id,
            }));
        }
        Ok(Incoming::Control(req)) => {
            // Control is answered inline and runs while a turn streams: a
            // permission reply, an interrupt or a model switch must not wait
            // for the model to finish talking.
            handle_control(rt, req).await;
        }
        Ok(Incoming::ControlResponse(resp)) => {
            let inner = resp.response;
            rt.resolve_pending(&inner.request_id, inner.response.unwrap_or(Value::Null));
        }
        Ok(Incoming::Unknown) => {}
        Err(e) => {
            rt.system("informational", json!({
                "level": "error",
                "message": format!("unrecognized message: {e}"),
            }));
        }
    }
}

/// The turn: one user prompt drives the model and any tool calls it asks for,
/// until a text-only answer arrives. One turn = one `result`.
///
/// Returns `Err` only for a turn that did not complete (interrupt, max_turns,
/// budget, API failure); `Ok` is a finished answer. The caller turns either into
/// the single `result` envelope, so a successful turn no longer ends in silence.
pub async fn turn(rt: &Arc<Runtime>, user: &Value) -> Result<()> {
    rt.reset_cancel();
    rt.api_client().reset_cancel();

    let prompt_blocks = extract_blocks(user);
    if prompt_blocks.is_empty() {
        return Ok(());
    }

    // UserPromptSubmit hooks may append context to this prompt.
    let payload = json!({
        "event": "UserPromptSubmit",
        "session_id": rt.session_id(),
        "prompt": prompt_blocks,
    });
    let hook = hooks::run(&rt.hooks(), "UserPromptSubmit", &rt.cwd(), None, &payload).await;
    let mut blocks = prompt_blocks;
    if let Some(extra) = hook.append {
        blocks.push(json!({"type": "text", "text": extra}));
    }

    // The transcript is the source of truth; a prompt that is not written to it
    // cannot be resumed, and the GUI's list would show a session it cannot open.
    let prompt_uuid = uuid::Uuid::new_v4().to_string();
    let _ = rt.record(json!({
        "type": "user",
        "message": {"role": "user", "content": blocks},
        "sessionId": rt.session_id(),
        "uuid": prompt_uuid,
    }));
    rt.history.lock().unwrap().push(json!({"role": "user", "content": blocks}));

    let mut turn_index: u32 = 0;
    let max_turns = rt.cfg.max_turns.unwrap_or(0);
    rt.turns.fetch_add(1, Ordering::SeqCst);

    loop {
        turn_index += 1;
        if max_turns > 0 && turn_index > max_turns {
            return Err(anyhow!("max_turns reached ({max_turns})"));
        }
        if rt.is_cancelled() {
            return Err(anyhow!("interrupted"));
        }
        if let Some(cap) = rt.cfg.max_budget_micros {
            if rt.total_cost_micros() >= cap {
                rt.record_denial(
                    "",
                    "",
                    &json!({}),
                    "max_budget_usd reached",
                    "budget",
                );
                return Err(anyhow!("max_budget_usd reached"));
            }
        }

        let msg = match one_model_step(rt).await? {
            Some(m) => m,
            None => return Ok(()), // interrupted before a full step
        };

        let tool_uses: Vec<Value> = msg
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
            .collect();

        if tool_uses.is_empty() {
            return Ok(());
        }

        // Run the tool calls. Read-only ones may run concurrently; the whole
        // result set goes back as ONE user message (the API requires it).
        let results = run_tool_calls(rt, &tool_uses).await;

        rt.record(json!({
            "type": "user",
            "message": {"role": "user", "content": results},
            "sessionId": rt.session_id(),
            "uuid": uuid::Uuid::new_v4().to_string(),
        }))?;
        rt.emit(json!({
            "type": "user",
            "message": {"role": "user", "content": results},
            "session_id": rt.session_id(),
            "parent_tool_use_id": rt.parent_tool_use_id,
        }));

        rt.history.lock().unwrap().push(json!({
            "role": "user",
            "content": results,
        }));
    }
}

fn extract_blocks(user: &Value) -> Vec<Value> {
    let content = user.get("message").and_then(|m| m.get("content"));
    match content {
        Some(Value::String(s)) => vec![json!({"type": "text", "text": s})],
        Some(Value::Array(a)) => a.clone(),
        _ => Vec::new(),
    }
}

/// One request/response cycle with the model. Streams deltas to the GUI,
/// records the transcript, and returns the assistant message envelope.
async fn one_model_step(rt: &Arc<Runtime>) -> Result<Option<Value>> {
    let force = rt.compact_requested.swap(false, Ordering::SeqCst);
    if crate::compact::compact_if_needed(rt, force).await? && force {
        rt.system("informational", json!({"message": "context compacted"}));
    }

    let system = build_system(rt);
    let tools = build_tools(rt);

    let mut request = json!({
        "model": effective_model(rt),
        "max_tokens": 32000,
        "system": system,
        "messages": rt.history.lock().unwrap().clone(),
    });
    if !tools.is_empty() {
        request["tools"] = Value::Array(tools);
    }
    let effort = rt.effective_effort();
    request["output_config"] = json!({"effort": effort});

    let tx = rt.tx.clone();
    let partial = rt.cfg.partial_messages;

    let mut on_delta = |d: Delta| -> Result<()> {
        if !partial {
            return Ok(());
        }
        let ev = match d {
            Delta::MessageStart { usage, model } => json!({
                "type": "stream_event",
                "event": {"type": "message_start", "message": {"model": model, "usage": usage}},
            }),
            Delta::BlockStart { index, block } => json!({
                "type": "stream_event",
                "event": {"type": "content_block_start", "index": index, "content_block": block},
            }),
            Delta::BlockDelta { index, delta } => json!({
                "type": "stream_event",
                "event": {"type": "content_block_delta", "index": index, "delta": delta},
            }),
            Delta::BlockStop { index } => json!({
                "type": "stream_event",
                "event": {"type": "content_block_stop", "index": index},
            }),
            Delta::MessageDelta { stop_reason, usage } => json!({
                "type": "stream_event",
                "event": {"type": "message_delta", "delta": {"stop_reason": stop_reason}, "usage": usage},
            }),
            Delta::Ping => return Ok(()),
        };
        let _ = tx.send(ev);
        Ok(())
    };

    let c = rt.api_client().stream(&request, &mut on_delta).await?;

    rt.add_usage(&c.model, &c.usage);
    rt.last_input_tokens.store(
        (c.usage.input_tokens
            + c.usage.cache_creation_input_tokens
            + c.usage.cache_read_input_tokens) as usize,
        Ordering::Relaxed,
    );

    let message = json!({
        "id": c.id,
        "type": "message",
        "role": "assistant",
        "model": c.model,
        "content": c.content,
        "stop_reason": c.stop_reason,
        "usage": {
            "input_tokens": c.usage.input_tokens,
            "cache_creation_input_tokens": c.usage.cache_creation_input_tokens,
            "cache_read_input_tokens": c.usage.cache_read_input_tokens,
            "output_tokens": c.usage.output_tokens,
        },
    });

    rt.record(json!({
        "type": "assistant",
        "message": message,
        "sessionId": rt.session_id(),
        "uuid": uuid::Uuid::new_v4().to_string(),
    }))?;

    // Recon: an assistant answer arrives one message per content block, joined
    // by message id. A GUI that wants one bubble groups on `message.id`.
    for block in c.content.iter().cloned() {
        let envelope = json!({
            "id": c.id,
            "type": "message",
            "role": "assistant",
            "model": c.model,
            "content": [block],
            "stop_reason": c.stop_reason,
            "usage": {
                "input_tokens": c.usage.input_tokens,
                "cache_creation_input_tokens": c.usage.cache_creation_input_tokens,
                "cache_read_input_tokens": c.usage.cache_read_input_tokens,
                "output_tokens": c.usage.output_tokens,
            },
        });
        rt.emit(json!({
            "type": "assistant",
            "message": envelope,
            "session_id": rt.session_id(),
            "parent_tool_use_id": rt.parent_tool_use_id,
        }));
    }

    // Append the assistant message. (Overwriting the last entry here used to
    // erase the user prompt / tool results from the model's context and kept
    // the history too short for compaction to ever trigger.)
    rt.history
        .lock()
        .unwrap()
        .push(json!({"role": "assistant", "content": c.content}));

    Ok(Some(json!({
        "role": "assistant",
        "content": c.content,
    })))
}

pub fn build_system(rt: &Arc<Runtime>) -> String {
    let mem = memory::render(&rt.memory);
    let sk = skills::listing(&rt.skills(), max_skill_desc(rt));
    let mut s = crate::prompt::with_memory(&mem, &sk, &rt.cwd().to_string_lossy());
    // A subagent defined in .kiri/agents/<name>.md carries its own instructions;
    // they are the agent's system prompt, not a suggestion.
    if let Some(def_prompt) = rt.agent_prompt.lock().ok().and_then(|g| g.clone()) {
        if !def_prompt.trim().is_empty() {
            s.push_str("\n\n");
            s.push_str(&def_prompt);
        }
    }
    s
}

fn max_skill_desc(rt: &Arc<Runtime>) -> usize {
    rt.setting_u64("skillListingMaxDescChars")
        .map(|v| v as usize)
        .unwrap_or(1024)
}

/// Tool definitions: deferred MCP tools are not sent as schemas; they are
/// discovered through ToolSearch and then appended by `loaded_tools`.
pub fn build_tools(rt: &Arc<Runtime>) -> Vec<Value> {
    let mut out: Vec<Value> = tools::definitions()
        .into_iter()
        .map(|mut d| {
            let mut schema_tool = Value::Object(serde_json::Map::new());
            schema_tool["name"] = d["name"].clone();
            schema_tool["description"] = d["description"].clone();
            schema_tool["input_schema"] = d["input_schema"].clone();
            schema_tool
        })
        .collect();

    let allowed = rt.cfg.allowed_tools.clone();
    let denied = rt.cfg.disallowed_tools.clone();
    out.retain(|t| {
        let name = t["name"].as_str().unwrap_or("");
        let allowed_ok = allowed.as_ref().map(|a| a.iter().any(|x| x == name)).unwrap_or(true);
        let denied_ok = !denied.as_ref().map(|d| d.iter().any(|x| x == name)).unwrap_or(false);
        allowed_ok && denied_ok
    });

    // MCP tools come from the servers discovered at init / reinitialize
    // (`mcp::list`): each connected stdio server contributes its live
    // tools/list under mcp__<server>__<tool>. A server that only carries a
    // static tools[] declaration in settings (no discovery yet) still
    // contributes those, so a configured schema is never silently dropped.
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut push_mcp = |out: &mut Vec<Value>, sname: &str, tool: &Value| {
        let tname = tool
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("tool")
            .to_string();
        let full = format!("mcp__{sname}__{tname}");
        if !seen.insert(full.clone()) {
            return;
        }
        let allowed_ok = allowed
            .as_ref()
            .map(|a| a.iter().any(|x| x == &full || x == sname))
            .unwrap_or(true);
        let denied_ok = !denied
            .as_ref()
            .map(|d| d.iter().any(|x| x == &full))
            .unwrap_or(false);
        if !(allowed_ok && denied_ok) {
            return;
        }
        out.push(json!({
            "name": full,
            "description": tool.get("description").cloned().unwrap_or(Value::Null),
            "input_schema": tool.get("inputSchema").cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
        }));
    };

    for server in rt.mcp_servers() {
        let sname = server
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("server")
            .to_string();
        for tool in server
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            push_mcp(&mut out, &sname, &tool);
        }
    }
    for server in rt.setting_array("mcpServers") {
        let sname = server
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("server")
            .to_string();
        for tool in server
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            push_mcp(&mut out, &sname, &tool);
        }
    }

    if let Ok(loaded) = rt.loaded_deferred.lock() {
        for name in loaded.iter() {
            if let Ok(d) = rt.deferred_tools.lock() {
                if let Some(def) = d.get(name) {
                    out.push(def.clone());
                }
            }
        }
    }
    out
}

fn effective_model(rt: &Arc<Runtime>) -> String {
    rt.effective_model()
}

fn model_catalogue(rt: &Arc<Runtime>) -> Value {
    const KNOWN: &[(&str, &str, u64, u64)] = &[
        ("claude-opus-5-5", "Opus 5.5", 4_000_000, 20_000_000),
        ("claude-opus-5", "Opus 5", 5_000_000, 25_000_000),
        ("claude-sonnet-5-5", "Sonnet 5.5", 2_000_000, 10_000_000),
        ("claude-haiku-5-5", "Haiku 5.5", 100_000, 500_000),
    ];
    let mut out: Vec<Value> = KNOWN
        .iter()
        .map(|(id, label, inp, outp)| {
            json!({
                "id": id,
                "label": label,
                "input_micros_per_mtok": inp,
                "output_micros_per_mtok": outp,
            })
        })
        .collect();
    for extra in rt.setting_array("models") {
        if extra.get("id").is_some() && !out.iter().any(|m| m["id"] == extra["id"]) {
            out.push(extra);
        }
    }
    Value::Array(out)
}

fn slash_commands() -> Value {
    json!([
        {"name": "compact", "description": "compress the conversation now"},
        {"name": "context", "description": "show context usage"},
        {"name": "clear", "description": "start a new transcript"},
        {"name": "cost", "description": "show the session cost"},
    ])
}

/// Run every tool call in one assistant message.
///
/// Read-only calls may run concurrently (recon); a call that can decide a
/// permission prompt is serialised and runs in call order, so the user is
/// never asked to approve a write before the read it depends on has finished.
async fn run_tool_calls(rt: &Arc<Runtime>, calls: &[Value]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::with_capacity(calls.len());
    let mut batch: Vec<(usize, Value)> = Vec::new();

    for (i, call) in calls.iter().enumerate() {
        let name = call.get("name").and_then(Value::as_str).unwrap_or("");
        if name_is_read_only(name) {
            batch.push((i, call.clone()));
        } else {
            for (_j, v) in drain_batch(rt, &mut batch).await {
                out.push(v);
            }
            out.push(run_one(rt.clone(), call.clone()).await);
        }
    }
    for (_j, v) in drain_batch(rt, &mut batch).await {
        out.push(v);
    }
    out
}

fn name_is_read_only(name: &str) -> bool {
    matches!(
        name,
        "Read" | "Glob" | "Grep" | "BashOutput" | "TaskGet" | "TaskList" | "Skill" | "ToolSearch"
    )
}

/// Await a batch of read-only calls and return their results in submission order.
async fn drain_batch(rt: &Arc<Runtime>, batch: &mut Vec<(usize, Value)>) -> Vec<(usize, Value)> {
    let mut handles = Vec::new();
    for (i, call) in batch.drain(..) {
        let rt2 = rt.clone();
        handles.push((i, tokio::spawn(async move { run_one(rt2, call).await })));
    }
    let mut out = Vec::new();
    for (i, h) in handles {
        let v = match h.await {
            Ok(v) => v,
            Err(e) => json!({
                "type": "tool_result",
                "tool_use_id": "unknown",
                "content": [{"type": "text", "text": format!("tool task failed: {e}")}],
                "is_error": true,
            }),
        };
        out.push((i, v));
    }
    out
}

/// Ask the model to judge the `prompt` hooks that match this tool call.
///
/// A `type: prompt` hook is a question the user put in settings, not code: the
/// engine sends it the event and the call input and expects one word back. That
/// makes a judge call cheap enough to run per call and keeps the hook file
/// editable by someone who cannot write a shell script.
///
/// Failure is not permission. If a hook cannot be judged -- API error, timeout,
/// unparseable answer -- the call falls through to the ordinary six-step
/// pipeline, which will prompt the user. Denying on a judge failure would let a
/// flaky network lock the app.
async fn judge_prompt_hooks(
    rt: &Arc<Runtime>,
    hooks: &[hooks::HookDef],
    tool: &str,
    input: &Value,
    payload: &Value,
) -> Option<Decision> {
    let mut matching = hooks
        .iter()
        .filter(|h| h.hook_type == "prompt" && h.enabled && hooks::matches(h, "PreToolUse", Some(tool)));
    let first = matching.next()?;
    // The first matching prompt hook is the judge.

    let question = first
        .command
        .clone()
        .unwrap_or_else(|| "Should this tool call be allowed?".to_string());

    let request = json!({
        "model": rt.effective_model(),
        "max_tokens": 16,
        "system": "You are a permission judge for kiri. Answer with exactly one of: allow, deny, ask.",
        "messages": [{
            "role": "user",
            "content": [{
                "type": "text",
                "text": format!(
                    "Rule:\n{question}\n\nTool call:\n{}\n",
                    serde_json::to_string_pretty(payload).unwrap_or_default()
                ),
            }],
        }],
    });

    let no_stream = |_: crate::api::Delta| -> Result<()> { Ok(()) };
    let mut sink = no_stream;
    let Ok(done) = rt.api_client().stream(&request, &mut sink).await else {
        return None;
    };
    let answer: String = done
        .content
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();

    if answer.contains("deny") {
        Some(Decision::Deny)
    } else if answer.contains("ask") {
        Some(Decision::Ask)
    } else if answer.contains("allow") {
        Some(Decision::Allow)
    } else {
        None
    }
}

/// One tool call: PreToolUse hook, permission pipeline, execute, PostToolUse.
///
/// This is the single gate for BOTH the parent loop and subagents
/// (`run_isolated`): hooks, the six-step evaluation, the GUI prompt through
/// the shared pending queue, and the denial log all live here.
pub(crate) async fn run_one(rt: Arc<Runtime>, call: Value) -> Value {
    let id = call
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let name = call
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let mut input = call.get("input").cloned().unwrap_or_else(|| json!({}));

    let base = json!({
        "type": "tool_result",
        "tool_use_id": id,
    });

    let hook_defs = rt.hooks();
    let pre_payload = json!({
        "event": "PreToolUse",
        "session_id": rt.session_id(),
        "tool_name": name,
        "tool_use_id": id,
        "tool_input": input,
    });
    let pre = hooks::run(&hook_defs, "PreToolUse", &rt.cwd(), Some(&name), &pre_payload).await;
    if let Some(reason) = pre.deny_reason() {
        rt.record_denial(&name, &id, &input, &reason, "hook");
        return merge(base, denied(&reason));
    }

    // `prompt` hooks are the one kind the pure hook runner cannot decide: they
    // ask the model. The loop is the only place with an API client, so it judges
    // them here and folds the verdict into the same gate a `command` hook's exit
    // code produces. A deny from either wins; nothing below can lift it.
    let prompt_gate = judge_prompt_hooks(&rt, &hook_defs, &name, &input, &pre_payload).await;
    if let Some(Decision::Deny) = prompt_gate {
        let reason = format!("hook: prompt hook denied {name}");
        rt.record_denial(&name, &id, &input, &reason, "hook");
        return merge(base, denied(&reason));
    }

    let risk = tools::risk(&name);
    let in_ws = read_target_in_workspace(&rt, &name, risk, &input);
    let verdict = permissions::evaluate(
        rt.mode(),
        risk,
        &name,
        &input,
        &rt.rules(),
        pre.gate().or(prompt_gate),
        in_ws,
    );

    match verdict.decision {
        Decision::Allow => {}
        Decision::Deny => {
            rt.record_denial(&name, &id, &input, &verdict.reason, "permission");
            return merge(base, denied(&verdict.reason));
        }
        _ => {
            let answer = ask_permission(&rt, &call, &verdict).await;
            match answer.get("behavior").and_then(Value::as_str) {
                Some("allow") => {
                    if let Some(updated) = answer.get("updated_input") {
                        input = updated.clone();
                    }
                    if answer.get("destination").is_some() || answer.get("rule").is_some() {
                        let dest = answer
                            .get("destination")
                            .and_then(Value::as_str)
                            .unwrap_or("localSettings")
                            .to_string();
                        // The GUI's explicit rule wins over the engine's
                        // suggestion: the user clicked a specific chip.
                        let rule = answer
                            .get("rule")
                            .and_then(rule_from_answer)
                            .unwrap_or_else(|| {
                                permissions::rule_from_suggestion(&name, &input, &dest)
                            });
                        rt.push_rule(rule.clone());
                        // A choice that dies with the process is not a choice:
                        // write it back to the settings file it belongs to.
                        let persisted = persist_rule(&dest, &rt.cfg.cwd, &rule);
                        rt.system(
                            "informational",
                            json!({
                                "level": if persisted.is_err() { "warning" } else { "info" },
                                "message": match &persisted {
                                    Ok(p) => format!(
                                        "rule saved to {}: {} {}",
                                        p.display(),
                                        rule.tool,
                                        rule.pattern.as_deref().unwrap_or("*")
                                    ),
                                    Err(e) => format!("rule saved for this session only: {e}"),
                                },
                                "rule": rule,
                                "destination": dest.clone(),
                                "persisted": persisted.is_ok(),
                            }),
                        );
                    }
                }
                Some("deny") => {
                    let msg = answer
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("denied")
                        .to_string();
                    rt.record_denial(&name, &id, &input, &msg, "gui_answer");
                    return merge(base, denied(&msg));
                }
                _ => {
                    let msg = answer
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("no_decision");
                    rt.record_denial(&name, &id, &input, msg, "gui_answer");
                    return merge(base, denied("no_decision"));
                }
            }
        }
    }

    let started = std::time::Instant::now();
    let result = exec::execute(&rt, &name, &input).await;
    let duration_ms = started.elapsed().as_millis() as u64;

    let post_payload = json!({
        "event": "PostToolUse",
        "session_id": rt.session_id(),
        "tool_name": name,
        "tool_use_id": id,
        "tool_input": input,
        "tool_result": result.content,
        "is_error": result.is_error,
    });
    let post = if result.is_error {
        hooks::run(&hook_defs, "PostToolUseFailure", &rt.cwd(), Some(&name), &post_payload).await
    } else {
        hooks::run(&hook_defs, "PostToolUse", &rt.cwd(), Some(&name), &post_payload).await
    };

    let mut content = result.content;
    if let Some(replacement) = post.mutate {
        content = vec![json!({"type": "text", "text": replacement})];
    }

    let mut out = base;
    out["content"] = Value::Array(content);
    if result.is_error {
        out["is_error"] = Value::Bool(true);
    }
    out["duration_ms"] = json!(duration_ms);
    if rt.parent_tool_use_id.is_some() {
        out["agent_id"] = json!(rt.agent_id);
    }
    out
}

/// Parse the rule the GUI sent with an always-allow answer. It arrives either
/// as the settings spelling (`Bash(npm test:*)`) or as a rule object.
fn rule_from_answer(v: &Value) -> Option<permissions::Rule> {
    match v {
        Value::String(s) => parse_rule_string(&RuleType::Allow, s),
        Value::Object(_) => permissions::Rule::from_json(v).map(|mut r| {
            r.rule_type = RuleType::Allow;
            r
        }),
        _ => None,
    }
}

/// Whether a read-risk call targets the workspace (live cwd + add_dirs).
/// Non-path reads (Skill, ToolSearch, todo/task registry) are session-local
/// and count as inside. A path that escapes the roots must ask.
fn read_target_in_workspace(rt: &Arc<Runtime>, name: &str, risk: permissions::Risk, input: &Value) -> bool {
    if !matches!(risk, permissions::Risk::Read) {
        return true;
    }
    let cwd = rt.cwd();
    let field = match name {
        "Read" | "NotebookRead" => Some("file_path"),
        "Glob" | "Grep" => Some("path"),
        _ => None,
    };
    let Some(field) = field else { return true };
    let Some(raw) = input.get(field).and_then(Value::as_str) else {
        return true; // defaults to the cwd itself
    };
    match tools::resolve_path(&cwd, raw) {
        Ok(p) => tools::in_workspace(&cwd, &rt.cfg.add_dirs, &p),
        Err(_) => false,
    }
}

/// Ask the GUI for one decision. The engine blocks here; the GUI is the only
/// writer of the answer. A deny, an error, or a closed pipe all resolve to deny.
async fn ask_permission(rt: &Arc<Runtime>, call: &Value, verdict: &permissions::Verdict) -> Value {
    let request_id = format!("perm-{}", uuid::Uuid::new_v4().simple());
    let tool = call
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("tool");
    let input = call.get("input").cloned().unwrap_or_else(|| json!({}));

    let payload = json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {
            "subtype": "can_use_tool",
            "tool_name": tool,
            "input": input,
            "reason": verdict.reason,
            "suggestions": permissions::suggestions(tool, &input),
            "blocked_path": Value::Null,
            "agent_id": rt.agent_id,
            "permission_mode": rt.mode().as_str(),
        }
    });

    let rx = rt.register_pending(&request_id);
    // Notification: the session needs the user's attention (a permission
    // decision). The GUI may render this as a badge or a toast.
    let notif = json!({
        "event": "Notification",
        "session_id": rt.session_id(),
        "message": format!("kiri needs permission to use {tool}"),
        "tool_name": tool,
        "request_id": request_id,
        "agent_id": rt.agent_id,
    });
    let _ = hooks::run(&rt.hooks(), "Notification", &rt.cwd(), None, &notif).await;
    rt.emit(payload.clone());
    rt.record(json!({
        "type": "permission_request",
        "requestId": request_id,
        "toolName": tool,
        "input": input,
        "sessionId": rt.session_id(),
        "uuid": uuid::Uuid::new_v4().to_string(),
    })).ok();

    match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
        Ok(Ok(v)) => v,
        Ok(Err(_)) => json!({"behavior": "deny", "message": "no_decision"}),
        Err(_) => {
            rt.resolve_pending(&request_id, Value::Null);
            json!({"behavior": "deny", "message": "permission_timeout"})
        }
    }
}

fn merge(mut base: Value, content: Value) -> Value {
    base["content"] = content.get("content").cloned().unwrap_or_else(|| json!([content]));
    if content.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
        base["is_error"] = Value::Bool(true);
    }
    base
}

fn denied(reason: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": format!("permission denied: {reason}")}],
        "is_error": true,
    })
}

pub fn result_message(rt: &Arc<Runtime>, subtype: &str, err: Option<&str>) -> Value {
    let cost = rt.total_cost_micros();
    let mut v = json!({
        "type": "result",
        "subtype": subtype,
        "is_error": err.is_some(),
        "model": rt.effective_model(),
        "duration_ms": (chrono::Utc::now() - rt.started_at).num_milliseconds().max(0) as u64,
        "total_cost_usd": cost as f64 / 1_000_000.0,
        "total_cost_micros": cost,
        "usage": rt.usage_snapshot().to_json(),
        "session_id": rt.session_id(),
        "permission_denials": rt.permission_denials.lock().map(|g| g.clone()).unwrap_or_default(),
        "num_turns": rt.turns.load(Ordering::SeqCst),
    });
    if let Some(e) = err {
        v["error"] = Value::String(e.to_string());
    }
    v
}

pub fn emit_error_result(rt: &Arc<Runtime>, msg: &str) {
    let subtype = match msg.split(':').next().unwrap_or("") {
        "interrupted" => "error_interrupted",
        "max_turns reached" => "error_max_turns",
        "max_budget_usd reached" => "error_max_budget_usd",
        _ => "error",
    };
    let envelope = result_message(rt, subtype, Some(msg));
    let _ = rt.record(envelope.clone());
    rt.emit(envelope);
}

/// The `result` that closes a turn which produced an answer.
fn completion_envelope(rt: &Arc<Runtime>) -> Value {
    result_message(rt, "success", None)
}

/// Control requests from the GUI. Every one is answered with the nested
/// envelope; any that the build does not implement answers with an explicit
/// error rather than silence, so the GUI's readiness probe cannot hang.
async fn handle_control(rt: &Arc<Runtime>, req: crate::protocol::ControlRequest) {
    let id = req.request_id.clone();
    let body = req.request;

    let reply = match body.subtype.as_str() {
        "initialize" => Ok(json!({
            "session_id": rt.session_id(),
            "cwd": rt.cwd().to_string_lossy(),
            "model": effective_model(rt),
            "models": model_catalogue(rt),
            "effort": rt.effective_effort(),
            "permission_mode": rt.mode().as_str(),
            "tools": build_tools(rt),
            "commands": slash_commands(),
            "agents": crate::subagents::list(&rt).await,
            "output_style": "default",
            "available_output_styles": ["default", "concise", "explanatory"],
            "account": {"kind": "api_key", "key_present": rt.api_client().has_key()},
            "pid": std::process::id(),
            "settings": rt.settings().snapshot(),
            "memory_files": rt.memory.iter().map(|m| json!({
                "path": m.path.to_string_lossy(), "scope": m.scope
            })).collect::<Vec<_>>(),
            "skills": skills::to_json(&rt.skills()),
            "hooks": rt.hooks().iter().map(|h| json!({
                "event": h.event,
                "matcher": h.matcher,
                "hookType": h.hook_type,
                "command": h.command,
                "url": h.url,
                "timeoutS": h.timeout_s,
                "enabled": h.enabled,
            })).collect::<Vec<_>>(),
            "plugins": rt.setting_array("plugins"),
            "mcp_servers": rt.mcp_servers(),
            "hook_events": hooks::EVENTS,
            "run_config": {
                "max_turns": rt.cfg.max_turns,
                "max_budget_usd": rt.cfg.max_budget_micros.map(|m| m as f64 / 1_000_000.0),
                "input_format": rt.cfg.input_format,
                "output_format": rt.cfg.output_format,
                "persist": rt.cfg.persist,
            },
        })),
        "interrupt" => {
            rt.cancel();
            rt.api_client().cancel();
            rt.deny_pending();
            let still = rt.queued_prompts();
            let cancelled = if body.cancel_queued.unwrap_or(false) {
                rt.cancel_queued_prompts()
            } else {
                0
            };
            Ok(json!({
                "still_queued": still.saturating_sub(cancelled),
                "cancelled": cancelled,
            }))
        }
        "setPermissionMode" => match body.mode.as_deref().map(Mode::parse) {
            Some(Ok(m)) => {
                rt.set_mode(m);
                Ok(json!({"permission_mode": m.as_str()}))
            }
            Some(Err(e)) => Err(e.to_string()),
            None => Err("setPermissionMode: mode is required".into()),
        },
        "setModel" => {
            let old = effective_model(rt);
            let payload = json!({"old_model": old, "new_model": body.model, "effort": body.effort});
            let _ = hooks::run(&rt.hooks(), "PreModelSwitch", &rt.cwd(), None, &payload).await;
            if let Some(m) = &body.model {
                rt.set_model(m.clone());
            }
            if let Some(e) = &body.effort {
                rt.set_effort(e.clone());
            }
            let new = effective_model(rt);
            let mut p2 = payload;
            p2["new_model"] = json!(new);
            let _ = hooks::run(&rt.hooks(), "PostModelSwitch", &rt.cwd(), None, &p2).await;
            Ok(json!({"model": new, "effort": rt.effective_effort()}))
        }
        "getContextUsage" => Ok(context_usage(rt)),
        "reinitialize" => Ok(reinitialize(rt).await),
        "reloadSkills" => {
            // A real reload: re-read the skill directories from disk and swap
            // the listing the system prompt and the GUI see.
            let fresh = skills::load_all(&rt.cfg, &rt.settings());
            rt.set_skills(fresh.clone());
            Ok(json!({"skills": skills::to_json(&fresh)}))
        }
        "reloadPlugins" => {
            // Plugins are declared in settings (and their components live in
            // the --plugin-dir directories); re-read the settings from disk
            // so an edited plugin list takes effect without a restart.
            let fresh = Settings::load(&rt.cfg);
            rt.set_settings(fresh.clone());
            Ok(json!({"plugins": fresh.array("plugins")}))
        }
        "reloadOutputStyles" => Ok(json!({
            "output_styles": ["default", "concise", "explanatory"]
        })),
        "readFile" => match read_file_via_engine(rt, &body).await {
            Ok(v) => Ok(v),
            Err(e) => Err(e),
        },
        "readMcpResource" => match crate::mcp::read_resource(rt, &body).await {
            Ok(v) => Ok(v),
            Err(e) => Err(e),
        },
        "listTasks" => Ok(json!({"tasks": rt.tasks_json()})),
        "stopTask" => match body.path.clone() {
            Some(tid) => Ok(json!({"stopped": rt.stop_task(&tid)})),
            None => Err("stopTask: task id is required".into()),
        },
        "getSettings" => Ok(rt.settings().snapshot()),
        "setSettings" => Err("control_request:setSettings is not supported; edit the settings file and call reinitialize".into()),
        "forkSession" => fork_session(rt),
        "compact" => {
            rt.compact_requested.store(true, Ordering::SeqCst);
            Ok(json!({"compaction": "requested"}))
        }
        other => Err(format!("unknown control_request subtype: {other}")),
    };

    match reply {
        Ok(v) => rt.emit(control_response(&id, v)),
        Err(e) => rt.emit(crate::protocol::control_error(&id, &e)),
    }
}

/// `reinitialize`: reload settings, hooks, skills and MCP discovery from disk,
/// and rebuild the API client from the fresh endpoint/key (env precedence).
/// This is the GUI's reattach path, so it reports the new effective state.
async fn reinitialize(rt: &Arc<Runtime>) -> Value {
    let fresh = Settings::load(&rt.cfg);
    rt.set_settings(fresh.clone());
    rt.set_hooks(if rt.cfg.no_hooks {
        Vec::new()
    } else {
        hooks::parse_settings_hooks(&fresh.hooks())
    });
    rt.set_skills(skills::load_all(&rt.cfg, &fresh));
    rt.set_rules(fresh.rules());

    let mut api = crate::api::Client::new(&fresh.base_url(), &fresh.api_key());
    api.set_events(rt.tx.clone());
    rt.set_api(api);

    let mcp = crate::mcp::list(rt).await;
    rt.set_mcp_servers(mcp.clone());

    json!({
        "settings": fresh.snapshot(),
        "skills": skills::to_json(&rt.skills()),
        "mcp_servers": mcp,
        "account": {"kind": "api_key", "key_present": rt.api_client().has_key()},
    })
}

/// `forkSession`: new session id, same history. The in-memory conversation
/// continues unchanged; the transcript is copied to the new id (re-stamped)
/// and every later entry lands in the fork's file. The source is untouched.
fn fork_session(rt: &Arc<Runtime>) -> std::result::Result<Value, String> {
    let old_id = rt.session_id();
    let new_id = uuid::Uuid::new_v4().to_string();
    let mut seeded = 0usize;
    if let Some(src) = rt.transcript_path() {
        if src.exists() {
            let dst = src.with_file_name(format!("{new_id}.jsonl"));
            seeded = session_state::copy_transcript(&src, &dst)
                .map_err(|e| format!("forkSession: {e}"))?;
            rt.set_transcript(Some(dst));
        }
    }
    rt.set_session_id(new_id.clone());
    rt.system(
        "informational",
        json!({
            "message": format!("forked session {old_id} -> {new_id}"),
            "forked_from": old_id,
        }),
    );
    Ok(json!({
        "fork": true,
        "session_id": new_id,
        "forked_from": old_id,
        "seeded_entries": seeded,
    }))
}

fn context_usage(rt: &Arc<Runtime>) -> Value {
    let history = rt.history.lock().map(|g| g.clone()).unwrap_or_default();
    let mut categories = serde_json::Map::new();
    let mut per_skill: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut per_tool: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut cat_u64: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    let mut total: u64 = 0;

    for m in &history {
        let role = m.get("role").and_then(Value::as_str).unwrap_or("other");
        let text = m.get("content").map(|c| c.to_string()).unwrap_or_default();
        let n = crate::api::estimate_tokens(&text);
        total += n;
        *cat_u64.entry(role.to_string()).or_insert(0) += n;
    }

    let system = build_system(rt);
    let sys_tokens = crate::api::estimate_tokens(&system);
    total += sys_tokens;
    cat_u64.insert("system".into(), sys_tokens);

    let tools_tokens: u64 = crate::tools::definitions()
        .iter()
        .map(|d| crate::api::estimate_tokens(&d.to_string()))
        .sum();
    total += tools_tokens;
    cat_u64.insert("tools".into(), tools_tokens);
    for (k, v) in &cat_u64 {
        categories.insert(k.clone(), json!(v));
    }

    for s in &rt.skills() {
        let n = crate::api::estimate_tokens(&s.body);
        *per_skill.entry(s.name.clone()).or_insert(0) += n;
    }

    for t in crate::tools::definitions() {
        let name = t.get("name").and_then(Value::as_str).unwrap_or("?").to_string();
        let n = crate::api::estimate_tokens(&t.to_string());
        *per_tool.entry(name).or_insert(0) += n;
    }
    let window = rt
        .setting_value("modelOverrides")
        .and_then(|m| m.get(&effective_model(rt)).cloned())
        .and_then(|m| m.get("contextWindow").and_then(Value::as_u64))
        .unwrap_or(200_000);

    json!({
        "total_tokens": total,
        "context_window": window,
        "categories": categories,
        "skills": per_skill,
        "tools": per_tool,
        "percent": (total as f64 / window as f64 * 100.0 * 10.0).round() / 10.0,
        "estimated": true,
    })
}

async fn read_file_via_engine(
    rt: &Arc<Runtime>,
    body: &crate::protocol::ControlRequestBody,
) -> std::result::Result<Value, String> {
    let Some(path) = &body.path else {
        return Err("readFile: path is required".into());
    };
    let p = crate::tools::resolve_path(&rt.cwd(), path).map_err(|e| e.to_string())?;
    let meta = std::fs::metadata(&p).map_err(|e| format!("readFile: {e}"))?;
    if meta.len() > crate::tools::MAX_READ_BYTES {
        return Err("readFile: file exceeds the 10 MB ceiling".into());
    }
    let body_text = std::fs::read_to_string(&p).map_err(|e| format!("readFile: {e}"))?;
    let offset = body.offset.unwrap_or(1).max(1) as usize;
    let limit = body.limit.unwrap_or(2000) as usize;
    let lines: Vec<&str> = body_text.lines().collect();
    let total = lines.len();
    let slice: Vec<&str> = lines.iter().skip(offset - 1).take(limit).copied().collect();
    Ok(json!({
        "path": p.to_string_lossy(),
        "offset": offset,
        "limit": limit,
        "total_lines": total,
        "content": slice.join("\n"),
        "truncated": offset + limit <= total,
    }))
}

/// AskUserQuestion: the engine blocks the loop and asks the GUI for a
/// structured answer. The GUI is the only writer; a timeout or a closed pipe
/// returns `timeout` to the model so the turn can finish honestly.
pub async fn ask_user_question(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let request_id = format!("q-{}", uuid::Uuid::new_v4().simple());
    let questions = input
        .get("questions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if questions.is_empty() {
        return ToolResult::err("AskUserQuestion: questions must hold at least one item");
    }
    let timeout_s = rt
        .setting_u64("questionTimeoutSeconds")
        .unwrap_or(300)
        .max(10);

    let rx = rt.register_pending(&request_id);
    let notif = json!({
        "event": "Notification",
        "session_id": rt.session_id(),
        "message": "kiri is asking a question",
        "tool_name": "AskUserQuestion",
        "request_id": request_id,
        "agent_id": rt.agent_id,
    });
    let _ = hooks::run(&rt.hooks(), "Notification", &rt.cwd(), None, &notif).await;
    rt.emit(json!({
        "type": "control_request",
        "request_id": request_id,
        "request": {
            "subtype": "can_use_tool",
            "tool_name": "AskUserQuestion",
            "input": {"questions": questions},
            "timeout_s": timeout_s,
            "agent_id": rt.agent_id,
        }
    }));
    rt.record(json!({
        "type": "question",
        "requestId": request_id,
        "toolName": "AskUserQuestion",
        "payload": questions,
        "sessionId": rt.session_id(),
        "uuid": uuid::Uuid::new_v4().to_string(),
    }))
    .ok();

    match tokio::time::timeout(std::time::Duration::from_secs(timeout_s), rx).await {
        Ok(Ok(v)) => {
            if v.get("behavior").and_then(Value::as_str) == Some("deny") {
                return ToolResult::err("The user did not answer the question.");
            }
            let answers = v.get("answers").cloned().unwrap_or(Value::Null);
            let response = v.get("response").cloned().unwrap_or(Value::Null);
            ToolResult::text(
                serde_json::to_string_pretty(&json!({
                    "answers": answers,
                    "response": response,
                }))
                .unwrap_or_default(),
            )
        }
        Ok(Err(_)) => ToolResult::err("AskUserQuestion: the GUI detached before answering."),
        Err(_) => {
            rt.resolve_pending(&request_id, Value::Null);
            ToolResult::err(format!("AskUserQuestion: timed out after {timeout_s}s."))
        }
    }
}

/// Agent: delegate to a subagent that has its OWN context window and returns
/// one report. `run_in_background` registers it in the task table instead.
///
/// The `subagent_type` selects a definition from .kiri/agents/<name>.md: its
/// prompt becomes the child's system prompt, its tools frontmatter is the
/// allowlist ceiling, and its model is the default. Explicit tool input wins
/// over the definition, but never widens the definition's allowlist.
pub async fn spawn_agent(rt: &Arc<Runtime>, input: &Value) -> ToolResult {
    let prompt = input
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if prompt.trim().is_empty() {
        return ToolResult::err("Agent: prompt is required");
    }
    let description = input
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("subagent")
        .to_string();
    let subagent_type = input
        .get("subagent_type")
        .and_then(Value::as_str)
        .unwrap_or("general-purpose")
        .to_string();
    let background = input
        .get("run_in_background")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Apply the AgentDef for this type. An unknown type is an honest error,
    // not a silent fallback to general-purpose.
    let def = crate::subagents::find_def(&rt.cfg, &subagent_type);
    if def.is_none() && subagent_type != "general-purpose" {
        let known = crate::subagents::load_all(&rt.cfg)
            .iter()
            .map(|a| a.name.clone())
            .collect::<Vec<_>>();
        return ToolResult::err(format!(
            "Agent: unknown subagent_type {subagent_type}. Available: general-purpose{}",
            if known.is_empty() {
                String::new()
            } else {
                format!(", {}", known.join(", "))
            }
        ));
    }

    let isolation = input
        .get("isolation")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| def.as_ref().and_then(|d| d.isolation.clone()));
    let model = input
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| def.as_ref().and_then(|d| d.model.clone()))
        .unwrap_or_else(|| rt.effective_model());
    let input_tools: Option<Vec<String>> = input.get("tools").and_then(Value::as_array).map(|a| {
        a.iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    });
    // The definition's allowlist is the ceiling; the call may narrow it.
    let tool_allow: Option<Vec<String>> = match (def.as_ref().and_then(|d| d.tools.clone()), input_tools) {
        (Some(def_tools), Some(call_tools)) => Some(
            call_tools
                .into_iter()
                .filter(|t| def_tools.iter().any(|d| d == t))
                .collect(),
        ),
        (Some(def_tools), None) => Some(def_tools),
        (None, call_tools) => call_tools,
    };

    let agent_id = format!("agent-{}", uuid::Uuid::new_v4().simple());
    let mut cwd = rt.cwd();
    let mut worktree: Option<crate::workspaces::Worktree> = None;

    if isolation.as_deref() == Some("worktree") {
        match crate::workspaces::create(&cwd, &description) {
            Ok((wt, path)) => {
                cwd = path;
                worktree = Some(wt);
            }
            Err(e) => return ToolResult::err(format!("Agent: worktree: {e}")),
        }
    } else if isolation.as_deref() == Some("remote") {
        return ToolResult::err(
            "Agent: isolation=remote needs a remote host; this build runs local-only.",
        );
    }

    rt.system(
        "informational",
        json!({
            "message": format!("subagent {agent_id} started: {description}"),
            "agent_id": agent_id,
            "agent_type": subagent_type,
            "isolation": isolation,
        }),
    );
    let start_payload = json!({
        "event": "SubagentStart",
        "session_id": rt.session_id(),
        "agent_id": agent_id,
        "agent_type": subagent_type,
        "description": description,
        "isolation": isolation,
        "background": background,
    });
    let _ = hooks::run(&rt.hooks(), "SubagentStart", &rt.cwd(), None, &start_payload).await;

    // A child runtime: same stream, same permission queue, own history.
    let child = rt.child(cwd, agent_id.clone(), agent_id.clone(), model.clone());
    // Enforce the definition/call model over any settings default.
    child.set_model(model);
    if let Ok(mut allow) = child.allowed_tools_override.lock() {
        *allow = tool_allow;
    }
    if let Some(d) = &def {
        if let Ok(mut p) = child.agent_prompt.lock() {
            *p = Some(d.prompt.clone());
        }
    }

    let report = if background {
        // The tool returns a handle at once; the report lands in the task table
        // and reaches the model as a task notification on a later turn. Holding
        // the parent's turn open here would block the GUI's stop button.
        let status = std::sync::Arc::new(std::sync::Mutex::new("running".to_string()));
        let output = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

        // TaskStop sets this flag; the child's cancellation watch sees it and
        // stops the loop / the in-flight stream promptly.
        if let Ok(mut g) = child.stop_flag.lock() {
            *g = Some(stop.clone());
        }

        let worktree_slot = std::sync::Mutex::new(worktree.take());

        rt.add_task(crate::runtime::BgTask {
            id: agent_id.clone(),
            kind: "agent".into(),
            subject: description.clone(),
            status: status.clone(),
            output: output.clone(),
            stop: stop.clone(),
            started_at: chrono::Utc::now(),
        });

        let rt2 = rt.clone();
        let id2 = agent_id.clone();
        let desc2 = description.clone();
        let kind2 = subagent_type.clone();
        // `run_isolated` is not `Send`: it recurses through the tool
        // dispatch, which yields non-Send futures. A thread with its own
        // current-thread runtime runs it beside the parent without reshaping
        // the whole dispatch chain for one `tokio::spawn`.
        std::thread::spawn(move || {
            let local = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    rt2.system(
                        "informational",
                        json!({
                            "message": format!("subagent {id2} could not start: {e}"),
                            "agent_id": id2,
                            "status": "error",
                        }),
                    );
                    return;
                }
            };
            let result = local.block_on(child.run_isolated(&prompt, &desc2, &kind2));

            if let Ok(mut g) = worktree_slot.lock() {
                if let Some(wt) = g.take() {
                    let _ = crate::workspaces::remove(&wt);
                }
            }

            let (status_text, report_text) = match result {
                Ok(text) => ("done", text),
                Err(e) => ("error", e.to_string()),
            };
            if let Ok(mut s) = status.lock() {
                *s = status_text.to_string();
            }
            if let Ok(mut o) = output.lock() {
                o.push_str(&report_text);
            }
            let stop_payload = json!({
                "event": "SubagentStop",
                "session_id": rt2.session_id(),
                "agent_id": id2,
                "agent_type": kind2,
                "status": status_text,
            });
            local.block_on(hooks::run(
                &rt2.hooks(),
                "SubagentStop",
                &rt2.cwd(),
                None,
                &stop_payload,
            ));
            rt2.system(
                "informational",
                json!({
                    "message": format!("subagent {id2} finished: {status_text}"),
                    "agent_id": id2,
                    "status": status_text,
                    "result": report_text,
                }),
            );
        });
        return ToolResult::text(format!(
            "Agent running in the background (agent_id: {agent_id}). \
             You will be told when it finishes."
        ));
    } else {
        child
            .run_isolated(&prompt, &description, &subagent_type)
            .await
    };

    if let Some(wt) = worktree {
        let _ = crate::workspaces::remove(&wt);
    }

    let (status_text, tool_result) = match report {
        Ok(text) => ("done", ToolResult::text(text)),
        Err(e) => ("error", ToolResult::err(format!("Agent: {e}"))),
    };
    let stop_payload = json!({
        "event": "SubagentStop",
        "session_id": rt.session_id(),
        "agent_id": agent_id,
        "agent_type": subagent_type,
        "status": status_text,
    });
    let _ = hooks::run(&rt.hooks(), "SubagentStop", &rt.cwd(), None, &stop_payload).await;
    rt.system(
        "informational",
        json!({
            "message": format!("subagent {agent_id} finished: {status_text}"),
            "agent_id": agent_id,
            "status": status_text,
        }),
    );
    tool_result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every recorded denial must reach the result envelope; subagent denials
    /// share the parent's cell, so they appear here too.
    #[test]
    fn result_carries_recorded_denials() {
        let rt = crate::runtime::tests::test_runtime();
        assert!(result_message(&rt, "success", None)["permission_denials"]
            .as_array()
            .unwrap()
            .is_empty());

        rt.record_denial("Bash", "tu1", &json!({"command": "rm -rf x"}), "deny_rule", "permission");
        let child = rt.child(std::env::temp_dir(), "a1".into(), "tu2".into(), "m".into());
        child.record_denial("Write", "tu3", &json!({}), "not in allowlist", "subagent_allowlist");

        let denials = result_message(&rt, "success", None)["permission_denials"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(denials.len(), 2);
        assert_eq!(denials[0]["tool_name"], "Bash");
        assert_eq!(denials[0]["source"], "permission");
        assert_eq!(denials[1]["source"], "subagent_allowlist");
        assert_eq!(denials[1]["agent_id"], "a1");
    }

    /// The GUI's explicit always-allow rule wins over the engine suggestion.
    #[test]
    fn always_allow_honors_the_gui_rule() {
        let r = rule_from_answer(&json!("Bash(npm test:*)")).expect("string rule");
        assert_eq!(r.tool, "Bash");
        assert_eq!(r.pattern.as_deref(), Some("npm test*"));
        assert!(matches!(r.rule_type, RuleType::Allow));

        let obj = json!({"ruleType": "allow", "tool": "Read", "pattern": "docs/**"});
        let r2 = rule_from_answer(&obj).expect("object rule");
        assert_eq!(r2.tool, "Read");
        assert_eq!(r2.pattern.as_deref(), Some("docs/**"));

        assert!(rule_from_answer(&json!(42)).is_none());
    }

    /// Discovered MCP tools are injected namespaced; the session allow/deny
    /// lists still apply to them.
    #[test]
    fn build_tools_injects_discovered_mcp_schemas() {
        let rt = crate::runtime::tests::test_runtime();
        rt.set_mcp_servers(vec![json!({
            "name": "mock",
            "transport": "stdio",
            "status": "connected",
            "tools": [{"name": "echo", "description": "echo back",
                       "inputSchema": {"type": "object", "properties": {}}}],
        })]);
        let defs = build_tools(&rt);
        let echo = defs
            .iter()
            .find(|d| d["name"] == "mcp__mock__echo")
            .expect("discovered tool must be injected");
        assert_eq!(echo["description"], "echo back");
        assert!(echo["input_schema"].is_object());

        // A static settings declaration for the same server must not
        // duplicate the discovered schema.
        let mut settings = rt.settings();
        settings.merged.insert(
            "mcpServers".into(),
            json!([{"name": "mock", "tools": [{"name": "echo"}]}]),
        );
        rt.set_settings(settings);
        let defs = build_tools(&rt);
        assert_eq!(
            defs.iter().filter(|d| d["name"] == "mcp__mock__echo").count(),
            1
        );
    }

    /// A subagent definition's allowlist is enforced at execution time, and
    /// the denial is recorded even when the model asks for a banned tool.
    /// Drives the real `run_isolated` loop against a local mock endpoint.
    #[tokio::test]
    async fn subagent_allowlist_denies_and_records() {
        let (base, _reqs, server) = mock_model(vec![
            tool_use_sse("Bash", &json!({"command": "rm -rf x"})),
            text_sse("done"),
        ]);
        let mut cfg = crate::runtime::tests::test_config();
        cfg.base_url = base;
        cfg.api_key = "test-key".into();
        let (tx, _rx) = mpsc::unbounded_channel();
        let rt = Runtime::new(cfg, Settings::default(), tx, Vec::new(), Vec::new(), Vec::new(), Mode::Default);
        let child = rt.child(std::env::temp_dir(), "a1".into(), "tu1".into(), "fake".into());
        *child.allowed_tools_override.lock().unwrap() = Some(vec!["Read".into()]);

        let report = child
            .run_isolated("do it", "desc", "general-purpose")
            .await
            .unwrap();
        assert_eq!(report, "done");

        let denials = rt.permission_denials.lock().unwrap().clone();
        assert_eq!(denials.len(), 1, "the banned call must be recorded: {denials:?}");
        assert_eq!(denials[0]["tool_name"], "Bash");
        assert_eq!(denials[0]["source"], "subagent_allowlist");
        assert_eq!(denials[0]["agent_id"], "a1");
        // Subagent spend is aggregated into the parent's usage (item: budget).
        assert!(rt.usage_snapshot().input_tokens >= 10);
        server.join().unwrap();
    }

    /// A subagent's tool call goes through the SAME permission gate as the
    /// parent: the GUI prompt arrives on the shared queue with agent
    /// attribution, and the GUI's allow unblocks the child's execution.
    #[tokio::test]
    async fn subagent_tool_call_uses_the_shared_permission_gate() {
        let dir = std::env::temp_dir().join(format!("kiri-gate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let (base, _reqs, server) = mock_model(vec![
            tool_use_sse("Write", &json!({"file_path": "gate.txt", "content": "gated"})),
            text_sse("ok"),
        ]);
        let mut cfg = crate::runtime::tests::test_config();
        cfg.base_url = base;
        cfg.api_key = "test-key".into();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let rt = Runtime::new(cfg, Settings::default(), tx, Vec::new(), Vec::new(), Vec::new(), Mode::Default);
        let child = rt.child(dir.clone(), "a2".into(), "tu2".into(), "fake".into());

        let rt2 = rt.clone();
        let (report, _) = tokio::join!(
            child.run_isolated("write it", "desc", "general-purpose"),
            async move {
                // Answer the permission prompt exactly as the GUI does,
                // through the PARENT's queue.
                while let Some(v) = rx.recv().await {
                    if v["type"] == "control_request"
                        && v["request"]["subtype"] == "can_use_tool"
                    {
                        assert_eq!(v["request"]["agent_id"], "a2");
                        assert_eq!(v["request"]["tool_name"], "Write");
                        let id = v["request_id"].as_str().unwrap().to_string();
                        assert!(rt2.resolve_pending(&id, json!({"behavior": "allow"})));
                        return;
                    }
                }
                panic!("no permission request reached the shared queue");
            }
        );
        assert_eq!(report.unwrap(), "ok");
        assert_eq!(std::fs::read_to_string(dir.join("gate.txt")).unwrap(), "gated");
        assert!(rt.permission_denials.lock().unwrap().is_empty());
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- local mock model endpoint (loopback only, no credentials) ----

    fn sse(events: &[Value]) -> String {
        events
            .iter()
            .map(|e| format!("data: {e}\n\n"))
            .collect::<Vec<_>>()
            .join("")
    }

    fn tool_use_sse(tool: &str, input: &Value) -> String {
        sse(&[
            json!({"type": "message_start", "message": {"id": "m1", "model": "fake",
                "usage": {"input_tokens": 5, "output_tokens": 1}}}),
            json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "tool_use", "id": "t1", "name": tool, "input": {}}}),
            json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "input_json_delta",
                          "partial_json": serde_json::to_string(input).unwrap()}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"},
                "usage": {"output_tokens": 1}}),
            json!({"type": "message_stop"}),
        ])
    }

    fn text_sse(text: &str) -> String {
        sse(&[
            json!({"type": "message_start", "message": {"id": "m2", "model": "fake",
                "usage": {"input_tokens": 5, "output_tokens": 1}}}),
            json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": text}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
                "usage": {"output_tokens": 1}}),
            json!({"type": "message_stop"}),
        ])
    }

    /// One SSE response per request, in order. Binds 127.0.0.1 only.
    /// Captures each request body so tests can assert what the model saw.
    fn mock_model(
        responses: Vec<String>,
    ) -> (
        String,
        Arc<std::sync::Mutex<Vec<Value>>>,
        std::thread::JoinHandle<()>,
    ) {
        use std::io::{BufRead, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let captured: Arc<std::sync::Mutex<Vec<Value>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let cap2 = captured.clone();
        let handle = std::thread::spawn(move || {
            for body in responses {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut content_len = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap() == 0 {
                        break;
                    }
                    if line.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        content_len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut req_body = vec![0u8; content_len];
                let _ = reader.read_exact(&mut req_body);
                if let Ok(v) = serde_json::from_slice::<Value>(&req_body) {
                    cap2.lock().unwrap().push(v);
                }
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let mut stream = stream;
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}"), captured, handle)
    }

    /// The AgentDef selected by subagent_type supplies the child's system
    /// prompt, model and tool allowlist; an unknown type is an honest error.
    #[tokio::test]
    async fn agent_def_applies_prompt_tools_and_model() {
        let dir = std::env::temp_dir().join(format!("kiri-def-{}", uuid::Uuid::new_v4()));
        let agents = dir.join(".kiri").join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("tester.md"),
            "---\nname: tester\ndescription: test agent\nmodel: def-model\ntools: Read\n---\nYou are the tester agent. Always answer TESTER.",
        )
        .unwrap();

        let (base, reqs, server) = mock_model(vec![text_sse("ok")]);
        let mut cfg = crate::runtime::tests::test_config();
        cfg.cwd = dir.clone();
        cfg.base_url = base;
        cfg.api_key = "test-key".into();
        let (tx, _rx) = mpsc::unbounded_channel();
        let rt = Runtime::new(cfg, Settings::default(), tx, Vec::new(), Vec::new(), Vec::new(), Mode::Default);

        let out = spawn_agent(
            &rt,
            &json!({"prompt": "go", "description": "d", "subagent_type": "tester"}),
        )
        .await;
        assert!(!out.is_error, "{:?}", out.content);

        let captured = reqs.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0]["model"], "def-model");
        let system = captured[0]["system"].as_str().unwrap_or("");
        assert!(system.contains("You are the tester agent"), "def prompt missing");
        let tools = captured[0]["tools"].as_array().unwrap();
        assert!(!tools.is_empty());
        assert!(
            tools.iter().all(|t| t["name"] == "Read"),
            "def allowlist not enforced: {tools:?}"
        );

        let bad = spawn_agent(
            &rt,
            &json!({"prompt": "go", "description": "d", "subagent_type": "nope"}),
        )
        .await;
        assert!(bad.is_error, "unknown subagent_type must error");
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }
}
