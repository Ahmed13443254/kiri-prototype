// One reducer per session stream (architecture.md: "one NDJSON reader feeds one
// reducer"). Pure: the reducer never calls the engine and never reads the clock.
// Ordering, grouping and "cut" are decided here so every window replays the
// same transcript identically.

import type { AssistantLine, ContentBlock, ResultLine, Suggestion, UserLine } from "./protocol";

export type Entry =
  | { kind: "user"; id: string; text: string; at: string }
  | {
      kind: "assistant";
      id: string;
      messageId: string;
      model: string;
      blocks: ContentBlock[];
      at: string;
      streaming?: boolean;
      cut?: boolean;
    }
  | {
      kind: "tool";
      id: string;
      toolUseId: string;
      name: string;
      input: unknown;
      result?: unknown;
      isError?: boolean;
      durationMs?: number;
      status: "running" | "done" | "error" | "denied" | "stopped";
      at: string;
      agentId?: string | null;
    }
  | { kind: "system"; id: string; subtype: string; text: string; at: string; data?: unknown }
  | { kind: "notice"; id: string; level: "info" | "warning" | "error"; text: string; at: string }
  | { kind: "boundary"; id: string; trigger: "auto" | "manual"; preTokens: number; postTokens: number; at: string };

export interface AssistantEntryGroup {
  entries: Entry[];
  lastTurn?: string;
}

export interface PermissionPrompt {
  requestId: string;
  toolName: string;
  input: unknown;
  reason?: string;
  suggestions: Suggestion[];
  blockedPath?: string | null;
  agentId?: string | null;
  permissionMode?: string;
  status: "pending" | "allowed" | "denied" | "always_allow" | "cancelled" | "resolved";
  createdAt: string;
}

export interface QuestionCard {
  requestId: string;
  questions: unknown[];
  status: "pending" | "answered" | "timed_out" | "cancelled";
  createdAt: string;
}

export interface UsageTotals {
  input_tokens: number;
  cache_creation_input_tokens: number;
  cache_read_input_tokens: number;
  output_tokens: number;
}

export interface SessionState {
  sessionId: string;
  cwd: string;
  model: string;
  effort: string;
  permissionMode: string;
  status: "idle" | "running" | "done" | "interrupted" | "error";
  entries: Entry[];
  prompts: PermissionPrompt[];
  questions: QuestionCard[];
  usage: UsageTotals;
  costMicros: number;
  lastResult?: ResultLine;
  /** Streaming text of the in-flight answer, keyed by block index, never reflowed. */
  live: Record<number, string>;
  /** Streaming thinking, kept apart from `live` so the two do not interleave. */
  liveThinking: Record<number, string>;
  init?: unknown;
}

export type Action =
  | { type: "init"; line: any }
  | { type: "assistant"; line: AssistantLine }
  | { type: "user"; line: UserLine }
  | { type: "stream"; line: any }
  | { type: "result"; line: ResultLine }
  | { type: "system"; line: any }
  | { type: "control_request"; line: any }
  | { type: "permission_resolved"; requestId: string; decision: string }
  | { type: "question_answered"; requestId: string; answers: unknown; response?: string }
  | { type: "turn_cut"; reason: string }
  | { type: "raw"; line: unknown };

export function emptyState(sessionId: string): SessionState {
  return {
    sessionId,
    cwd: "",
    model: "",
    effort: "",
    permissionMode: "default",
    status: "idle",
    entries: [],
    prompts: [],
    questions: [],
    usage: {
      input_tokens: 0,
      cache_creation_input_tokens: 0,
      cache_read_input_tokens: 0,
      output_tokens: 0,
    },
    costMicros: 0,
    live: {},
    liveThinking: {},
  };
}

let seq = 0;
function nextId(prefix: string): string {
  seq += 1;
  return `${prefix}-${seq}`;
}

const stamp = () => new Date().toISOString();

/** Text that arrived as one message is grouped by message id, not by index. */
export function reduce(state: SessionState, action: Action): SessionState {
  switch (action.type) {
    case "init": {
      const line = action.line as any;
      return {
        ...state,
        cwd: line.cwd ?? state.cwd,
        model: line.model ?? state.model,
        effort: line.effort ?? state.effort,
        permissionMode: line.permissionMode ?? state.permissionMode,
        init: line,
      };
    }    case "assistant": {
      const msg = action.line.message;
      const blocks = Array.isArray(msg.content) ? msg.content : [];
      const id = msg.id ?? nextId("msg");
      const model = msg.model ?? state.model;

      // The real block arrives: its streamed shadow is finished, not reflowed.
      const live = { ...state.live };
      const liveThinking = { ...state.liveThinking };
      for (let i = 0; i < blocks.length; i += 1) {
        delete live[i];
        delete liveThinking[i];
      }

      const entries = [...state.entries];
      // A tool_use block becomes a tool card at the moment it is announced.
      // Waiting for the `user` tool_result would render a call that is still
      // running as if it had never happened.
      for (const b of blocks) {
        if ((b as any).type !== "tool_use") continue;
        const tool = b as any;
        if (entries.some((e) => e.kind === "tool" && e.toolUseId === tool.id)) continue;
        entries.push({
          kind: "tool",
          id: nextId("t"),
          toolUseId: tool.id,
          name: tool.name,
          input: tool.input,
          status: "running",
          at: stamp(),
        });
      }

      const group = entries.findIndex(
        (e) => e.kind === "assistant" && e.messageId === id,
      );
      if (group >= 0) {
        const prev = entries[group] as Extract<Entry, { kind: "assistant" }>;
        entries[group] = { ...prev, blocks: [...prev.blocks, ...blocks], model, streaming: false };
      } else {
        entries.push({
          kind: "assistant",
          id: nextId("a"),
          messageId: id,
          model,
          blocks: [...blocks],
          at: stamp(),
          streaming: false,
        });
      }
      return { ...state, entries, live, liveThinking, status: "running" };
    }

    case "user": {
      const msg = action.line.message;
      const blocks = Array.isArray(msg.content) ? msg.content : [];
      const next = { ...state, entries: [...state.entries] };
      for (const raw of blocks) {
        const b = raw as any;
        if (b?.type === "text" && typeof b.text === "string") {
          next.entries.push({ kind: "user", id: nextId("u"), text: b.text, at: stamp() });
          continue;
        }
        if (b?.type !== "tool_result") continue;
        const idx = next.entries.findIndex(
          (e) => e.kind === "tool" && e.toolUseId === b.tool_use_id,
        );
        if (idx < 0) {
          // A result with no tool card (resumed transcript) still renders.
          next.entries.push({
            kind: "tool",
            id: nextId("t"),
            toolUseId: String(b.tool_use_id),
            name: "(result)",
            input: null,
            result: b.content,
            isError: Boolean(b.is_error),
            status: b.is_error ? "error" : "done",
            at: stamp(),
          });
          continue;
        }
        const entry = next.entries[idx];
        if (entry.kind === "tool") {
          entry.result = b.content;
          entry.isError = Boolean(b.is_error);
          entry.durationMs = (b as any).duration_ms;
          entry.status = b.is_error ? "error" : "done";
        }
      }
      return next;
    }

    case "stream": {
      const ev = action.line.event;
      if (!ev) return state;
      const idx = Number(ev.index ?? 0);

      // A tool call opens its card on the start event only; the card is filled
      // by the `assistant` message that follows, never by partial input_json.
      if (ev.type === "content_block_start" && ev.content_block?.type === "tool_use") {
        const block = ev.content_block;
        if (state.entries.some((e) => e.kind === "tool" && e.toolUseId === block.id)) return state;
        return {
          ...state,
          entries: [
            ...state.entries,
            {
              kind: "tool",
              id: nextId("t"),
              toolUseId: block.id,
              name: block.name,
              input: {},
              status: "running",
              at: stamp(),
            },
          ],
        };
      }
      if (ev.type !== "content_block_delta") return state;

      if (ev.delta?.type === "text_delta") {
        return {
          ...state,
          live: { ...state.live, [idx]: (state.live[idx] ?? "") + (ev.delta.text ?? "") },
        };
      }
      if (ev.delta?.type === "thinking_delta") {
        return {
          ...state,
          liveThinking: {
            ...state.liveThinking,
            [idx]: (state.liveThinking[idx] ?? "") + (ev.delta.thinking ?? ""),
          },
        };
      }
      return state;
    }

    case "control_request": {
      const req = action.line.request ?? {};
      const requestId = action.line.request_id;
      // The engine can repeat a request after reattach. A repeated id updates
      // the existing row; it must never stack a second dialog.
      if (state.questions.some((q) => q.requestId === requestId)) {
        return {
          ...state,
          questions: state.questions.map((q) =>
            q.requestId === requestId
              ? { ...q, questions: req.input?.questions ?? q.questions }
              : q,
          ),
        };
      }
      if (state.prompts.some((p) => p.requestId === requestId)) {
        return {
          ...state,
          prompts: state.prompts.map((p) =>
            p.requestId === requestId
              ? {
                  ...p,
                  toolName: req.tool_name ?? p.toolName,
                  input: req.input ?? p.input,
                  reason: req.reason ?? p.reason,
                  suggestions: req.suggestions ?? p.suggestions,
                  blockedPath: req.blocked_path ?? p.blockedPath,
                  agentId: req.agent_id ?? p.agentId,
                  permissionMode: req.permission_mode ?? p.permissionMode,
                }
              : p,
          ),
        };
      }
      if (req.tool_name === "AskUserQuestion") {
        return {
          ...state,
          questions: [
            ...state.questions,
            { requestId, questions: req.input?.questions ?? [], status: "pending", createdAt: stamp() },
          ],
        };
      }
      return {
        ...state,
        prompts: [
          ...state.prompts,
          {
            requestId,
            toolName: req.tool_name ?? "tool",
            input: req.input ?? {},
            reason: req.reason,
            suggestions: req.suggestions ?? [],
            blockedPath: req.blocked_path ?? null,
            agentId: req.agent_id ?? null,
            permissionMode: req.permission_mode,
            status: "pending",
            createdAt: stamp(),
          },
        ],
      };
    }

    case "permission_resolved":
      return {
        ...state,
        prompts: state.prompts.map((p) =>
          p.requestId === action.requestId ? { ...p, status: action.decision as PermissionPrompt["status"] } : p,
        ),
      };

    case "question_answered":
      return {
        ...state,
        questions: state.questions.map((q) =>
          q.requestId === action.requestId ? { ...q, status: "answered" } : q,
        ),
      };

    case "result": {
      const r = action.line;
      const u = r.usage ?? {};
      return {
        ...state,
        status: r.subtype === "success" ? "done" : r.is_error ? "error" : "interrupted",
        lastResult: r,
        usage: {
          input_tokens: state.usage.input_tokens + (u.input_tokens ?? 0),
          cache_creation_input_tokens:
            state.usage.cache_creation_input_tokens + (u.cache_creation_input_tokens ?? 0),
          cache_read_input_tokens:
            state.usage.cache_read_input_tokens + (u.cache_read_input_tokens ?? 0),
          output_tokens: state.usage.output_tokens + (u.output_tokens ?? 0),
        },
        costMicros: r.total_cost_micros ?? state.costMicros,
        live: {},
        liveThinking: {},
      };
    }

    case "system": {
      const line = action.line as any;
      if (line.subtype === "compact_boundary") {
        return {
          ...state,
          entries: [
            ...state.entries,
            {
              kind: "boundary",
              id: nextId("b"),
              trigger: line.trigger === "manual" ? "manual" : "auto",
              preTokens: line.pre_tokens ?? 0,
              postTokens: line.post_tokens ?? 0,
              at: stamp(),
            },
          ],
        };
      }
      if (line.subtype === "informational") {
        const level = line.level === "error" ? "error" : line.level === "warning" ? "warning" : "info";
        return {
          ...state,
          entries: [
            ...state.entries,
            { kind: "notice", id: nextId("n"), level, text: String(line.message ?? ""), at: stamp() },
          ],
        };
      }
      return state;
    }

    case "turn_cut":
      return {
        ...state,
        status: "interrupted",
        entries: [
          ...state.entries,
          { kind: "notice", id: nextId("n"), level: "warning", text: action.reason, at: stamp() },
        ],
      };

    case "raw":
      return state;

    default:
      return state;
  }
}

export function entriesByTurn(entries: Entry[]): Entry[] {
  return entries;
}
