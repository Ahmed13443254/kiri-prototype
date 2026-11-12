// MCP: the engine is an MCP client.
//
// Servers are configured in our own settings under `mcpServers`; each entry is
// one connection. Their tools are injected into the tool list under
// `mcp__<server>__<tool>`. stdio is a child process speaking newline-delimited
// JSON-RPC. http/sse are a URL on the same config row, not a different table.
//
// Connections are made on demand and closed after the call: MCP servers are
// short-lived helpers for one tool call, and a GUI crash must not leave an
// orphaned child holding a pipe.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::{timeout, Duration};

use crate::runtime::Runtime;
use crate::tools::ToolResult;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

impl McpTool {
    fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
        })
    }
}

pub struct Conn {
    name: String,
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    pub tools: Vec<McpTool>,
}

impl Conn {
    pub async fn open_stdio(
        name: &str,
        command: &str,
        args: &[String],
        env: &std::collections::HashMap<String, String>,
    ) -> Result<Self> {
        let mut c = tokio::process::Command::new(command);
        c.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for (k, v) in env {
            c.env(k, v);
        }
        let mut child = c.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("mcp: no stdin on {command}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("mcp: no stdout on {command}"))?;
        Ok(Self {
            name: name.to_string(),
            child,
            stdin,
            lines: BufReader::new(stdout).lines(),
            next_id: 1,
            tools: Vec::new(),
        })
    }

    async fn send(&mut self, msg: &Value) -> Result<()> {
        let mut line = serde_json::to_string(msg)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn recv_id(&mut self, id: u64) -> Result<Value> {
        loop {
            let line = timeout(DEFAULT_TIMEOUT, self.lines.next_line())
                .await
                .map_err(|_| anyhow!("mcp {}: timeout waiting for reply {}", self.name, id))??
                .ok_or_else(|| anyhow!("mcp {}: server closed stdout", self.name))?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
                continue;
            };
            if v.get("id").and_then(Value::as_u64) == Some(id) {
                return Ok(v);
            }
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await?;
        let resp = self.recv_id(id).await?;
        if let Some(err) = resp.get("error") {
            return Err(anyhow!("mcp {}: {method}: {err}", self.name));
        }
        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    pub async fn initialize(&mut self) -> Result<()> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "kiri-engine", "version": env!("CARGO_PKG_VERSION")}
            }),
        )
        .await?;
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await?;

        let list = self.request("tools/list", json!({})).await?;
        self.tools = list
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|t| McpTool {
                name: t
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("?")
                    .to_string(),
                description: t
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                input_schema: t
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            })
            .collect();
        Ok(())
    }

    pub async fn call(&mut self, tool: &str, args: Value) -> Result<Value> {
        self.request("tools/call", json!({"name": tool, "arguments": args}))
            .await
    }

    pub async fn read_resource(&mut self, uri: &str) -> Result<Value> {
        self.request("resources/read", json!({"uri": uri})).await
    }

    pub fn kill(&mut self) {
        let _ = self.child.start_kill();
    }
}

fn server_config(rt: &Arc<Runtime>, name: &str) -> Option<Value> {
    rt.setting_array("mcpServers")
        .into_iter()
        .find(|s| s.get("name").and_then(Value::as_str) == Some(name))
}

fn env_of(cfg: &Value) -> std::collections::HashMap<String, String> {
    cfg.get("env")
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn args_of(cfg: &Value) -> Vec<String> {
    cfg.get("args")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

async fn connect(name: &str, cfg: &Value) -> Result<Conn> {
    let transport = cfg
        .get("type")
        .or_else(|| cfg.get("transport"))
        .and_then(Value::as_str)
        .unwrap_or(if cfg.get("url").is_some() { "http" } else { "stdio" });

    if transport != "stdio" {
        return Err(anyhow!(
            "mcp {name}: transport {transport} is not supported by this build; use stdio"
        ));
    }
    let command = cfg
        .get("command")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("mcp {name}: stdio server needs a command"))?;
    let mut conn = Conn::open_stdio(name, command, &args_of(cfg), &env_of(cfg)).await?;
    conn.initialize().await?;
    Ok(conn)
}

/// Tool definitions for every configured server. A server that fails to
/// connect reports its error to the GUI and contributes no tools; it never
/// prevents the other servers from working.
pub async fn list(rt: &Arc<Runtime>) -> Vec<Value> {
    let mut out = Vec::new();
    for cfg in rt.setting_array("mcpServers") {
        let name = cfg
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("server")
            .to_string();
        let transport = cfg
            .get("type")
            .or_else(|| cfg.get("transport"))
            .and_then(Value::as_str)
            .unwrap_or(if cfg.get("url").is_some() { "http" } else { "stdio" })
            .to_string();
        let disabled = cfg.get("enabled").and_then(Value::as_bool) == Some(false);
        if disabled || transport != "stdio" {
            out.push(json!({
                "name": name,
                "transport": transport,
                "status": "disconnected",
                "last_error": if transport == "stdio" { Value::Null } else { Value::String(format!("transport {transport} is not supported by this build; use stdio")) },
                "tools": [],
            }));
            continue;
        }
        match connect(&name, &cfg).await {
            Ok(mut conn) => {
                let tools: Vec<Value> = conn.tools.iter().map(|t| t.to_json()).collect();
                conn.kill();
                out.push(json!({
                    "name": name,
                    "transport": "stdio",
                    "status": "connected",
                    "last_error": Value::Null,
                    "tools": tools,
                }));
            }
            Err(e) => out.push(json!({
                "name": name,
                "transport": "stdio",
                "status": "error",
                "last_error": e.to_string(),
                "tools": [],
            })),
        }
    }
    out
}

/// Split `mcp__server__tool` into its two halves.
pub fn split_name(full: &str) -> Option<(String, String)> {
    let rest = full.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    Some((server.to_string(), tool.to_string()))
}

pub async fn call_tool(rt: &Arc<Runtime>, full: &str, input: &Value) -> ToolResult {
    let Some((server, tool)) = split_name(full) else {
        return ToolResult::err(format!("bad mcp tool name: {full}"));
    };
    let Some(cfg) = server_config(rt, &server) else {
        return ToolResult::err(format!("mcp: no configured server named {server}"));
    };
    let mut conn = match connect(&server, &cfg).await {
        Ok(c) => c,
        Err(e) => return ToolResult::err(format!("mcp: {e}")),
    };
    if !conn.tools.iter().any(|t| t.name == tool) {
        conn.kill();
        let known: Vec<&str> = conn.tools.iter().map(|t| t.name.as_str()).collect();
        return ToolResult::err(format!(
            "mcp: {server} has no tool {tool}; known: {}",
            known.join(", ")
        ));
    }
    let res = conn.call(&tool, input.clone()).await;
    conn.kill();
    match res {
        Ok(result) => {
            if result.get("isError").and_then(Value::as_bool).unwrap_or(false) {
                ToolResult::err(result.to_string())
            } else {
                let text = result
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|b| b.get("text").and_then(Value::as_str))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_else(|| result.to_string());
                ToolResult::text(text)
            }
        }
        Err(e) => ToolResult::err(e.to_string()),
    }
}

/// `control_request: readMcpResource` - reads a resource through the engine's
/// own path rules. The GUI never speaks MCP itself.
pub async fn read_resource(
    rt: &Arc<Runtime>,
    body: &crate::protocol::ControlRequestBody,
) -> Result<Value, String> {
    let (Some(server), Some(uri)) = (body.server.clone(), body.uri.clone()) else {
        return Err("readMcpResource: server and uri are required".into());
    };
    let Some(cfg) = server_config(rt, &server) else {
        return Err(format!("mcp: no configured server named {server}"));
    };
    let mut conn = connect(&server, &cfg).await.map_err(|e| e.to_string())?;
    let res = conn.read_resource(&uri).await.map_err(|e| e.to_string());
    conn.kill();
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_name_parses_the_namespace() {
        assert_eq!(
            split_name("mcp__srv__tool"),
            Some(("srv".into(), "tool".into()))
        );
        assert_eq!(split_name("Read"), None);
        assert_eq!(split_name("mcp__srv"), None);
    }

    /// A minimal stdio MCP server: answers initialize / tools/list / tools/call
    /// on newline-delimited JSON-RPC. Written to a temp file per test run.
    const MOCK_SERVER: &str = r#"
import sys, json
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    m = json.loads(line)
    mid = m.get("id")
    method = m.get("method")
    if method == "initialize":
        res = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}},
               "serverInfo": {"name": "mock", "version": "0"}}
    elif method == "tools/list":
        res = {"tools": [{"name": "echo", "description": "echo back",
                          "inputSchema": {"type": "object",
                                          "properties": {"text": {"type": "string"}}}}]}
    elif method == "tools/call":
        text = m.get("params", {}).get("arguments", {}).get("text", "")
        res = {"content": [{"type": "text", "text": text}]}
    else:
        res = {}
    if mid is not None:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": res}) + "\n")
        sys.stdout.flush()
"#;

    fn runtime_with_mock_server(script: &std::path::Path) -> Arc<Runtime> {
        let mut settings = crate::session_state::Settings::default();
        settings.merged.insert(
            "mcpServers".into(),
            json!([{ "name": "mock", "command": "python", "args": [script.to_string_lossy()] }]),
        );
        let mut cfg = crate::runtime::tests::test_config();
        cfg.session_id = "mcp-test".into();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        Runtime::new(
            cfg,
            settings,
            tx,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            crate::permissions::Mode::Default,
        )
    }

    /// Discovery must really connect: initialize, tools/list, and a call all
    /// round-trip through the child process. Skipped only where no python
    /// exists to run the mock (the test says so; it never fakes a pass).
    #[tokio::test]
    async fn stdio_discovery_lists_and_calls_tools() {
        if crate::tools::which("python").is_none() {
            eprintln!("SKIP: python not available for the mock MCP server");
            return;
        }
        let dir = std::env::temp_dir().join(format!("kiri-mcp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("mock_mcp.py");
        std::fs::write(&script, MOCK_SERVER).unwrap();

        let rt = runtime_with_mock_server(&script);
        let servers = list(&rt).await;
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0]["status"], "connected", "{:?}", servers[0]);
        assert_eq!(servers[0]["tools"][0]["name"], "echo");

        // The discovered schema reaches the model's tool list namespaced.
        rt.set_mcp_servers(servers);
        let defs = crate::agent::build_tools(&rt);
        assert!(
            defs.iter().any(|d| d["name"] == "mcp__mock__echo"),
            "discovered tool not injected: {defs:?}"
        );

        // And a call round-trips through the same server.
        let out = call_tool(&rt, "mcp__mock__echo", &json!({"text": "hi"})).await;
        assert!(!out.is_error, "{:?}", out.content);
        assert_eq!(out.content[0]["text"], "hi");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Unsupported transports keep an honest error instead of a fake connect.
    #[tokio::test]
    async fn http_transport_reports_unsupported() {
        let mut settings = crate::session_state::Settings::default();
        settings.merged.insert(
            "mcpServers".into(),
            json!([{ "name": "remote", "type": "http", "url": "https://example.invalid/mcp" }]),
        );
        let cfg = crate::runtime::tests::test_config();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let rt = Runtime::new(
            cfg,
            settings,
            tx,
            Vec::new(),
            Vec::new(),
            Vec::new(),
            crate::permissions::Mode::Default,
        );
        let servers = list(&rt).await;
        assert_eq!(servers[0]["status"], "disconnected");
        let err = servers[0]["last_error"].as_str().unwrap_or("");
        assert!(err.contains("not supported"), "{err}");
        assert!(servers[0]["tools"].as_array().unwrap().is_empty());
    }
}
