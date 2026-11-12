import { useEffect, useRef, useState } from "react";
import { Badge, Button, Mono } from "./primitives";
import { DiffView } from "./DiffView";
import { useFocusTrap } from "../lib/focus";
import type { PermissionPrompt as Prompt } from "../lib/reducer";

function titleFor(tool: string, input: any): string {
  if (tool === "Bash") return "تشغيل أمر في الصدفة";
  if (tool === "Write") return "كتابة ملف جديد";
  if (tool === "Edit") return "تعديل ملف";
  if (tool === "Read") return "قراءة ملف";
  if (tool === "Glob") return "سرد ملفات";
  if (tool === "Grep") return "بحث في المحتوى";
  if (tool === "WebFetch") return "جلب صفحة ويب";
  if (tool === "NotebookEdit") return "تعديل دفتر Jupyter";
  if (tool.startsWith("mcp__")) return "نداء أداة MCP";
  if (input?.description) return String(input.description);
  return tool;
}

function bodyFor(input: any): string {
  if (!input) return "";
  const b = input as any;
  if (typeof b.command === "string") return b.command;
  if (typeof b.file_path === "string") return b.file_path;
  if (typeof b.url === "string") return b.url;
  if (typeof b.pattern === "string") return b.pattern;
  return JSON.stringify(input, null, 2);
}

function diffFor(tool: string, input: any): { oldText: string; newText: string } | null {
  if (!input) return null;
  if (tool === "Edit" && typeof input.old_string === "string" && typeof input.new_string === "string") {
    return { oldText: input.old_string, newText: input.new_string };
  }
  if (tool === "Write" && typeof input.content === "string") {
    return { oldText: "", newText: input.content };
  }
  return null;
}

export function PermissionDialog(props: {
  prompt: Prompt;
  queueLength: number;
  onDecide: (behavior: "allow" | "deny", extra?: Record<string, unknown>) => void;
}) {
  const { prompt, queueLength, onDecide } = props;
  const [edit, setEdit] = useState(false);
  const [draft, setDraft] = useState(() => {
    try {
      return JSON.stringify(prompt.input, null, 2);
    } catch {
      return "{}";
    }
  });
  const denyRef = useRef<HTMLButtonElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const diff = diffFor(prompt.toolName, prompt.input);
  useFocusTrap(dialogRef);

  // Focus lands on the safest control, never Allow.
  useEffect(() => {
    denyRef.current?.focus();
    setEdit(false);
  }, [prompt.requestId]);

  // Alt+A allow, Alt+D deny, Alt+E edit. No bare Y/N: too close to typing.
  useEffect(() => {
    const onKey = (ev: KeyboardEvent) => {
      if (ev.altKey && ev.key.toLowerCase() === "a") {
        ev.preventDefault();
        if (!edit) onDecide("allow");
      } else if (ev.altKey && ev.key.toLowerCase() === "d") {
        ev.preventDefault();
        onDecide("deny", { message: "denied by the user" });
      } else if (ev.altKey && ev.key.toLowerCase() === "e") {
        ev.preventDefault();
        setEdit((v) => !v);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [edit, onDecide]);

  function allowEdited() {
    try {
      const parsed = JSON.parse(draft);
      onDecide("allow", { updatedInput: parsed });
    } catch {
      setDraft(draft); // keep the invalid text for the user to fix
    }
  }

  return (
    <div className="k-scrim" role="alertdialog" aria-modal="true" aria-labelledby="perm-title">
      <div ref={dialogRef} className="k-dialog">
        <div className="k-dialog__head">
          <Badge variant="warning">يتطلّب موافقتك</Badge>
          {queueLength > 1 ? <Badge variant="info">{`${queueLength} في الانتظار`}</Badge> : null}
        </div>
        <h2 id="perm-title">{titleFor(prompt.toolName, prompt.input)}</h2>
        <div className="k-dialog__meta">
          <span>الأداة</span>
          <Mono>{prompt.toolName}</Mono>
          {prompt.permissionMode ? (
            <>
              <span>·</span>
              <span>الوضع</span>
              <Mono>{prompt.permissionMode}</Mono>
            </>
          ) : null}
        </div>

        {prompt.reason ? <p className="k-note">{prompt.reason}</p> : null}

        {edit ? (
          <textarea
            className="k-input k-input--mono"
            dir="ltr"
            rows={Math.min(14, Math.max(5, draft.split("\n").length + 1))}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            aria-label="مدخل الأداة القابل للتعديل"
          />
        ) : diff ? (
          <div dir="rtl">
            <DiffView oldText={diff.oldText} newText={diff.newText} dialog label="تغييرات الأداة" />
          </div>
        ) : (
          <pre className="k-code" dir="ltr">
            <code>{bodyFor(prompt.input)}</code>
          </pre>
        )}

        {prompt.suggestions.length > 0 ? (
          <div className="k-chiprow">
            {prompt.suggestions.map((s) => (
              <button
                key={s.rule}
                type="button"
                className="k-chip"
                dir="auto"
                onClick={() => onDecide("allow", { destination: s.destination, rule: s })}
                title={s.rule}
              >
                {s.label}
                <Mono>{s.rule}</Mono>
              </button>
            ))}
          </div>
        ) : null}

        <div className="k-dialog__actions">
          <Button variant="ghost" onClick={() => setEdit((v) => !v)}>
            {edit ? "إلغاء التعديل" : "تعديل المدخل"}
          </Button>
          <button
            ref={denyRef}
            type="button"
            className="k-btn k-btn--secondary k-btn--md"
            onClick={() => onDecide("deny", { message: "denied by the user" })}
          >
            منع
          </button>
          {edit ? (
            <Button variant="primary" onClick={allowEdited}>
              إرسال التعديل
            </Button>
          ) : (
            <Button variant="primary" onClick={() => onDecide("allow")}>
              سماح
            </Button>
          )}
        </div>
        <p className="k-kbdhint">
          <Mono>Alt+A</Mono> سماح · <Mono>Alt+D</Mono> منع · <Mono>Alt+E</Mono> تعديل
        </p>
      </div>
    </div>
  );
}
