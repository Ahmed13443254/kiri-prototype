// Thin typed wrapper over the host commands. There is no fetch() here and no
// URL: the renderer speaks to the Rust host, the host owns the child's pipes.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { EngineLine } from "./protocol";

export interface ProjectRow {
  id: string;
  path: string;
  name: string;
  lastOpenedAt?: string;
  sessionCount?: number;
}

export interface SessionRow {
  id: string;
  title: string;
  model: string;
  effort?: string | null;
  permissionMode: string;
  status: string;
  transcriptPath: string;
  missingTranscript: boolean;
  pid?: number | null;
  forkOf?: string | null;
  costMicros: number;
  createdAt: string;
  updatedAt: string;
  turnCount: number;
}

export interface MessageRow {
  rowid: number;
  id: string;
  turnId?: number | null;
  parentUuid?: string | null;
  role: "user" | "assistant" | "system";
  messageId?: string | null;
  isSidechain: boolean;
  createdAt: string;
  blocks: BlockRow[];
}

export interface BlockRow {
  idx: number;
  kind: "text" | "thinking" | "tool_use" | "tool_result" | "image" | "other";
  text?: string | null;
  toolUseId?: string | null;
  isError: boolean;
  json?: unknown;
}

export interface SearchHit {
  sessionId: string;
  messageId: string;
  blockId: number;
  kind: string;
  snippet: string;
  sessionTitle?: string | null;
  createdAt: string;
}

export interface RuleRow {
  id: string;
  ruleType: "allow" | "deny" | "ask";
  tool: string;
  pattern?: string | null;
  scope: string;
  origin: string;
  enabled: boolean;
  createdAt: string;
}

export interface SettingRow {
  key: string;
  value: string;
  source: string;
  filePath?: string | null;
  updatedAt: string;
}

export interface VaultStatus {
  present: boolean;
  backend: string;
  hasEnvKey: boolean;
}

export interface TaskRow {
  id: string;
  kind: string;
  subject: string;
  description?: string | null;
  activeForm?: string | null;
  status: string;
  owner?: string | null;
  subagentId?: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface SubagentRow {
  id: string;
  parentCallId?: string | null;
  agentType: string;
  agentName?: string | null;
  model?: string | null;
  isolation?: string | null;
  status: string;
  result?: string | null;
  worktree?: string | null;
  costMicros: number;
  createdAt: string;
  endedAt?: string | null;
}

export interface CheckpointRow {
  id: string;
  path: string;
  blobSha?: string | null;
  existedPre: boolean;
  turnId?: number | null;
  createdAt: string;
}

export interface RestoreReport {
  written: string[];
  deleted: string[];
}

export interface CostRollup {
  projectMicros: number;
  sessions: { sessionId: string; costMicros: number }[];
  turns: { turnId: number | null; costMicros: number }[];
}

export interface TranscriptRow {
  sessionId: string;
  path: string;
  bytes: number;
  updatedAt: string;
}

export const api = {
  openProject: (path: string, name?: string) =>
    invoke<ProjectRow>("open_project", { path, name }),

  listProjects: () => invoke<ProjectRow[]>("list_projects"),

  listSessions: (projectId: string) =>
    invoke<SessionRow[]>("list_sessions", { projectId }),

  getSession: (projectId: string, sessionId: string) =>
    invoke<SessionRow | null>("get_session", { projectId, sessionId }),

  deleteSession: (projectId: string, sessionId: string) =>
    invoke<void>("delete_session", { projectId, sessionId }),

  readMessages: (projectId: string, sessionId: string, limit = 200, beforeRowid?: number) =>
    invoke<MessageRow[]>("read_messages", {
      projectId,
      sessionId,
      limit,
      beforeRowid,
    }),

  spawnSession: (args: {
    projectId: string;
    workspaceId?: string;
    sessionId?: string;
    cwd?: string;
    model?: string;
    effort?: string;
    permissionMode: string;
    resume?: string;
    forkOf?: string;
  }) => invoke<{ sessionId: string; pid: number }>("spawn_session", args),

  sendPrompt: (sessionId: string, text: string) =>
    invoke<void>("send_prompt", { sessionId, text }),

  interrupt: (sessionId: string, cancelQueued = false) =>
    invoke<{ requestId: string }>("interrupt_session", { sessionId, cancelQueued }),

  answerPermission: (args: {
    sessionId: string;
    requestId: string;
    behavior: "allow" | "deny";
    message?: string;
    updatedInput?: unknown;
    destination?: string;
    rule?: unknown;
    answers?: unknown;
    response?: string;
  }) => invoke<void>("answer_permission", args),

  setPermissionMode: (sessionId: string, mode: string) =>
    invoke<void>("set_permission_mode", { sessionId, mode }),

  setModel: (sessionId: string, model: string, effort?: string) =>
    invoke<void>("set_model", { sessionId, model, effort }),

  contextUsage: (sessionId: string) => invoke<unknown>("context_usage", { sessionId }),

  compactSession: (sessionId: string) => invoke<unknown>("compact_session", { sessionId }),

  listTasks: (sessionId: string) => invoke<TaskRow[]>("list_tasks", { sessionId }),

  stopTask: (sessionId: string, taskId: string) =>
    invoke<unknown>("stop_task", { sessionId, taskId }),

  listSubagents: (sessionId: string) =>
    invoke<SubagentRow[]>("list_subagents", { sessionId }),

  costRollup: (projectId: string, since?: string) =>
    invoke<CostRollup>("cost_rollup", { projectId, since }),

  listRules: (projectId: string) => invoke<RuleRow[]>("list_rules", { projectId }),

  upsertRule: (args: {
    projectId: string;
    ruleType: string;
    tool: string;
    pattern?: string;
    scopeName?: string;
    origin?: string;
    enabled?: boolean;
  }) => invoke<void>("upsert_rule", args),

  deleteRule: (projectId: string, id: string) =>
    invoke<void>("delete_rule", { projectId, id }),

  listSettings: (projectId: string) =>
    invoke<SettingRow[]>("list_settings", { projectId }),

  setSetting: (projectId: string, key: string, value: string) =>
    invoke<void>("set_setting", { projectId, key, value }),

  listMemory: (projectId: string) =>
    invoke<{ path: string; scope: string }[]>("list_memory", { projectId }),

  vaultStatus: () => invoke<VaultStatus>("vault_status"),

  vaultSetKey: (key: string) =>
    invoke<{ present: boolean }>("vault_set_key", { key }),

  vaultClear: () => invoke<{ present: boolean }>("vault_clear"),

  listCheckpoints: (projectId: string, sessionId: string) =>
    invoke<CheckpointRow[]>("list_checkpoints", { projectId, sessionId }),

  checkpointDiff: (projectId: string, sessionId: string, checkpointId: string) =>
    invoke<{ path: string; existedPre: boolean; oldText?: string | null; newText?: string | null }>(
      "checkpoint_diff",
      { projectId, sessionId, checkpointId },
    ),

  restoreCheckpointFile: (projectId: string, sessionId: string, checkpointId: string) =>
    invoke<{ path: string; deleted: boolean }>("restore_checkpoint_file", { projectId, sessionId, checkpointId }),

  restoreCheckpoint: (projectId: string, sessionId: string, turnId: number) =>
    invoke<RestoreReport>("restore_checkpoint", { projectId, sessionId, turnId }),

  listTranscripts: (projectId: string) =>
    invoke<TranscriptRow[]>("list_transcripts", { projectId }),

  /** FTS5 over `blocks.text`. Never a `like '%...%'` scan. */
  searchBlocks: (projectId: string, query: string, limit = 50) =>
    invoke<SearchHit[]>("search_blocks", { projectId, query, limit }),

  /** Export a session transcript to markdown with its attachments. */
  exportSession: (projectId: string, sessionId: string) =>
    invoke<string>("export_session", { projectId, sessionId }),

  writeExport: (path: string, content: string) =>
    invoke<void>("write_export", { path, content }),

  /** Read a file through the engine's own path rules, not the renderer's. */
  readFile: (sessionId: string, path: string, offset?: number, limit?: number) =>
    invoke<unknown>("read_file", { sessionId, path, offset, limit }),
};

/** Subscribe to the host's engine pump. Order is the pipe's order. */
export async function onEngineEvent(
  cb: (ev: { sessionId: string; line: EngineLine }) => void,
): Promise<UnlistenFn> {
  return listen<{ sessionId: string; line: EngineLine }>("engine://event", (e) => cb(e.payload));
}

export async function onEngineLagged(cb: (n: number) => void): Promise<UnlistenFn> {
  return listen<{ dropped: number }>("engine://lagged", (e) => cb(e.payload.dropped));
}
