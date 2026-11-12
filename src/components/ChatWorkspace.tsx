import { useEffect, useRef, useState } from "react";
import { useStore } from "../lib/store";
import { api, type SearchHit, type SessionRow } from "../lib/engine";
import { Badge, Button, IconButton, Mono } from "./primitives";
import { PromptComposer } from "./PromptComposer";
import { Transcript } from "./Transcript";
import { PermissionDialog } from "./PermissionDialog";
import { QuestionCard } from "./QuestionCard";
import { useFocusTrap } from "../lib/focus";
import { formatCost, sessionTitle, statusLabel } from "../lib/labels";
import type { Question } from "./QuestionCard";

function formatRelative(value: string): string {
  const elapsed = Date.now() - new Date(value).getTime();
  if (!Number.isFinite(elapsed) || elapsed < 60_000) return "الآن";
  const units: [Intl.RelativeTimeFormatUnit, number][] = [
    ["year", 31_536_000_000], ["month", 2_592_000_000], ["day", 86_400_000],
    ["hour", 3_600_000], ["minute", 60_000],
  ];
  const [unit, ms] = units.find(([, size]) => elapsed >= size) ?? ["minute", 60_000];
  return new Intl.RelativeTimeFormat("ar", { numeric: "auto" }).format(-Math.round(elapsed / ms), unit);
}

function sessionStatus(row: SessionRow): string {
  return row.missingTranscript ? "بلا ملف" : statusLabel(row.status);
}

function sessionBadge(row: SessionRow): string {
  if (row.missingTranscript) return "warning";
  if (row.status === "active") return "running";
  if (row.status === "done") return "success";
  if (row.status === "error") return "danger";
  return "neutral";
}

function DeleteSessionDialog(props: { session: SessionRow; onCancel: () => void; onConfirm: () => void }) {
  const [draft, setDraft] = useState("");
  const cancelRef = useRef<HTMLButtonElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const title = sessionTitle(props.session.title);
  useFocusTrap(dialogRef);
  useEffect(() => cancelRef.current?.focus(), []);

  return (
    <div className="k-scrim" onKeyDown={(event) => { if (event.key === "Escape") props.onCancel(); }}>
      <div ref={dialogRef} className="k-dialog" role="dialog" aria-modal="true" aria-labelledby="delete-session-title">
        <h2 id="delete-session-title">حذف الجلسة</h2>
        <p>اكتب اسم الجلسة «{title}» لتأكيد الحذف. لا يمكن التراجع عن هذا الإجراء.</p>
        <label className="k-field"><span className="k-label">اسم الجلسة</span>
          <input className="k-input" value={draft} onChange={(event) => setDraft(event.target.value)} autoComplete="off" />
        </label>
        <div className="k-dialog__actions">
          <button ref={cancelRef} type="button" className="k-btn k-btn--secondary k-btn--md" onClick={props.onCancel}>إلغاء</button>
          <Button variant="danger" disabled={draft !== title} onClick={props.onConfirm}>حذف نهائي</Button>
        </div>
      </div>
    </div>
  );
}

export function ChatWorkspace({ onOpenNavigation }: { onOpenNavigation: () => void }) {
  const s = useStore();
  const [deleteTarget, setDeleteTarget] = useState<SessionRow>();
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[]>([]);
  const [searched, setSearched] = useState(false);
  const [sessionsOpen, setSessionsOpen] = useState(false);
  const [context, setContext] = useState<{ total: number; window: number } | null>(null);

  async function search() {
    if (!s.activeProject || !query.trim()) return;
    try {
      setHits(await api.searchBlocks(s.activeProject.id, query.trim()));
      setSearched(true);
    } catch (e) {
      setHits([]);
      setSearched(true);
      useStore.setState({ error: String(e) });
    }
  }

  const pendingPrompt = s.stream.prompts.find((p) => p.status === "pending");
  const pendingQuestion = s.stream.questions.find((q) => q.status === "pending");
  const lastUserEntry = [...s.stream.entries].reverse().find((entry) => entry.kind === "user");
  const lastUserText = lastUserEntry?.kind === "user" ? lastUserEntry.text : "";
  const sessionId = s.activeSession?.id;

  useEffect(() => {
    if (!sessionId) { setContext(null); return; }
    let alive = true;
    api.contextUsage(sessionId).then((value) => {
      if (!alive) return;
      const any = value as any;
      const total = Number(any?.total_tokens);
      const window = Number(any?.context_window);
      setContext(Number.isFinite(total) && Number.isFinite(window) && window > 0 ? { total, window } : null);
    }).catch(() => { if (alive) setContext(null); });
    return () => { alive = false; };
  }, [sessionId, s.stream.costMicros, s.stream.status]);

  function selectSession(row: SessionRow) {
    void s.selectSession(row);
    setSessionsOpen(false);
  }

  return (
    <>
      <div className="k-shell">
        {sessionsOpen ? <button className="k-sessions-scrim" aria-label="إغلاق الجلسات" onClick={() => setSessionsOpen(false)} /> : null}
        <aside className={`k-sidebar ${sessionsOpen ? "is-open" : ""}`} aria-label="المشاريع والجلسات">
          <div className="k-sidebar__head">
            <div><h1>المحادثات</h1><p>{s.sessions.length} جلسة في هذا المشروع</p></div>
            <Button variant="primary" size="sm" onClick={() => void s.newSession()}>+ جلسة جديدة</Button>
          </div>

          <label className="k-project-select">
            <span className="k-sr-only">المشروع</span>
            <select
              value={s.activeProject?.id ?? ""}
              onChange={(event) => {
                const project = s.projects.find((item) => item.id === event.target.value);
                if (project) void s.selectProject(project);
              }}
              disabled={s.projects.length === 0}
            >
              {s.projects.length === 0 ? <option value="">لا يوجد مشروع مفتوح</option> : null}
              {s.projects.map((project) => <option key={project.id} value={project.id}>{project.name}</option>)}
            </select>
          </label>

          <div className="k-session-search">
            <svg viewBox="0 0 24 24" fill="none" aria-hidden="true"><circle cx="11" cy="11" r="7" stroke="currentColor" strokeWidth="1.6" /><path d="m16 16 4 4" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" /></svg>
            <input
              value={query}
              onChange={(event) => { setQuery(event.target.value); setHits([]); setSearched(false); }}
              onKeyDown={(event) => { if (event.key === "Enter") void search(); }}
              placeholder="ابحث في المحادثات"
              aria-label="بحث في المحادثات"
            />
          </div>

          <div className="k-sidebar__sessions">
            <div className="k-sidebar__caption"><span>{hits.length > 0 || searched ? "نتائج البحث" : "الأخيرة"}</span>{s.loading ? <span>جارٍ التحميل…</span> : null}</div>
            {hits.length === 0 && searched ? <p className="k-note">لا توجد نتائج مطابقة.</p> : null}
            {hits.length > 0 ? (
              <ul className="k-slist" role="list">
                {hits.map((hit) => (
                  <li key={`${hit.messageId}-${hit.blockId}`}>
                    <button type="button" className="k-srow" onClick={() => {
                      const row = s.sessions.find((session) => session.id === hit.sessionId);
                      if (row) selectSession(row);
                    }}>
                      <span className="k-srow__t">{hit.sessionTitle ?? hit.sessionId}</span>
                      <span className="k-srow__snippet">{hit.snippet}</span>
                    </button>
                  </li>
                ))}
              </ul>
            ) : null}

            {!searched && s.sessions.length === 0 ? (
              <div className="k-empty k-empty--compact"><h2>ابدأ أول محادثة</h2><p>أنشئ جلسة جديدة، وستجدها محفوظة هنا.</p></div>
            ) : null}
            {!searched && s.sessions.length > 0 ? (
              <ul className="k-slist" role="list">
                {s.sessions.map((row) => (
                  <li key={row.id} className={`k-srow-item ${s.activeSession?.id === row.id ? "is-active" : ""}`}>
                    <a
                      href={`#session-${row.id}`}
                      className={`k-srow ${s.activeSession?.id === row.id ? "is-active" : ""}`}
                      aria-current={s.activeSession?.id === row.id ? "page" : undefined}
                      onClick={(event) => { event.preventDefault(); selectSession(row); }}
                    >
                      <span className="k-srow__top"><span className="k-srow__t">{sessionTitle(row.title)}</span><span className="k-srow__time">{formatRelative(row.updatedAt)}</span></span>
                      <span className="k-srow__m">
                        <Badge variant={sessionBadge(row)}>{row.status === "active" ? <span className="k-dot" aria-hidden="true" /> : null}{sessionStatus(row)}</Badge>
                        <span>{row.turnCount} أدوار</span><Mono>{formatCost(row.costMicros)}</Mono>
                      </span>
                      {row.forkOf ? <span className="k-note">جلسة متفرعة</span> : null}
                      {row.missingTranscript ? <span className="k-cut">ملف المحادثة مفقود</span> : null}
                    </a>
                    <IconButton label={`حذف ${sessionTitle(row.title)}`} onClick={() => setDeleteTarget(row)}>×</IconButton>
                  </li>
                ))}
              </ul>
            ) : null}
          </div>
        </aside>

        <main className={`k-main ${s.error ? "k-main--with-notice" : ""}`}>
          <div className="k-mobilebar">
            <IconButton label="فتح التنقل" onClick={onOpenNavigation}>☰</IconButton>
            <strong>{s.activeSession ? sessionTitle(s.activeSession.title) : "محادثة جديدة"}</strong>
            <Button variant="ghost" size="sm" onClick={() => setSessionsOpen(true)}>الجلسات</Button>
          </div>
          {s.error ? <div className="k-notice k-notice--error" role="alert">{s.error}</div> : null}
          <Transcript
            entries={s.stream.entries}
            live={s.stream.live}
            liveThinking={s.stream.liveThinking}
            busy={s.stream.status === "running"}
            projectId={s.activeProject?.id}
            sessionId={s.activeSession?.id}
            title={s.activeSession ? sessionTitle(s.activeSession.title) : "محادثة جديدة"}
            canRetry={Boolean(lastUserText)}
            onRetry={() => { if (lastUserText) void s.send(lastUserText); }}
          />
          <PromptComposer
            busy={s.stream.status === "running"}
            model={s.stream.model || "claude-opus-5-5"}
            effort={s.stream.effort || "high"}
            mode={s.stream.permissionMode || "default"}
            contextUsed={context?.total}
            contextWindow={context?.window}
            costMicros={s.stream.costMicros}
            onSend={(text) => void s.send(text)}
            onStop={() => void s.interrupt()}
            onMode={(mode) => void s.setMode(mode)}
            onModel={(model, effort) => void s.setModel(model, effort)}
          />
        </main>
      </div>

      {pendingPrompt ? <PermissionDialog prompt={pendingPrompt} queueLength={s.stream.prompts.filter((p) => p.status === "pending").length} onDecide={(behavior, extra) => void s.decide(pendingPrompt.requestId, behavior, extra)} /> : null}
      {pendingQuestion ? <QuestionCard requestId={pendingQuestion.requestId} questions={pendingQuestion.questions as Question[]} onAnswer={(answers, response) => void s.answerQuestion(pendingQuestion.requestId, answers, response)} /> : null}
      {deleteTarget ? <DeleteSessionDialog session={deleteTarget} onCancel={() => setDeleteTarget(undefined)} onConfirm={() => { void s.deleteSession(deleteTarget); setDeleteTarget(undefined); }} /> : null}
    </>
  );
}
