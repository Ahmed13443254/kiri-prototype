import { useEffect, useRef, useState } from "react";
import { Badge, EmptyState, IconButton, Mono } from "./primitives";
import { DiffView } from "./DiffView";
import { api } from "../lib/engine";
import type { Entry } from "../lib/reducer";
import type { ContentBlock } from "../lib/protocol";

/** One assistant message is one content block: the grouping lives upstream in
 * the reducer, so a stream of text renders as one bubble, not N. */

function blockText(b: ContentBlock): string {
  const any = b as any;
  if (typeof any.text === "string") return any.text;
  if (typeof any.thinking === "string") return any.thinking;
  return "";
}

function familyOf(name: string): string {
  switch (name) {
    case "Read":
    case "Glob":
    case "Grep":
    case "Skill":
    case "ToolSearch":
      return "read";
    case "Write":
      return "write";
    case "Edit":
    case "NotebookEdit":
      return "edit";
    case "Bash":
    case "BashOutput":
    case "TaskStop":
      return "bash";
    case "WebFetch":
    case "WebSearch":
      return "web";
    case "Agent":
      return "agent";
    case "AskUserQuestion":
      return "question";
    case "TodoWrite":
      return "todo";
    default:
      return name.startsWith("mcp__") ? "mcp" : "unknown";
  }
}

function verb(kind: string): string {
  switch (kind) {
    case "read":
      return "قراءة";
    case "write":
      return "كتابة ملف";
    case "edit":
      return "تعديل ملف";
    case "bash":
      return "تشغيل أمر";
    case "web":
      return "جلب من الويب";
    case "agent":
      return "تفويض لوكيل فرعي";
    case "question":
      return "سؤال المستخدم";
    case "todo":
      return "تحديث المهام";
    default:
      return "أداة";
  }
}

function statusBadge(status: string) {
  switch (status) {
    case "running":
      return (
        <Badge variant="running">
          <span className="k-dot" aria-hidden="true" />
          يعمل
        </Badge>
      );
    case "error":
      return <Badge variant="danger">فشل</Badge>;
    case "denied":
      return <Badge variant="danger">مرفوض</Badge>;
    case "stopped":
      return <Badge variant="warning">متوقف</Badge>;
    default:
      return <Badge variant="success">تمّ</Badge>;
  }
}

function toolInputText(input: unknown): string {
  try {
    const raw = JSON.stringify(input, null, 2);
    return raw.length > 1200 ? raw.slice(0, 1200) + "\n..." : raw;
  } catch {
    return String(input);
  }
}

function toolResultText(result: unknown): string {
  if (result == null) return "";
  if (typeof result === "string") return result;
  try {
    if (Array.isArray(result)) {
      return result
        .map((x) =>
          x && typeof x === "object" && typeof x.text === "string"
            ? x.text
            : typeof x === "string"
              ? x
              : JSON.stringify(x),
        )
        .join("\n");
    }
    const any = result as any;
    if (any && typeof any === "object" && Array.isArray(any.content)) {
      return toolResultText(any.content);
    }
    return JSON.stringify(any, null, 2);
  } catch {
    return "";
  }
}

function ElementRow(props: { name: string; input: unknown; result?: unknown; isError?: boolean; durationMs?: number; status: string }) {
  const fam = familyOf(props.name);
  const input = (props.input ?? {}) as any;
  const filePath = typeof input.file_path === "string" ? input.file_path : null;
  const writeBody = fam === "write" && typeof input.content === "string" ? input.content : null;
  const editBody =
    fam === "edit" && typeof input.old_string === "string" && typeof input.new_string === "string"
      ? { oldText: input.old_string, newText: input.new_string }
      : null;
  const inputText = toolInputText(props.input);
  const resultText = toolResultText(props.result);

  return (
    <details className="k-tool" open={props.status === "running"}>
      <summary>
        <span className="k-tool-name" dir="ltr">
          {props.name}
        </span>
        <span>{verb(fam)}</span>
        {statusBadge(props.status)}
        {props.durationMs != null ? <Mono>{Math.round(props.durationMs)}ms</Mono> : null}
        <svg className="k-tool__chevron" width="14" height="14" viewBox="0 0 16 16" fill="none" aria-hidden="true">
          <path d="m3.5 6 4.5 4 4.5-4" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
        </svg>
      </summary>
      {writeBody != null || editBody ? (
        <>
          {filePath ? (
            <p className="k-tool__path">
              <span>الملف</span>
              <Mono>{filePath}</Mono>
            </p>
          ) : null}
          <DiffView
            oldText={editBody ? editBody.oldText : ""}
            newText={editBody ? editBody.newText : (writeBody ?? "")}
            label={fam === "write" ? "محتوى الملف" : "تعديل الملف"}
          />
        </>
      ) : (
        <pre dir="ltr">
          <code>{inputText}</code>
        </pre>
      )}
      {resultText ? (
        <pre dir="ltr" className={props.isError ? "k-pre--error" : undefined}>
          <code>{resultText.length > 4000 ? resultText.slice(0, 4000) + "\n..." : resultText}</code>
        </pre>
      ) : null}
    </details>
  );
}

function transcriptMarkdown(entries: Entry[], liveText: string): string {
  const blocks = entries.map((entry) => {
    if (entry.kind === "user") return `## المستخدم\n\n${entry.text}`;
    if (entry.kind === "assistant") {
      const text = entry.blocks.filter((block) => block.type === "text").map(blockText).join("");
      return text ? `## المساعد\n\n${text}` : "";
    }
    if (entry.kind === "tool") {
      const input = JSON.stringify(entry.input, null, 2) ?? "";
      const result = typeof entry.result === "string" ? entry.result : JSON.stringify(entry.result, null, 2) ?? "";
      return [`### ${entry.name}`, "```json", input, "```", result ? "```text" : "", result, result ? "```" : ""].filter(Boolean).join("\n");
    }
    if (entry.kind === "system" || entry.kind === "notice") return `> ${entry.text}`;
    if (entry.kind === "boundary") return `---\nحدّ السياق: ${entry.preTokens} → ${entry.postTokens}`;
    return "";
  });
  if (liveText) blocks.push(`## المساعد\n\n${liveText}`);
  return blocks.filter(Boolean).join("\n\n");
}

export function Transcript({
  entries,
  live = {},
  liveThinking = {},
  onRetry,
  canRetry = false,
  busy = false,
  projectId,
  sessionId,
  title,
}: {
  entries: Entry[];
  live?: Record<number, string>;
  liveThinking?: Record<number, string>;
  onRetry?: () => void;
  canRetry?: boolean;
  busy?: boolean;
  projectId?: string;
  sessionId?: string;
  title?: string;
}) {
  const [copyState, setCopyState] = useState("");
  const scrollRef = useRef<HTMLDivElement>(null);
  const stickToBottom = useRef(true);
  const liveKeys = Object.keys(live);
  const liveThinkKeys = Object.keys(liveThinking);
  const liveText =
    liveKeys.length > 0
      ? liveKeys
          .sort((a, b) => Number(a) - Number(b))
          .map((k) => live[Number(k)])
          .join("")
      : "";
  const liveThought =
    liveThinkKeys.length > 0
      ? liveThinkKeys
          .sort((a, b) => Number(a) - Number(b))
          .map((k) => liveThinking[Number(k)])
          .join("")
      : "";

  async function copyTranscript() {
    try {
      await navigator.clipboard.writeText(transcriptMarkdown(entries, liveText));
      setCopyState("تم النسخ");
    } catch {
      setCopyState("تعذّر النسخ");
    }
  }

  async function exportTranscript() {
    if (!projectId || !sessionId) return;
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const path = await save({
        defaultPath: `${sessionId}.md`,
        filters: [{ name: "Markdown", extensions: ["md"] }],
      });
      if (!path) return;
      const markdown = await api.exportSession(projectId, sessionId);
      await api.writeExport(path, markdown);
      setCopyState("تم التصدير");
    } catch {
      setCopyState("تعذّر التصدير");
    }
  }

  useEffect(() => {
    const el = scrollRef.current;
    if (el && stickToBottom.current) el.scrollTop = el.scrollHeight;
  }, [entries, liveText, liveThought]);

  return (
    <div className="k-transcript-shell">
      <div className="k-toolbar" role="toolbar" aria-label="أدوات المحادثة">
        <span className="k-toolbar__title">{title || "محادثة جديدة"}</span>
        <span className="k-copy-state" role="status">{copyState}</span>
        <span className="k-spacer" />
        <IconButton label="تصدير المحادثة Markdown" disabled={!projectId || !sessionId || entries.length === 0} onClick={() => void exportTranscript()}>
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true">
            <path d="M8 2v8m0 0 3-3M8 10 5 7M3 12v1a1 1 0 0 0 1 1h8a1 1 0 0 0 1-1v-1" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        </IconButton>
        <IconButton label="إعادة آخر طلب" disabled={!canRetry || busy} onClick={onRetry}>
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true">
            <path d="M3 6a5 5 0 1 1 .5 5M3 2v4h4" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
          </svg>
        </IconButton>
        <IconButton label="نسخ المحادثة Markdown" disabled={entries.length === 0 && !liveText} onClick={() => void copyTranscript()}>
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true">
            <rect x="5" y="2" width="8" height="11" rx="1.5" stroke="currentColor" strokeWidth="1.5" />
            <path d="M11 13H4a2 2 0 0 1-2-2V5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          </svg>
        </IconButton>
      </div>
      <div
        className={`k-transcript k-scroll ${
          entries.length === 0 && !liveText && !liveThought ? "k-transcript--empty" : ""
        }`}
        ref={scrollRef}
        onScroll={() => {
          const el = scrollRef.current;
          if (el) stickToBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
        }}
      >
        <div className="k-conversation">
        {entries.length === 0 && !liveText && !liveThought ? (
          <EmptyState title="لا رسائل بعد" body="اكتب أول رسالة أو أرفق ملفًا لبدء المحادثة." />
        ) : null}
      {entries.map((e) => {
        switch (e.kind) {
          case "user":
            return (
              <article key={e.id} className="k-msg k-msg--user" aria-label="رسالة المستخدم">
                <p>{e.text}</p>
              </article>
            );
          case "assistant": {
            const text = e.blocks
              .filter((b) => (b as any).type === "text")
              .map(blockText)
              .join("");
            const thinking = e.blocks.filter((b): b is any => (b as any).type === "thinking");
            // A tool-only assistant block is already the tool card below; an
            // empty model badge above it would be noise, not a message.
            if (!text && thinking.length === 0) return null;
            return (
              <article key={e.id} className="k-msg" aria-label={`${e.model || "النموذج"} · رد`}>
                <div className="k-meta">
                  <Badge variant="neutral"><Mono>{e.model || "—"}</Mono></Badge>
                  {e.cut ? <span className="k-cut">انقطع الرد</span> : null}
                </div>
                {thinking.length > 0 ? (
                  <details className="k-thinking">
                    <summary>طيّ التفكير</summary>
                    <pre dir="ltr">
                      <code>{thinking.map((t) => String(t.thinking ?? "")).join("\n")}</code>
                    </pre>
                  </details>
                ) : null}
                {text ? <p className="k-text" dir="auto">{text}</p> : null}
              </article>
            );
          }
          case "tool":
            return (
              <div key={e.id} className="k-tool-wrap">
                <ElementRow
                  name={e.name}
                  input={e.input}
                  result={e.result}
                  isError={e.isError}
                  durationMs={e.durationMs}
                  status={e.status}
                />
              </div>
            );
          case "system":
            return (
              <div key={e.id} className="k-system-bar">
                <span>{e.subtype}</span>
                <span>{e.text}</span>
              </div>
            );
          case "notice":
            return (
              <div key={e.id} className={`k-notice k-notice--${e.level}`} role={e.level === "error" ? "alert" : "status"}>
                {e.text}
              </div>
            );
          case "boundary":
            return (
              <div key={e.id} className="k-boundary">
                <span>حدّ السياق</span>
                <Mono>{`${e.preTokens} → ${e.postTokens} tokens`}</Mono>
              </div>
            );
          default:
            return null;
        }
      })}

      {liveThought || liveText ? (
        <article className="k-msg k-msg--live" aria-live="off">
                              {liveThought ? (
            <details className="k-thinking" open>
              <summary>التفكير</summary>
              <pre dir="ltr">
                <code>{liveThought}</code>
              </pre>
            </details>
          ) : null}
          {liveText ? (
            <p className="k-text" dir="auto">
              {liveText}
              <span className="k-caret" aria-hidden="true" />
            </p>
          ) : null}
        </article>
      ) : null}
        </div>
      </div>
    </div>
  );
}

