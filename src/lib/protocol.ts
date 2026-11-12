// The wire types, exactly as the engine emits them (recon + architecture.md).
// One NDJSON object per line; five core types plus the control envelope.

export type EngineLine =
  | SystemLine
  | AssistantLine
  | UserLine
  | StreamEventLine
  | ResultLine
  | ControlRequestLine
  | ControlResponseLine
  | UnknownLine;

export interface SystemLine {
  type: "system";
  subtype: "init" | "compact_boundary" | "informational" | "worker_shutting_down" | string;
  session_id?: string;
  [key: string]: unknown;
}

export interface AssistantLine {
  type: "assistant";
  message: ApiMessage;
  session_id?: string;
  parent_tool_use_id?: string | null;
}

export interface UserLine {
  type: "user";
  message: ApiMessage;
  session_id?: string;
  parent_tool_use_id?: string | null;
}

export interface StreamEventLine {
  type: "stream_event";
  event: { type: string; [k: string]: unknown };
}

export interface ResultLine {
  type: "result";
  subtype: string;
  is_error: boolean;
  duration_ms: number;
  total_cost_usd: number;
  total_cost_micros: number;
  usage: TokenUsage;
  session_id: string;
  permission_denials: unknown[];
  num_turns: number;
  error?: string;
}

export interface ControlRequestLine {
  type: "control_request";
  request_id: string;
  request: {
    subtype: string;
    tool_name?: string;
    input?: unknown;
    timeout_s?: number;
    reason?: string;
    suggestions?: Suggestion[];
    blocked_path?: string | null;
    agent_id?: string | null;
    permission_mode?: string;
  };
}

export interface ControlResponseLine {
  type: "control_response";
  response: {
    subtype: "success" | "error";
    request_id: string;
    response?: unknown;
    error?: string;
  };
}

export interface UnknownLine {
  type: string;
  [k: string]: unknown;
}

export interface ApiMessage {
  id?: string;
  type?: "message";
  role?: "assistant" | "user";
  model?: string;
  content: ContentBlock[];
  stop_reason?: string | null;
  usage?: TokenUsage;
  [k: string]: unknown;
}

export type ContentBlock =
  | TextBlock
  | ThinkingBlock
  | ToolUseBlock
  | ToolResultBlock
  | ImageBlock
  | DocumentBlock
  | UnknownBlock;

export interface TextBlock {
  type: "text";
  text: string;
}

export interface ThinkingBlock {
  type: "thinking";
  thinking: string;
  signature?: string;
}

export interface ToolUseBlock {
  type: "tool_use";
  id: string;
  name: string;
  input: unknown;
}

export interface ToolResultBlock {
  type: "tool_result";
  tool_use_id: string;
  content?: unknown;
  is_error?: boolean;
  duration_ms?: number;
}

export interface ImageBlock {
  type: "image";
  source: { type: "base64"; media_type: string; data: string };
}

export interface DocumentBlock {
  type: "document";
  source: { type: "base64"; media_type: string; data: string };
}

export interface UnknownBlock {
  type: string;
  [k: string]: unknown;
}

export interface TokenUsage {
  input_tokens?: number;
  cache_creation_input_tokens?: number;
  cache_read_input_tokens?: number;
  output_tokens?: number;
}

export interface Suggestion {
  rule: string;
  label: string;
  destination: string;
}

export type AssistantContent = { blocks: ContentBlock[]; model: string; messageId: string };
