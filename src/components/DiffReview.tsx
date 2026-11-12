// S08 Diff review: what a turn changed on disk, with per-file restore.
//
// The engine records the pre-edit body by content hash before a file tool runs
// (recordCheckpoint). This screen lists those checkpoints and can put a file
// back the way it was before the turn -- a real restore through the host, not
// a re-rendered view of the diff.

import { useEffect, useRef, useState } from "react";
import { api, type CheckpointRow } from "../lib/engine";
import { Badge, Button, EmptyState, Mono } from "./primitives";
import { DiffView } from "./DiffView";
import { useFocusTrap } from "../lib/focus";
import { useStore } from "../lib/store";

type PendingRestore =
  | { kind: "turn"; turnId: number; files: number }
  | { kind: "file"; checkpointId: string; path: string };

function RestoreDialog(props: { pending: PendingRestore; onCancel: () => void; onConfirm: () => void }) {
  const dialog = useRef<HTMLDivElement>(null);
  const cancelRef = useRef<HTMLButtonElement>(null);
  useFocusTrap(dialog);

  useEffect(() => {
    cancelRef.current?.focus();
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") props.onCancel();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [props.onCancel]);

  return (
    <div className="k-scrim">
      <div ref={dialog} className="k-dialog" role="alertdialog" aria-modal="true" aria-labelledby="restore-title">
        <h2 id="restore-title">تأكيد الاسترجاع</h2>
        <p>
          {props.pending.kind === "turn"
            ? `سيُعاد ${props.pending.files} ملف إلى ما قبل الدور ${props.pending.turnId}. أي تعديلات لاحقة على هذه الملفات ستُفقد.`
            : `سيُعاد الملف ${props.pending.path} إلى ما قبل التعديل. أي تعديلات لاحقة عليه ستُفقد.`}
        </p>
        <div className="k-dialog__actions">
          <button ref={cancelRef} type="button" className="k-btn k-btn--secondary k-btn--md" onClick={props.onCancel}>
            إلغاء
          </button>
          <Button variant="danger" onClick={props.onConfirm}>تأكيد الاسترجاع</Button>
        </div>
      </div>
    </div>
  );
}

export function DiffReview() {
  const project = useStore((s) => s.activeProject);
  const session = useStore((s) => s.activeSession);
  const [rows, setRows] = useState<CheckpointRow[]>([]);
  const [error, setError] = useState<string>();
  const [report, setReport] = useState<string>();
  const [busy, setBusy] = useState<number | null>(null);
  const [busyFile, setBusyFile] = useState<string>();
  const [pending, setPending] = useState<PendingRestore>();

  async function restoreFile(checkpointId: string) {
    if (!project || !session) return;
    setBusyFile(checkpointId);
    try {
      const r = await api.restoreCheckpointFile(project.id, session.id, checkpointId);
      setReport(r.deleted ? `حُذف ${r.path}` : `أُعيد ${r.path}`);
      setError(undefined);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyFile(undefined);
    }
  }

  useEffect(() => {
    let alive = true;
    async function load() {
      if (!project || !session) {
        setRows([]);
        return;
      }
      try {
        const next = await api.listCheckpoints(project.id, session.id);
        if (alive) {
          setRows(next);
          setError(undefined);
        }
      } catch (e) {
        if (alive) setError(String(e));
      }
    }
    void load();
    return () => {
      alive = false;
    };
  }, [project?.id, session?.id]);

  async function restore(turnId: number) {
    if (!project || !session) return;
    setBusy(turnId);
    try {
      const r = await api.restoreCheckpoint(project.id, session.id, turnId);
      setReport(
        `أُعيد ${r.written.length} ملف` +
          (r.deleted.length ? ` · حُذف ${r.deleted.length} ملف أُنشئ بعد الدور` : ""),
      );
      setError(undefined);
    } catch (e) {
      setError(String(e));
      setReport(undefined);
    } finally {
      setBusy(null);
    }
  }

  const turns = [...new Set(rows.map((r) => r.turnId ?? 0))].sort((a, b) => b - a);

  return (
    <main className="k-main k-main--page">
      <section className="k-page">
        <header className="k-page__head">
          <h1>الفروقات والاستعادة</h1>
          <p className="k-note">
            نقطة استرجاع لكل دور عدّل ملفًا. المحتوى مخزّن بالبصمة (<Mono>SHA-256</Mono>)،
            فالملف المتطابق لا يُنسخ مرتين.
          </p>
        </header>

        {error ? <div className="k-notice k-notice--error" role="alert">{error}</div> : null}
        {report ? <div className="k-notice k-notice--info" role="status">{report}</div> : null}

        {!session ? (
          <EmptyState
            title="لا جلسة مفتوحة"
            body="اختر جلسة من الشاشة الأولى، ثم عُد هنا لمراجعة ما عدّله المحرك على القرص."
          />
        ) : rows.length === 0 ? (
          <EmptyState
            title="لا تعديلات بعد"
            body="كل كتابة أو تعديل يقوم به المحرك يسجّل نسخة ما قبل التعديل هنا."
          />
        ) : (
          <div className="k-turns">
            {turns.map((t) => (
              <section key={t} className="k-turngroup">
                <header className="k-turngroup__head">
                  <h2>الدور {t || "—"}</h2>
                  <Badge variant="info">{rows.filter((r) => (r.turnId ?? 0) === t).length} ملف</Badge>
                  <Button
                    variant="secondary"
                    size="sm"
                    busy={busy === t}
                    disabled={busy === t || t === 0}
                    onClick={() =>
                      setPending({
                        kind: "turn",
                        turnId: t,
                        files: rows.filter((r) => (r.turnId ?? 0) === t).length,
                      })
                    }
                  >
                    استرجاع ما قبل الدور
                  </Button>
                </header>
                <ul className="k-filelist" role="list">
                  {rows
                    .filter((r) => (r.turnId ?? 0) === t)
                    .map((r) => (
                      <li key={r.id}>
                        <Mono>{r.path}</Mono>
                        <Badge variant={r.existedPre ? "neutral" : "success"}>
                          {r.existedPre ? "معدّل" : "أُنشئ"}
                        </Badge>
                        <span className="k-meta k-mono">{r.blobSha?.slice(0, 12) ?? "—"}</span>
                        {project && session ? (
                          <>
                            <CheckpointDiff projectId={project.id} sessionId={session.id} checkpointId={r.id} />
                            <Button
                              variant="ghost"
                              size="sm"
                              busy={busyFile === r.id}
                              disabled={busyFile != null}
                              onClick={() => setPending({ kind: "file", checkpointId: r.id, path: r.path })}
                            >
                              استرجاع الملف
                            </Button>
                          </>
                        ) : null}
                      </li>
                    ))}
                </ul>
              </section>
            ))}
          </div>
        )}
      </section>

      {pending ? (
        <RestoreDialog
          pending={pending}
          onCancel={() => setPending(undefined)}
          onConfirm={() => {
            const target = pending;
            setPending(undefined);
            if (target.kind === "turn") void restore(target.turnId);
            else void restoreFile(target.checkpointId);
          }}
        />
      ) : null}
    </main>
  );
}

function CheckpointDiff(props: { projectId: string; sessionId: string; checkpointId: string }) {
  const [diff, setDiff] = useState<{ path?: string; oldText?: string | null; newText?: string | null }>();
  const [error, setError] = useState<string>();

  async function load() {
    try {
      setDiff(await api.checkpointDiff(props.projectId, props.sessionId, props.checkpointId));
      setError(undefined);
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <details
      className="k-thinking"
      onToggle={(event) => {
        if (event.currentTarget.open && !diff) void load();
      }}
    >
      <summary>عرض الفرق</summary>
      {error ? <p className="k-note">{error}</p> : null}
      {diff ? (
        <div dir="rtl">
          <DiffView
            oldText={diff.oldText ?? ""}
            newText={diff.newText ?? ""}
            label={`فرق ${diff.path ?? ""}`}
          />
        </div>
      ) : null}
    </details>
  );
}
