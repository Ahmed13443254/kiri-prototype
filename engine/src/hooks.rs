// Hooks: lifecycle interception on engine events.
//
// Hook kinds: `command` (a subprocess), `prompt` (an LLM judges), `http`
// (an outbound POST to a user-configured URL - the engine is the client; no
// part of this product listens). Exit code 2 from a command hook on
// PreToolUse denies the call. A hook `allow` never overrides a deny rule.

use anyhow::Result;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use crate::permissions::Decision;

#[derive(Debug, Clone)]
pub struct HookDef {
    pub event: String,
    pub matcher: Option<String>,
    pub hook_type: String,
    pub command: Option<String>,
    pub url: Option<String>,
    pub body_json: Option<String>,
    pub timeout_s: u64,
    pub enabled: bool,
}

pub fn parse_settings_hooks(raw: &[Value]) -> Vec<HookDef> {
    let mut out = Vec::new();
    for item in raw {
        let event = item
            .get("event")
            .or_else(|| item.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if event.is_empty() {
            continue;
        }
        let hook_type = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("command")
            .to_string();
        out.push(HookDef {
            event,
            matcher: item.get("matcher").and_then(Value::as_str).map(str::to_string),
            hook_type,
            command: item.get("command").and_then(Value::as_str).map(str::to_string),
            url: item.get("url").and_then(Value::as_str).map(str::to_string),
            body_json: item.get("body").map(|v| v.to_string()),
            timeout_s: item.get("timeout").and_then(Value::as_u64).unwrap_or(60),
            enabled: item.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        });
    }
    out
}

pub fn matches(h: &HookDef, event: &str, tool_name: Option<&str>) -> bool {
    if !h.enabled || h.event != event {
        return false;
    }
    match (&h.matcher, tool_name) {
        (None, _) => true,
        (Some(_), None) => true,
        (Some(m), Some(t)) => crate::permissions::glob_match(m, t),
    }
}

#[derive(Debug)]
pub struct HookOutcome {
    pub decision: Option<Decision>,
    /// stdout appended to context (SessionStart / UserPromptSubmit).
    pub append: Option<String>,
    /// PostToolUse can replace the tool result body.
    pub mutate: Option<String>,
    pub exit_code: Option<i32>,
    pub event_for: Option<String>,
}

impl HookOutcome {
    /// The permission gate this hook imposes, if any. `Ask` is not a gate.
    pub fn gate(&self) -> Option<Decision> {
        match self.decision {
            Some(Decision::Allow) | Some(Decision::Deny) => self.decision,
            _ => None,
        }
    }

    /// Why the gate denied, for the tool_result body.
    pub fn deny_reason(&self) -> Option<String> {
        match self.decision {
            Some(Decision::Deny) => Some(format!(
                "denied by {} hook",
                self.event_for.as_deref().unwrap_or("a")
            )),
            _ => None,
        }
    }

    fn empty() -> Self {
        Self {
            decision: None,
            append: None,
            mutate: None,
            exit_code: None,
            event_for: None,
        }
    }
}

/// Run every matching hook of one event, in declared order. The first
/// decision-producing hook wins a gate; appends accumulate.
pub async fn run(
    hooks: &[HookDef],
    event: &str,
    cwd: &std::path::Path,
    tool_name: Option<&str>,
    payload: &Value,
) -> HookOutcome {
    let mut out = HookOutcome::empty();
    out.event_for = Some(event.to_string());
    let mut appended = String::new();

    for h in hooks {
        if !matches(h, event, tool_name) {
            continue;
        }
        match h.hook_type.as_str() {
            "command" => {
                let Some(cmd) = &h.command else { continue };
                let res = run_command(
                    cmd,
                    cwd,
                    &payload.to_string(),
                    Duration::from_secs(h.timeout_s.max(1)),
                )
                .await;
                let Some((code, stdout, stderr)) = res else {
                    continue;
                };
                out.exit_code = Some(code);
                if !stdout.trim().is_empty() {
                    appended.push_str(&stdout);
                }
                if !stderr.trim().is_empty() && crate::session_state::debug_enabled() {
                    eprintln!("[kiri] hook {} stderr: {}", h.event, stderr.trim());
                }
                // exit 2 = deny on PreToolUse; elsewhere it is an error
                // note, not a permission decision.
                if code == 2 && event == "PreToolUse" {
                    out.decision = Some(Decision::Deny);
                    return out;
                }
                if event == "PreToolUse" {
                    if let Ok(v) = serde_json::from_str::<Value>(&stdout) {
                        if let Some(d) = v.get("decision").and_then(Value::as_str) {
                            out.decision = match d {
                                "allow" => Some(Decision::Allow),
                                "deny" => Some(Decision::Deny),
                                "ask" => Some(Decision::Ask),
                                _ => out.decision,
                            };
                        }
                        if let Some(d) = v
                            .get("hookSpecificOutput")
                            .and_then(|x| x.get("permissionDecision"))
                            .and_then(Value::as_str)
                        {
                            out.decision = match d {
                                "allow" => Some(Decision::Allow),
                                "deny" => Some(Decision::Deny),
                                "ask" => Some(Decision::Ask),
                                _ => out.decision,
                            };
                        }
                    }
                } else if event == "PostToolUse" {
                    if let Ok(v) = serde_json::from_str::<Value>(&stdout) {
                        if let Some(m) = v.get("mutate").and_then(Value::as_str) {
                            out.mutate = Some(m.to_string());
                        }
                        if let Some(ok) = v.get("continue").and_then(Value::as_bool) {
                            if !ok {
                                out.decision = Some(Decision::Deny);
                            }
                        }
                    }
                }
            }
            "prompt" => {
                // Prompt hooks are evaluated by the agent loop's `judge_hook`,
                // which returns a Decision; that function calls back into
                // `apply_judgement` so the same gate rules hold.
            }
            "http" => {
                // Outbound only: the engine posts to an inspector URL the user
                // configured. Nothing in this product listens.
                if let Some(url) = &h.url {
                    if let Ok(body) =
                        post_json(url, payload, Duration::from_secs(h.timeout_s.max(1))).await
                    {
                        if let Some(d) = body.get("decision").and_then(Value::as_str) {
                            if event == "PreToolUse" {
                                out.decision = match d {
                                    "allow" => Some(Decision::Allow),
                                    "deny" => Some(Decision::Deny),
                                    "ask" => Some(Decision::Ask),
                                    _ => out.decision,
                                };
                            }
                        }
                        if let Some(m) = body.get("mutate").and_then(Value::as_str) {
                            if event == "PostToolUse" {
                                out.mutate = Some(m.to_string());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        // First gate wins; later hooks cannot un-deny a call.
        if out.decision.is_some() && event == "PreToolUse" {
            return out;
        }
    }

    if !appended.is_empty() {
        out.append = Some(appended);
    }
    out
}

async fn run_command(
    cmd: &str,
    cwd: &std::path::Path,
    payload: &str,
    timeout: Duration,
) -> Option<(i32, String, String)> {
    #[cfg(windows)]
    let mut child = {
        let mut c = tokio::process::Command::new("powershell");
        c.arg("-NoProfile")
            .arg("-NonInteractive")
            .arg("-Command")
            .arg(cmd);
        c
    };
    #[cfg(not(windows))]
    let mut child = {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    };

    child
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = child.spawn().ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes()).await;
        let _ = stdin.shutdown().await;
    }
    let out = tokio::time::timeout(timeout, child.wait_with_output())
        .await
        .ok()?
        .ok()?;
    let code = out.status.code().unwrap_or(1);
    Some((
        code,
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    ))
}

async fn post_json(url: &str, payload: &Value, timeout: Duration) -> Result<Value> {
    let client = reqwest::Client::builder().timeout(timeout).build()?;
    let resp = client.post(url).json(payload).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("hook_http_status {}", resp.status());
    }
    let text = resp.text().await?;
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    Ok(v)
}

/// Events the engine emits.
pub const EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Notification",
    "Stop",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "PostCompact",
    "SessionEnd",
    "PreModelSwitch",
    "PostModelSwitch",
    "Setup",
];

pub fn describe() -> Value {
    json!({
        "events": EVENTS,
        "types": ["command", "prompt", "http"],
        "denyExitCode": 2
    })
}
