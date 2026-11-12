// Anthropic-compatible Messages API client.
// Only outbound HTTP in the product; nothing binds or listens.
//
// Transport rules this file owns:
//   - streaming SSE is the only path (long inputs/outputs must not time out)
//   - the engine retries 429/5xx itself, with backoff, and reports each retry
//     to the GUI; the GUI must NOT add a retry on top (recon, "Rate limits")
//   - 401/403 are never retried (a rejected key is not a transient failurenick)

use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use serde_json::{json, Value};
use std::time::Duration;

pub const DEFAULT_BASE: &str = "https://api.anthropic.com";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
pub const DEFAULT_MODEL: &str = "claude-opus-5-5";

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    api_key: String,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Optional wire sink for retry / error notices (the GUI's retry counter).
    /// Never carries the key or the request body.
    events: Option<tokio::sync::mpsc::UnboundedSender<Value>>,
}

#[derive(Debug, Clone)]
pub enum Delta {
    MessageStart { usage: Usage, model: String },
    BlockStart { index: u64, block: Value },
    BlockDelta { index: u64, delta: Value },
    BlockStop { index: u64 },
    MessageDelta { stop_reason: Option<String>, usage: Usage },
    Ping,
}

#[derive(Debug, Default)]
pub struct Completed {
    pub id: Option<String>,
    pub model: String,
    pub content: Vec<Value>,
    pub stop_reason: Option<String>,
    pub usage: Usage,
    pub text: String,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub output_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens
            + self.cache_creation_input_tokens
            + self.cache_read_input_tokens
            + self.output_tokens
    }
}

impl Client {
    pub fn new(base: &str, api_key: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(600))
                .connect_timeout(Duration::from_secs(30))
                .build()
                .expect("http client"),
            base: base.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            events: None,
        }
    }

    /// Attach the session's wire channel so retries and API errors surface as
    /// events the GUI can count. Only status/attempt metadata is sent.
    pub fn set_events(&mut self, tx: tokio::sync::mpsc::UnboundedSender<Value>) {
        self.events = Some(tx);
    }

    /// A copy with its own cancellation flag: a subagent shares the endpoint
    /// and key, but stopping it must not cancel the parent's stream.
    pub fn fork_cancel(&self) -> Self {
        let mut c = self.clone();
        c.cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        c
    }

    fn emit_event(&self, v: Value) {
        if let Some(tx) = &self.events {
            let _ = tx.send(v);
        }
    }

    /// Interrupt: the in-flight stream stops at the next chunk boundary and
    /// returns an `interrupted` error instead of a partial completion.
    pub fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn reset_cancel(&self) {
        self.cancelled
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn has_key(&self) -> bool {
        !self.api_key.is_empty()
    }

    /// Stream one assistant turn. `on_event` is called for every delta; a
    /// retry re-sends the request from the start, so the caller must be
    /// prepared inertia to receive a second message_start for the same turn.
    pub async fn stream(
        &self,
        req: &Value,
        on_event: &mut (dyn FnMut(Delta) -> Result<()> + Send),
    ) -> Result<Completed> {
        if !self.has_key() {
            return Err(anyhow!("no_effective_model_key"));
        }

        let mut attempt = 0u32;
        let mut delay_ms: u64 = 700;
        loop {
            attempt += 1;
            match self.try_stream(req, on_event).await {
                Ok(c) => return Ok(c),
                Err(E::Retry { status, retry_after }) => {
                    if attempt > 6 {
                        self.emit_event(json!({
                            "type": "api_error",
                            "status": status,
                            "attempt": attempt,
                            "message": "api_retry_exhausted",
                        }));
                        return Err(anyhow!("api_retry_exhausted status={status}"));
                    }
                    let wait = retry_after.unwrap_or(delay_ms).min(30_000);
                    // The GUI shows the retry counter from these events and
                    // must not add a retry of its own (recon, "Rate limits").
                    self.emit_event(json!({
                        "type": "api_error",
                        "retry": true,
                        "status": status,
                        "attempt": attempt,
                        "retry_after_ms": wait,
                    }));
                    tokio::time::sleep(Duration::from_millis(wait)).await;
                    delay_ms = (delay_ms * 2).min(20_000);
                }
                Err(E::Fatal(m)) => {
                    // An interrupt is the user's own action, not an API error.
                    if !m.starts_with("interrupted") {
                        self.emit_event(json!({
                            "type": "api_error",
                            "retry": false,
                            "attempt": attempt,
                            "message": m,
                        }));
                    }
                    return Err(anyhow!(m));
                }
            }
        }
    }

    async fn try_stream(
        &self,
        req: &Value,
        on_event: &mut (dyn FnMut(Delta) -> Result<()> + Send),
    ) -> std::result::Result<Completed, E> {
        let body = req.clone();

        let resp = self
            .http
            .post(format!("{}/v1/messages", self.base))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| E::Fatal(format!("api_network_error: {e}")))?;

        let status = resp.status();
        if status == 429 || status == 529 || status.as_u16() >= 500 {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .map(|s| s.saturating_mul(1000));
            return Err(E::Retry {
                status: status.as_u16(),
                retry_after,
            });
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(E::Fatal(format!(
                "api_error {}: {}",
                status.as_u16(),
                clip(&text, 2000)
            )));
        }

        let mut out = Completed::default();
        let mut blocks: Vec<Option<Value>> = Vec::new();
        let mut partial_json: Vec<String> = Vec::new();
        let mut cur_text = String::new();
        let mut cur_thinking = String::new();

        let mut stream = resp.bytes_stream();
        let mut buf = String::new();

        while let Some(chunk) = stream.next().await {
            if self.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
                return Err(E::Fatal("interrupted: stream cancelled by the GUI".into()));
            }
            let chunk = chunk.map_err(|e| E::Fatal(format!("api_stream_error: {e}")))?;
            buf.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(pos) = buf.find('\n') {
                let line = buf[..pos].trim_end_matches('\r').to_string();
                buf.drain(..pos + 1);
                if line.is_empty() || line.starts_with("event:") || line.starts_with(':') {
                    continue;
                }
                let Some(data) = line.strip_prefix("data: ") else {
                    continue;
                };
                if data == "[DONE]" {
                    continue;
                }
                let ev: Value = serde_json::from_str(data)
                    .map_err(|e| E::Fatal(format!("api_sse_parse: {e}: {}", clip(data, 300))))?;
                match ev.get("type").and_then(Value::as_str).unwrap_or("") {
                    "message_start" => {
                        let m = ev.get("message").cloned().unwrap_or(Value::Null);
                        out.id = m.get("id").and_then(Value::as_str).map(str::to_string);
                        out.model = m
                            .get("model")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        out.usage = parse_usage(m.get("usage"));
                        on_event(Delta::MessageStart {
                            usage: out.usage.clone(),
                            model: out.model.clone(),
                        })?;
                    }
                    "content_block_start" => {
                        let index = ev["index"].as_u64().unwrap_or(0);
                        let block = ev["content_block"].clone();
                        while blocks.len() as u64 <= index {
                            blocks.push(None);
                            partial_json.push(String::new());
                        }
                        match block.get("type").and_then(Value::as_str) {
                            Some("text") => cur_text.clear(),
                            Some("thinking") => cur_thinking.clear(),
                            _ => {}
                        }
                        blocks[index as usize] = Some(block.clone());
                        on_event(Delta::BlockStart { index, block })?;
                    }
                    "content_block_delta" => {
                        let index = ev["index"].as_u64().unwrap_or(0);
                        let d = ev.get("delta").cloned().unwrap_or(Value::Null);
                        match d.get("type").and_then(Value::as_str).unwrap_or("") {
                            "text_delta" => {
                                if let Some(t) = d.get("text").and_then(Value::as_str) {
                                    cur_text.push_str(t);
                                    out.text.push_str(t);
                                }
                            }
                            "thinking_delta" => {
                                if let Some(t) = d.get("thinking").and_then(Value::as_str) {
                                    cur_thinking.push_str(t);
                                }
                            }
                            "input_json_delta" => {
                                if let Some(t) = d.get("partial_json").and_then(Value::as_str) {
                                    while partial_json.len() as u64 <= index {
                                        partial_json.push(String::new());
                                    }
                                    partial_json[index as usize].push_str(t);
                                }
                            }
                            _ => {}
                        }
                        on_event(Delta::BlockDelta { index, delta: d })?;
                    }
                    "content_block_stop" => {
                        let index = ev["index"].as_u64().unwrap_or(0);
                        if let Some(Some(blk)) = blocks.get_mut(index as usize) {
                            match blk.get("type").and_then(Value::as_str) {
                                Some("text") => {
                                    blk["text"] =
                                        Value::String(std::mem::take(&mut cur_text));
                                }
                                Some("thinking") => {
                                    blk["thinking"] =
                                        Value::String(std::mem::take(&mut cur_thinking));
                                }
                                Some("tool_use") => {
                                    let raw =
                                        partial_json
                                            .get(index as usize)
                                            .cloned()
                                            .unwrap_or_default();
                                    let input: Value = if raw.trim().is_empty() {
                                        json!({})
                                    } else {
                                        serde_json::from_str(&raw).map_err(|e| {
                                            E::Fatal(format!(
                                                "tool_input_json: {e}: {}",
                                                clip(&raw, 500)
                                            ))
                                        })?
                                    };
                                    blk["input"] = input;
                                }
                                _ => {}
                            }
                        }
                        on_event(Delta::BlockStop { index })?;
                    }
                    "message_delta" => {
                        let stop_reason = ev
                            .pointer("/delta/stop_reason")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        let u = parse_usage(ev.get("usage"));
                        if u.output_tokens > 0 {
                            out.usage.output_tokens = u.output_tokens;
                        }
                        out.stop_reason = stop_reason.clone();
                        on_event(Delta::MessageDelta { stop_reason, usage: u })?;
                    }
                    "message_stop" => {}
                    "ping" => {
                        on_event(Delta::Ping)?;
                    }
                    "error" => {
                        return Err(E::Fatal(format!(
                            "api_stream_error: {}",
                            clip(&ev.to_string(), 1000)
                        )))
                    }
                    _ => {}
                }
            }
        }

        out.content = blocks.into_iter().flatten().collect();
        Ok(out)
    }
}

impl From<anyhow::Error> for E {
    fn from(value: anyhow::Error) -> Self {
        E::Fatal(value.to_string())
    }
}

enum E {
    Retry {
        status: u16,
        retry_after: Option<u64>,
    },
    Fatal(String),
}

fn parse_usage(v: Option<&Value>) -> Usage {
    let Some(v) = v else { return Usage::default() };
    Usage {
        input_tokens: v.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
        cache_creation_input_tokens: v
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read_input_tokens: v
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: v.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
    }
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "..."
    }
}

/// Cheap client-side token estimate. Never used for billing; the `result`
/// message carries the API's own usage, which is authoritative.
pub fn estimate_tokens(text: &str) -> u64 {
    let chars = text.chars().count() as u64;
    (chars / 4).max(text.lines().count() as u64)
}

/// Per-token price in micro-USD. Unknown models return None so the GUI can
/// show "-" rather than a fabricated 0 (recon hard part 5).
pub fn price_per_token(model: &str) -> Option<ModelPrice> {
    let m = model.to_ascii_lowercase();
    let (input_usd_per_mtok, output_usd_per_mtok) = if m.contains("fable-5-1")
        || m.contains("mythos-5-1")
    {
        (10.0, 50.0)
    } else if m.contains("fable-5") || m.contains("mythos-5") {
        (10.0, 50.0)
    } else if m.contains("opus-5-5") {
        (4.0, 20.0)
    } else if m.contains("opus-5")
        || m.contains("opus-4-8")
        || m.contains("opus-4-7")
        || m.contains("opus-4-6")
    {
        (5.0, 25.0)
    } else if m.contains("sonnet-5-5") {
        (2.0, 10.0)
    } else if m.contains("sonnet-5") || m.contains("sonnet-4-6") {
        (3.0, 15.0)
    } else if m.contains("haiku-5-5") {
        (0.10, 0.50)
    } else if m.contains("haiku-4-5") {
        (1.0, 5.0)
    } else {
        return None;
    };

    Some(ModelPrice {
        input_micros_per_token: input_usd_per_mtok / 1_000_000.0,
        output_micros_per_token: output_usd_per_mtok / 1_000_000.0,
    })
}

#[derive(Debug, Clone, Copy)]
pub struct ModelPrice {
    pub input_micros_per_token: f64,
    pub output_micros_per_token: f64,
}

pub fn cost_micros(model: &str, usage: &Usage) -> Option<i64> {
    let p = price_per_token(model)?;
    // Cache read is 0.1x input, cache write 1.25x input (API standard).
    let input = usage.input_tokens as f64 * p.input_micros_per_token
        + usage.cache_read_input_tokens as f64 * p.input_micros_per_token * 0.10
        + usage.cache_creation_input_tokens as f64 * p.input_micros_per_token * 1.25;
    let output = usage.output_tokens as f64 * p.output_micros_per_token;
    Some((input + output).round() as i64)
}
