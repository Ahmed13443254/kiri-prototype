// The engine child. One per session; the host owns both pipe ends.
//
// recon: "a direct-IPC GUI does not need to invent a transport. A GUI that
// spawns the binary and speaks this protocol is the same client the SDK is."
// Nothing here opens a port, binds, or listens.

use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, Mutex};

#[derive(Debug, Clone)]
pub struct EngineEvent {
    pub session_id: String,
    pub line: Value,
}

pub struct EngineChild {
    pub session_id: String,
    pub pid: Option<u32>,
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    alive: AtomicBool,
    /// Control requests awaiting a `control_response`, keyed by request_id.
    pending: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<Value>>>>,
    pub events: broadcast::Sender<EngineEvent>,
}

impl EngineChild {
    /// Spawn the engine with the flags architecture.md pins for the transport.
    pub fn spawn(session_id: &str, cwd: &str, extra: &[String], resource_dir: Option<&std::path::Path>) -> Result<Arc<Self>> {
        let exe = engine_binary(resource_dir);
        let mut args: Vec<String> = vec![
            "--input-format".into(),
            "stream-json".into(),
            "--output-format".into(),
            "stream-json".into(),
            "--verbose".into(),
            "--include-partial-messages".into(),
            "--cwd".into(),
            cwd.to_string(),
        ];
        args.extend_from_slice(extra);

        let mut cmd = Command::new(&exe);
        cmd.args(&args).current_dir(cwd);
        // The model key is handed over in the environment, never in argv:
        // argv is readable by every process on the machine, and the engine
        // already reads ANTHROPIC_API_KEY itself. The engine never writes it
        // to the transcript and never echoes it back in a message.
        if let Some(key) = crate::vault::read_key() {
            if !key.is_empty() {
                cmd.env("ANTHROPIC_API_KEY", key);
            }
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn engine at {}", exe))?;

        let pid = child.id();
        let stdin = child.stdin.take().context("engine stdin")?;
        let stdout = child.stdout.take().context("engine stdout")?;
        let stderr = child.stderr.take().context("engine stderr")?;

        let (tx, _) = broadcast::channel::<EngineEvent>(4096);
        let sid = session_id.to_string();
        let pending: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // stdout -> events. One reader; the reducer in the renderer is fed
        // from this exact order. A `control_response` is routed to its waiter
        // as well as broadcast, so `readFile`/`getContextUsage` can return an
        // answer instead of a request id.
        {
            let tx = tx.clone();
            let sid = sid.clone();
            let pending = pending.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    match serde_json::from_str::<Value>(&line) {
                        Ok(v) => {
                            if v.get("type").and_then(Value::as_str) == Some("control_response") {
                                let id = v
                                    .get("response")
                                    .and_then(|r| r.get("request_id"))
                                    .and_then(Value::as_str)
                                    .map(str::to_string);
                                if let Some(id) = id {
                                    if let Some(waiter) = pending.lock().await.remove(&id) {
                                        let payload = v
                                            .get("response")
                                            .cloned()
                                            .unwrap_or(Value::Null);
                                        let _ = waiter.send(payload);
                                    }
                                }
                            }
                            let _ = tx.send(EngineEvent {
                                session_id: sid.clone(),
                                line: v,
                            });
                        }
                        Err(_) => {
                            // A torn line is bookkeeping for the log, not a
                            // reason to drop the stream. recon: reader goes to
                            // EOF, never breaks on one bad record.
                        }
                    }
                }
            });
        }

        // stderr is engine diagnostics only; surfaced to the log panel.
        {
            let tx = tx.clone();
            let sid = sid.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let _ = tx.send(EngineEvent {
                        session_id: sid.clone(),
                        line: serde_json::json!({
                            "type": "system",
                            "subtype": "informational",
                            "level": "debug",
                            "message": line,
                        }),
                    });
                }
            });
        }

        Ok(Arc::new(Self {
            session_id: session_id.to_string(),
            pid,
            child: Mutex::new(child),
            stdin: Mutex::new(Some(stdin)),
            alive: AtomicBool::new(true),
            pending,
            events: tx,
        }))
    }

    /// Write one NDJSON object. A closed pipe is reported, never swallowed.
    pub async fn send(&self, value: &Value) -> Result<()> {
        let mut slot = self.stdin.lock().await;
        let stdin = slot.as_mut().context("engine stdin is closed")?;
        let mut line = serde_json::to_string(value)?;
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .await
            .context("engine stdin write")?;
        stdin.flush().await?;
        Ok(())
    }

    pub async fn send_user_prompt(&self, text: &str) -> Result<()> {
        self.send(&serde_json::json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [{"type": "text", "text": text}]
            }
        }))
        .await
    }

    /// A control request that waits for its reply. The reader task routes each
    /// `control_response` to the waiter registered here under the same
    /// `request_id`; the reply lives at `response.response`, not the top level.
    pub async fn request(
        &self,
        subtype: &str,
        extra: Value,
        timeout: std::time::Duration,
    ) -> Result<Value> {
        let request_id = format!("req-{}", uuid::Uuid::new_v4().simple());
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending
            .lock()
            .await
            .insert(request_id.clone(), tx);
        if let Err(e) = self.control(&request_id, subtype, extra).await {
            self.pending.lock().await.remove(&request_id);
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(_)) => anyhow::bail!("engine closed before answering {subtype}"),
            Err(_) => {
                self.pending.lock().await.remove(&request_id);
                anyhow::bail!("engine did not answer {subtype} in {:?}", timeout)
            }
        }
    }

    pub async fn control(&self, request_id: &str, subtype: &str, extra: Value) -> Result<()> {
        let mut body = serde_json::json!({"subtype": subtype});
        if let (Some(b), Value::Object(e)) = (body.as_object_mut(), extra) {
            for (k, v) in e {
                b.insert(k, v);
            }
        }
        self.send(&serde_json::json!({
            "type": "control_request",
            "request_id": request_id,
            "request": body,
        }))
        .await
    }

    /// Answer a permission prompt. Echoing `request_id` is the contract; an
    /// answer without it is ignored by the engine.
    pub async fn answer_permission(&self, request_id: &str, behavior: &str, extra: Value) -> Result<()> {
        let mut response = serde_json::json!({"behavior": behavior});
        if let (Some(r), Value::Object(e)) = (response.as_object_mut(), extra) {
            for (k, v) in e {
                r.insert(k, v);
            }
        }
        self.send(&serde_json::json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": request_id,
                "response": response,
            }
        }))
        .await
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    pub fn exit_code(&self) -> Option<i32> {
        None
    }

    pub async fn signal(&self, kind: Signal) -> Result<()> {
        let mut child = self.child.lock().await;
        match kind {
            Signal::Interrupt => {
                let _ = child.start_kill();
            }
            Signal::Term => {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
            Signal::Kill => {
                let _ = child.kill().await;
            }
        }
        self.alive.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Clean end of session: close the engine's stdin and wait for it to exit
    /// on its own. The engine sees EOF, runs SessionEnd hooks and answers
    /// `worker_shutting_down`; killing it first would skip both, which recon
    /// calls out as the wrong way to stop a session.
    pub async fn shutdown(&self) -> Result<Option<i32>> {
        self.alive.store(false, Ordering::SeqCst);
        self.close_stdin().await?;
        let mut child = self.child.lock().await;
        match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
            Ok(Ok(status)) => Ok(status.code()),
            // A child that will not exit on EOF is killed only after the grace
            // period; the caller still gets "no code" rather than a hang.
            Ok(Err(_)) | Err(_) => {
                let _ = child.kill().await;
                Ok(None)
            }
        }
    }

    /// Drop the write end of the pipe. The engine's reader sees EOF, which is
    /// its documented clean-shutdown signal.
    pub async fn close_stdin(&self) -> Result<()> {
        let mut slot = self.stdin.lock().await;
        // ChildStdin has no close(); dropping the only handle is closing it.
        if let Some(mut stdin) = slot.take() {
            let _ = stdin.flush().await;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Signal {
    Interrupt,
    Term,
    Kill,
}

fn engine_binary(resource_dir: Option<&std::path::Path>) -> String {
    if let Ok(p) = std::env::var("KIRI_ENGINE_BIN") {
        return p;
    }
    let exe_name = if cfg!(windows) { "kiri-engine.exe" } else { "kiri-engine" };
    if let Some(dir) = resource_dir {
        let bundled = dir.join("engine").join(exe_name);
        if bundled.exists() {
            return bundled.to_string_lossy().to_string();
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in [dir.join(exe_name), dir.join("engine").join(exe_name)] {
                if candidate.exists() {
                    return candidate.to_string_lossy().to_string();
                }
            }
        }
    }
    let here = std::env::current_dir().unwrap_or_default();
    for candidate in [
        here.join("..").join("engine").join("target").join("debug").join(exe_name),
        here.join("..").join("engine").join("target").join("release").join(exe_name),
        here.join("engine").join("target").join("debug").join(exe_name),
        here.join("engine").join("target").join("release").join(exe_name),
    ] {
        if candidate.exists() {
            return candidate.to_string_lossy().to_string();
        }
    }
    "kiri-engine".to_string()
}
