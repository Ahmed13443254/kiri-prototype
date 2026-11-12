import { useEffect, useRef, useState } from "react";
import { Button, IconButton, Mono } from "./primitives";
import { ModelSelect } from "./ModelSelect";

export function PromptComposer(props: {
  busy: boolean;
  model: string;
  effort: string;
  mode: string;
  contextUsed?: number;
  contextWindow?: number;
  costMicros: number;
  onSend: (text: string) => void;
  onStop: () => void;
  onMode: (mode: string) => void;
  onModel: (model: string, effort: string) => void;
}) {
  const [text, setText] = useState("");
  const [attachments, setAttachments] = useState<string[]>([]);
  const [error, setError] = useState("");
  const [modelChangedMidTurn, setModelChangedMidTurn] = useState(false);
  const ta = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    if (ta.current) {
      ta.current.style.height = "auto";
      ta.current.style.height = `${Math.min(180, ta.current.scrollHeight)}px`;
    }
  }, [text]);

  useEffect(() => { if (!props.busy) setModelChangedMidTurn(false); }, [props.busy]);

  function submit() {
    if (props.busy) return;
    const value = text.trim();
    if (!value && attachments.length === 0) return;
    const files = attachments.map((path) => `ملف مرفق: ${path}`).join("\n");
    props.onSend([value, files].filter(Boolean).join("\n\n"));
    setText("");
    setAttachments([]);
    setError("");
  }

  function changeModel(model: string, effort: string) {
    if (props.busy) setModelChangedMidTurn(true);
    props.onModel(model, effort);
  }

  async function pickAttachments() {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const chosen = await open({ directory: false, multiple: true, title: "أرفق ملفات بالطلب" });
      const paths = Array.isArray(chosen) ? chosen : chosen ? [chosen] : [];
      setAttachments((current) => [...new Set([...current, ...paths])]);
      setError("");
    } catch (cause) {
      setError(String(cause));
    }
  }

  const contextKnown = props.contextUsed != null && props.contextWindow != null && props.contextWindow > 0;
  const pct = contextKnown ? contextPct(props.contextUsed!, props.contextWindow!) : 0;
  const canSend = !props.busy && Boolean(text.trim() || attachments.length);

  return (
    <div className="k-composer">
      <div className="k-composer__inner">
        {attachments.length > 0 ? (
          <div className="k-attachments" aria-label="الملفات المرفقة">
            {attachments.map((path) => (
              <span className="k-attachment" key={path}><Mono>{path}</Mono><IconButton label={`إزالة ${path}`} onClick={() => setAttachments((current) => current.filter((item) => item !== path))}>×</IconButton></span>
            ))}
          </div>
        ) : null}
        {error ? <div className="k-notice k-notice--error" role="alert">{error}</div> : null}

        <div className="k-composer__box">
          <label className="k-sr-only" htmlFor="composer-input">نص الرسالة</label>
          <textarea
            id="composer-input"
            ref={ta}
            className="k-composer__input"
            dir="auto"
            rows={1}
            placeholder="اكتب طلبك هنا…"
            value={text}
            onChange={(event) => setText(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault();
                submit();
              }
            }}
          />
          <div className="k-composer__actions">
            <IconButton label="إرفاق ملفات" onClick={() => void pickAttachments()}>
              <svg width="18" height="18" viewBox="0 0 16 16" fill="none" aria-hidden="true"><path d="M10.5 4.5 5.8 9.2a1.5 1.5 0 0 0 2.1 2.1l5-5a3 3 0 0 0-4.2-4.2l-5 5a4.5 4.5 0 0 0 6.4 6.4l4.2-4.2" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" /></svg>
            </IconButton>
            {props.busy ? (
              <Button variant="danger-ghost" size="sm" onClick={props.onStop}>إيقاف الرد</Button>
            ) : (
              <Button variant="primary" onClick={submit} disabled={!canSend}>
                إرسال
                <svg width="16" height="16" viewBox="0 0 24 24" fill="none" aria-hidden="true"><path d="m5 12 14-7-4 14-3-6-7-1Z" stroke="currentColor" strokeWidth="1.7" strokeLinejoin="round" /></svg>
              </Button>
            )}
          </div>
        </div>

        <div className="k-composer__status">
          <ModelSelect value={props.model} changed={modelChangedMidTurn} onChange={(model) => changeModel(model, props.effort)} />
          <label className="k-setting-select"><span>الجهد</span>
            <select className={modelChangedMidTurn ? "is-changed" : ""} value={props.effort} onChange={(event) => changeModel(props.model, event.target.value)} aria-label="مستوى الجهد">
              <option value="low">منخفض</option><option value="medium">متوسط</option><option value="high">عالٍ</option><option value="xhigh">أعلى</option><option value="max">أقصى</option>
            </select>
          </label>
          <label className="k-setting-select"><span>الصلاحيات</span>
            <select value={props.mode} onChange={(event) => props.onMode(event.target.value)} aria-label="نمط الصلاحيات">
              <option value="default">افتراضي</option><option value="acceptEdits">قبول التعديلات</option><option value="plan">خطة</option><option value="dontAsk">بلا سؤال</option><option value="auto">تلقائي</option><option value="bypassPermissions">تجاوز</option>
            </select>
          </label>
          <span className="k-spacer" />
          <span className="k-composer__cost"><Mono>{fmtCost(props.costMicros)}</Mono></span>
        </div>

        <div className="k-composer__footer">
          <span><Mono>Enter</Mono> للإرسال · <Mono>Shift + Enter</Mono> لسطر جديد</span>
          {modelChangedMidTurn ? <span className="k-midturn">يبدأ النموذج الجديد من الرد التالي</span> : null}
          <span className="k-context">
            {contextKnown ? <><span className="k-meter" role="meter" aria-valuemin={0} aria-valuemax={props.contextWindow} aria-valuenow={props.contextUsed} aria-label="استخدام السياق"><span className={pct >= 80 ? "k-meter__fill k-meter__fill--near" : "k-meter__fill"} style={{ inlineSize: `${pct}%` }} /></span><Mono>{pct}% · {fmtTokens(props.contextUsed!)} / {fmtTokens(props.contextWindow!)}</Mono></> : "السياق —"}
          </span>
          <span className="k-sr-only" role="status" aria-live="polite">{props.busy ? "الرد قيد الإنشاء" : ""}</span>
        </div>
      </div>
    </div>
  );
}

function contextPct(used: number, window: number): number {
  if (!window) return 0;
  return Math.min(100, Math.round((used / window) * 100));
}

function fmtTokens(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(0)}k`;
  return String(n);
}

function fmtCost(micros: number): string {
  const usd = micros / 1_000_000;
  return usd < 1 ? `$${usd.toFixed(4)}` : `$${usd.toFixed(2)}`;
}
