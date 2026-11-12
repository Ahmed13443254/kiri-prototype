// Tool executors. Failures are results, not panics.

pub mod misc;
pub mod readtools;
pub mod shell;

use serde_json::Value;
use std::sync::Arc;

use crate::runtime::Runtime;
use crate::tools::ToolResult;

pub fn arg_str(input: &Value, key: &str) -> Option<String> {
    input.get(key).and_then(Value::as_str).map(str::to_string)
}

pub fn arg_u64(input: &Value, key: &str) -> Option<u64> {
    input.get(key).and_then(Value::as_u64)
}

pub fn arg_bool(input: &Value, key: &str) -> Option<bool> {
    input.get(key).and_then(Value::as_bool)
}

pub fn skip_dir(name: &str) -> bool {
    matches!(
        name,
        "node_modules" | ".git" | "target" | "dist" | ".next" | "build" | ".venv" | "venv"
    )
}

pub async fn execute(rt: &Arc<Runtime>, name: &str, input: &Value) -> ToolResult {
    match name {
        other if other.starts_with("mcp__") => crate::mcp::call_tool(rt, other, input).await,
        other => Box::pin(misc::dispatch(rt, other, input)).await,
    }
}
