// Wire types for the engine <-> GUI protocol.
// Verified shape (recon): stdio, one JSON object per line, no
// Content-Length framing. The control channel is one nested envelope:
//   {type:"control_response", response:{subtype, request_id, response:{...}}}
// The request_id echo lives at the SECOND level.

use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum Incoming {
    #[serde(rename = "user")]
    User(UserMessage),
    #[serde(rename = "control_request")]
    Control(ControlRequest),
    #[serde(rename = "control_response")]
    ControlResponse(ControlResponse),
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct UserMessage {
    #[serde(default)]
    pub message: Option<Value>,
    #[serde(default)]
    pub parent_tool_use_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ControlRequest {
    pub request_id: String,
    pub request: ControlRequestBody,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ControlResponse {
    pub response: ControlResponseBody,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ControlResponseBody {
    pub subtype: String,
    pub request_id: String,
    #[serde(default)]
    pub response: Option<Value>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ControlRequestBody {
    #[serde(default)]
    pub subtype: String,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub offset: Option<u64>,
    #[serde(default)]
    pub limit: Option<u64>,
    #[serde(default)]
    pub cancel_queued: Option<bool>,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub uri: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub hooks: Option<Value>,
    #[serde(default)]
    pub permission_mode: Option<String>,
    #[serde(default)]
    pub updated_input: Option<Value>,
}

/// A permission decision sent back by the GUI.
#[derive(Debug, Clone, Deserialize)]
pub struct CanUseToolResponse {
    pub behavior: String,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub updated_input: Option<Value>,
    #[serde(default)]
    pub destination: Option<String>,
    #[serde(default)]
    pub rule: Option<Value>,
}

/// The single envelope the GUI reads for a control reply.
pub fn control_response(request_id: &str, payload: Value) -> Value {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": payload
        }
    })
}

pub fn control_error(request_id: &str, message: &str) -> Value {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "error",
            "request_id": request_id,
            "error": message
        }
    })
}
