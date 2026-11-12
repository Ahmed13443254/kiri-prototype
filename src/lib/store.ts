// The GUI's live state. One SessionState per session stream; the store is the
// only place the renderer keeps data, and it is rebuilt from the index on open.

import { create } from "zustand";
import {
  api,
  onEngineEvent,
  onEngineLagged,
  type ProjectRow,
  type SessionRow,
  type MessageRow,
} from "./engine";
import type { EngineLine, ResultLine } from "./protocol";
import { emptyState, reduce, type SessionState, type Entry } from "./reducer";

interface Store {
  screen: "s01" | "s06" | "s08" | "s09" | "s10" | "s11" | "s13" | "s14";
  setScreen: (id: Store["screen"]) => void;
  projects: ProjectRow[];
  activeProject?: ProjectRow;
  sessions: SessionRow[];
  activeSession?: SessionRow;
  stream: SessionState;
  lagged: number;
  loading: boolean;
  error?: string;

  bootstrap: () => Promise<void>;
  openProject: (path: string, name?: string) => Promise<void>;
  pickProject: () => Promise<void>;
  selectProject: (p: ProjectRow) => Promise<void>;
  selectSession: (s: SessionRow) => Promise<void>;
  deleteSession: (s: SessionRow) => Promise<void>;
  newSession: () => Promise<SessionRow | undefined>;
  send: (text: string) => Promise<void>;
  interrupt: (cancelQueued?: boolean) => Promise<void>;
  decide: (
    requestId: string,
    behavior: "allow" | "deny",
    extra?: Record<string, unknown>,
  ) => Promise<void>;
  answerQuestion: (requestId: string, answers: unknown, response?: string) => Promise<void>;
  setMode: (mode: string) => Promise<void>;
  setModel: (model: string, effort?: string) => Promise<void>;
  handleLine: (sessionId: string, line: EngineLine) => void;
  loadSession: (s: SessionRow) => Promise<void>;
}

/**
 * The index stores one row per message and one per block. This turns a flat,
 * re-openable page of those rows back into the same Entry shapes the live pipe
 * produces, so a reopened session renders through one code path.
 */
/** The host reports transport failures in English; the shell copy is Arabic. */
function errText(e: unknown): string {
  const text = String(e);
  if (
    text.includes("__TAURI_INTERNALS__") ||
    text.includes("reading 'invoke'") ||
    text.includes("transformCallback")
  ) {
    return "تعذّر الوصول إلى مضيف kiri. هذه الواجهة تعمل داخل تطبيق kiri فقط.";
  }
  return text;
}

function messagesToEntries(messages: MessageRow[], fallbackModel = ""): Entry[] {
  const out: Entry[] = [];
  const toolAt = new Map<string, number>();

  for (const m of messages) {
    if (m.role === "user") {
      const text = m.blocks
        .filter((b) => b.kind === "text")
        .map((b) => b.text ?? "")
        .join("");
      for (const b of m.blocks) {
        if (b.kind !== "tool_result") continue;
        const id = String(b.toolUseId ?? "");
        const at = toolAt.get(id);
        if (at == null) {
          out.push({
            kind: "tool",
            id: `t-${m.id}-${b.idx}`,
            toolUseId: id,
            name: "(result)",
            input: null,
            result: b.text ?? b.json,
            isError: b.isError,
            status: b.isError ? "error" : "done",
            at: m.createdAt,
          });
          continue;
        }
        const card = out[at];
        if (card.kind === "tool") {
          out[at] = {
            ...card,
            result: b.text ?? b.json,
            isError: b.isError,
            status: b.isError ? "error" : "done",
          };
        }
      }
      if (text) out.push({ kind: "user", id: m.id, text, at: m.createdAt });
      continue;
    }

    if (m.role !== "assistant") continue;

    const blocks: any[] = [];
    for (const b of m.blocks) {
      if (b.kind === "text") {
        blocks.push({ type: "text", text: b.text ?? "" });
      } else if (b.kind === "thinking") {
        blocks.push({ type: "thinking", thinking: b.text ?? "" });
      } else if (b.kind === "tool_use") {
        const j = (b.json ?? {}) as { id?: string; name?: string; input?: unknown };
        const id = String(j.id ?? b.toolUseId ?? "");
        const name = String(j.name ?? "");
        blocks.push({ type: "tool_use", id, name, input: j.input ?? {} });
        toolAt.set(id, out.length);
        out.push({
          kind: "tool",
          id: `t-${m.id}-${b.idx}`,
          toolUseId: id,
          name,
          input: j.input ?? {},
          status: "running",
          at: m.createdAt,
        });
      }
    }
    out.push({
      kind: "assistant",
      id: m.id,
      messageId: m.messageId ?? m.id,
      model: fallbackModel,
      blocks,
      at: m.createdAt,
    });
  }
  return out;
}

const idle = (): SessionState => ({ ...emptyState(""), status: "idle" });

export const useStore = create<Store>((set, get) => ({
  screen: "s01",
  setScreen: (id) => set({ screen: id }),
  projects: [],
  sessions: [],
  stream: idle(),
  lagged: 0,
  loading: false,

  async bootstrap() {
    set({ loading: true });
    try {
      const projects = await api.listProjects();
      const first = projects[0];
      set({ projects, activeProject: first, loading: false, error: undefined });
      if (first) await get().selectProject(first);
    } catch (e) {
      set({ loading: false, error: errText(e) });
    }
  },

  async openProject(path, name) {
    set({ loading: true });
    try {
      const p = await api.openProject(path, name);
      const projects = await api.listProjects();
      const match = projects.find((x) => x.id === p.id) ?? p;
      set({ projects, activeProject: match, loading: false, error: undefined });
      await get().selectProject(match);
    } catch (e) {
      set({ loading: false, error: errText(e) });
    }
  },

  /** The native folder picker. Runs in the Rust host, never in the renderer. */
  async pickProject() {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const chosen = await open({
        directory: true,
        multiple: false,
        title: "افتح مجلد مشروع",
      });
      if (typeof chosen !== "string") return;
      await get().openProject(chosen);
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  async selectProject(p) {
    set({ activeProject: p, loading: true, error: undefined });
    try {
      const sessions = await api.listSessions(p.id);
      set({ sessions, loading: false });
      const first = sessions.find((s) => s.status === "active") ?? sessions[0];
      if (first) {
        await get().selectSession(first);
      } else {
        set({ activeSession: undefined, stream: idle() });
      }
    } catch (e) {
      set({ loading: false, error: errText(e) });
    }
  },

  async selectSession(s) {
    set({ activeSession: s });
    await get().loadSession(s);
  },

  async loadSession(s) {
    const project = get().activeProject;
    if (!project) return;

    set({ loading: true, error: undefined });

    // The transcript is the source of truth: load it from disk, not from memory.
    let stored: Entry[] = [];
    try {
      const messages = await api.readMessages(project.id, s.id);
      stored = messagesToEntries(messages, s.model);
    } catch {
      stored = [];
    }

    const prev = get().stream;
    const same = prev.sessionId === s.id;
    const base = same ? prev : emptyState(s.id);

    set({
      stream: {
        ...base,
        sessionId: s.id,
        cwd: project.path,
        model: s.model || base.model,
        effort: s.effort ?? base.effort,
        permissionMode: s.permissionMode || base.permissionMode,
        status: (s.status as SessionState["status"]) ?? base.status,
        entries: stored.length > 0 ? stored : base.entries,
      },
      loading: false,
    });
  },

  async deleteSession(s) {
    const project = get().activeProject;
    if (!project) return;
    try {
      await api.deleteSession(project.id, s.id);
      const sessions = get().sessions.filter((row) => row.id !== s.id);
      if (get().activeSession?.id !== s.id) {
        set({ sessions, error: undefined });
        return;
      }
      const next = sessions[0];
      set({ sessions, activeSession: next, stream: idle(), error: undefined });
      if (next) await get().loadSession(next);
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  async newSession(): Promise<SessionRow | undefined> {
    const project = get().activeProject;
    if (!project) {
      set({ error: "افتح مشروعاً أولاً." });
      return undefined;
    }
    const mode = get().stream.permissionMode || "default";
    const model = get().stream.model;
    const effort = get().stream.effort;
    try {
      const p = await api.spawnSession({
        projectId: project.id,
        cwd: project.path,
        model: model || undefined,
        effort: effort || undefined,
        permissionMode: mode,
      });
      const session: SessionRow = {
        id: p.sessionId,
        title: "جلسة جديدة",
        model: model || "claude-opus-5-5",
        effort: effort || null,
        permissionMode: mode,
        status: "active",
        transcriptPath: "",
        missingTranscript: false,
        pid: p.pid,
        costMicros: 0,
        createdAt: new Date().toISOString(),
        updatedAt: new Date().toISOString(),
        turnCount: 0,
      };
      set((st) => ({
        sessions: [session, ...st.sessions.filter((x) => x.id !== session.id)],
        activeSession: session,
        stream: { ...emptyState(session.id), model: session.model, effort: session.effort ?? "", permissionMode: mode },
        error: undefined,
      }));
      return session;
    } catch (e) {
      set({ error: errText(e) });
      return undefined;
    }
  },

  async send(text) {
    if (!text.trim()) return;
    let s = get().activeSession;
    if (!s) {
      s = await get().newSession();
      if (!s) return;
    }
    const current = get().stream;
    if (current.sessionId !== s.id) {
      set({ stream: { ...emptyState(s.id), model: s.model }, error: undefined });
    }
    set({
      stream: {
        ...get().stream,
        status: "running",
        entries: [...get().stream.entries, { kind: "user", id: `u-${Date.now()}`, text, at: new Date().toISOString() }],
      },
      error: undefined,
    });
    try {
      await api.sendPrompt(s.id, text);
    } catch (e) {
      const project = get().activeProject;
      if (project && String(e).includes("no live engine")) {
        try {
          const p = await api.spawnSession({
            projectId: project.id,
            cwd: project.path,
            model: s.model || undefined,
            effort: s.effort ?? undefined,
            permissionMode: s.permissionMode || "default",
            resume: s.id,
          });
          await api.sendPrompt(p.sessionId, text);
          return;
        } catch (resumeError) {
          set({ stream: { ...get().stream, status: "error" }, error: String(resumeError) });
          return;
        }
      }
      set({ stream: { ...get().stream, status: "error" }, error: errText(e) });
    }
  },

  async interrupt(cancelQueued) {
    const s = get().activeSession;
    if (!s) return;
    try {
      await api.interrupt(s.id, cancelQueued ?? false);
      set({ stream: { ...get().stream, status: "interrupted" } });
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  async decide(requestId, behavior, extra) {
    const s = get().activeSession;
    if (!s) {
      set({ error: "لا جلسة نشطة لاستلام القرار." });
      return;
    }
    set({ error: undefined });
    // Optimistic: the row is resolved on the engine side and in the index.
    set((st) => ({
      stream: {
        ...st.stream,
        prompts: st.stream.prompts.map((p) =>
          p.requestId === requestId
            ? { ...p, status: behavior === "allow" ? "allowed" : "denied" }
            : p,
        ),
      },
    }));
    try {
      await api.answerPermission({ sessionId: s.id, requestId, behavior, ...extra });
    } catch (e) {
      // The engine is still waiting; the prompt must not disappear.
      set((st) => ({
        error: errText(e),
        stream: {
          ...st.stream,
          prompts: st.stream.prompts.map((p) =>
            p.requestId === requestId ? { ...p, status: "pending" } : p,
          ),
        },
      }));
    }
  },

  async answerQuestion(requestId, answers, response) {
    const s = get().activeSession;
    if (!s) {
      set({ error: "لا جلسة نشطة لاستلام الإجابة." });
      return;
    }
    set((st) => ({
      stream: {
        ...st.stream,
        questions: st.stream.questions.map((q) =>
          q.requestId === requestId ? { ...q, status: "answered" } : q,
        ),
      },
    }));
    try {
      await api.answerPermission({
        sessionId: s.id,
        requestId,
        behavior: "allow",
        answers,
        response,
      });
    } catch (e) {
      set((st) => ({
        error: errText(e),
        stream: {
          ...st.stream,
          questions: st.stream.questions.map((q) =>
            q.requestId === requestId ? { ...q, status: "pending" } : q,
          ),
        },
      }));
    }
  },

  async setMode(mode) {
    const s = get().activeSession;
    set((st) => ({ stream: { ...st.stream, permissionMode: mode } }));
    if (!s) return;
    try {
      await api.setPermissionMode(s.id, mode);
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  async setModel(model, effort) {
    const s = get().activeSession;
    const nextEffort = effort ?? get().stream.effort;
    set((st) => ({
      stream: { ...st.stream, model, effort: nextEffort },
      activeSession: st.activeSession ? { ...st.activeSession, model, effort: nextEffort } : undefined,
      sessions: st.sessions.map((row) => row.id === s?.id ? { ...row, model, effort: nextEffort } : row),
      error: undefined,
    }));
    if (!s) return;
    try {
      await api.setModel(s.id, model, nextEffort);
    } catch (e) {
      set((st) => ({
        stream: st.activeSession?.id === s.id ? { ...st.stream, model: s.model, effort: s.effort ?? "" } : st.stream,
        activeSession: st.activeSession?.id === s.id ? s : st.activeSession,
        sessions: st.sessions.map((row) => row.id === s.id ? s : row),
        error: errText(e),
      }));
    }
  },

  handleLine(sessionId, line) {
    const st = get().stream;
    if (st.sessionId && st.sessionId !== sessionId) return;
    switch (line.type) {
      case "system":
        if ((line as any).subtype === "init") {
          set({ stream: reduce(st, { type: "init", line: line as any }) });
        } else {
          set({ stream: reduce(st, { type: "system", line: line as any }) });
        }
        break;
      case "assistant":
        set({ stream: reduce(st, { type: "assistant", line: line as any }) });
        break;
      case "user":
        set({ stream: reduce(st, { type: "user", line: line as any }) });
        break;
      case "stream_event":
        set({ stream: reduce(st, { type: "stream", line: line as any }) });
        break;
      case "result": {
        set({ stream: reduce(st, { type: "result", line: line as ResultLine }) });
        const project = get().activeProject;
        if (project) {
          void api.listSessions(project.id).then((sessions) => {
            set((state) => ({
              sessions,
              activeSession: sessions.find((row) => row.id === state.activeSession?.id) ?? state.activeSession,
            }));
          }).catch(() => {});
        }
        break;
      }
      case "control_request":
        set({ stream: reduce(st, { type: "control_request", line: line as any }) });
        break;
      default:
        break;
    }
  },
}));

export async function wireEngine(): Promise<() => void> {
  const un1 = await onEngineEvent(({ sessionId, line }) => {
    useStore.getState().handleLine(sessionId, line);
  });
  const un2 = await onEngineLagged((n) => {
    useStore.setState({ lagged: n });
  });
  return () => {
    un1();
    un2();
  };
}
