// S09 Resume & fork picker.
//
// "Open a conversation" must open month-old transcripts as readily as live
// ones (recon, F03): the engine resumes from a session id or an absolute
// .jsonl path, and this screen lists the files on disk so a session the index
// never saw is still reachable.

import { useEffect, useState } from "react";
import { api, type SessionRow, type TranscriptRow } from "../lib/engine";
import { Badge, Button, EmptyState, Mono } from "./primitives";
import { sessionTitle, statusLabel } from "../lib/labels";
import { useStore } from "../lib/store";

export function ResumePicker() {
  const project = useStore((s) => s.activeProject);
  const sessions = useStore((s) => s.sessions);
  const [files, setFiles] = useState<TranscriptRow[]>([]);
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState<string>();
  const [tab, setTab] = useState<"index" | "files">("index");

  useEffect(() => {
    let alive = true;
    async function load() {
      if (!project) {
        setFiles([]);
        return;
      }
      try {
        const next = await api.listTranscripts(project.id);
        if (alive) {
          setFiles(next);
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
  }, [project?.id]);

  async function resume(target: string, fork: boolean) {
    if (!project) return;
    setBusy(target + (fork ? ":fork" : ":resume"));
    try {
      const p = await api.spawnSession({
        projectId: project.id,
        cwd: project.path,
        permissionMode: "default",
        resume: target,
        forkOf: fork ? target : undefined,
      });
      const sessions = await api.listSessions(project.id);
      const row = sessions.find((s) => s.id === p.sessionId);
      useStore.setState({ sessions, screen: "s01" });
      if (row) await useStore.getState().selectSession(row);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(undefined);
    }
  }

  if (!project) {
    return (
      <main className="k-main k-main--page k-main--center">
        <section className="k-page">
          <EmptyState title="لا مشروع مفتوح" body="استخدم زر «فتح مجلد» في الأعلى لاختيار مشروع." />
        </section>
      </main>
    );
  }

  return (
    <main className="k-main k-main--page">
      <section className="k-page">
        <header className="k-page__head">
          <h1>استئناف أو تفرّع</h1>
          <p className="k-note">
            الاستئناف يعيد فتح الجلسة نفسها على ملف السيرة ذاته. التفرّع ينسخ
            التاريخ إلى معرّف جديد ويترك الأصل كما هو.
          </p>
        </header>

        <div className="k-tabs" role="tablist" aria-label="مصدر الاستئناف">
          <button
            type="button"
            role="tab"
            aria-selected={tab === "index"}
            className={`k-tab ${tab === "index" ? "is-selected" : ""}`}
            onClick={() => setTab("index")}
          >
            من الفهرس
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={tab === "files"}
            className={`k-tab ${tab === "files" ? "is-selected" : ""}`}
            onClick={() => setTab("files")}
          >
            ملفات السيرة
          </button>
        </div>

        {error ? <div className="k-notice k-notice--error" role="alert">{error}</div> : null}

        {tab === "index" ? (
          <SessionTable sessions={sessions} busy={busy} onResume={resume} />
        ) : (
          <FileTable files={files} busy={busy} onResume={resume} />
        )}
      </section>
    </main>
  );
}

function SessionTable(props: {
  sessions: SessionRow[];
  busy?: string;
  onResume: (target: string, fork: boolean) => Promise<void>;
}) {
  const { sessions, busy, onResume } = props;
  if (sessions.length === 0) {
    return (
      <EmptyState
        title="لا جلسات في الفهرس"
        body="جرّب تبويب ملفات السيرة؛ الملف الذي كتبه المحرك موجود حتى لو لم يُفهرس بعد."
      />
    );
  }
  return (
    <table className="k-table">
      <thead>
        <tr>
          <th>العنوان</th>
          <th>النموذج</th>
          <th>الحالة</th>
          <th>الأدوار</th>
          <th>آخر تحديث</th>
          <th />
        </tr>
      </thead>
      <tbody>
        {sessions.map((s) => (
          <tr key={s.id}>
            <td>{sessionTitle(s.title)}</td>
            <td><Mono>{s.model}</Mono></td>
            <td>
              <Badge
                variant={
                  s.missingTranscript
                    ? "danger"
                    : s.status === "active"
                      ? "running"
                      : s.status === "error"
                        ? "danger"
                        : s.status === "done"
                          ? "success"
                          : "neutral"
                }
              >
                {s.missingTranscript ? "بلا ملف" : statusLabel(s.status)}
              </Badge>
            </td>
            <td>{s.turnCount}</td>
            <td><Mono>{shortTime(s.updatedAt)}</Mono></td>
            <td className="k-table__actions">
              <Button
                variant="secondary"
                size="sm"
                busy={busy === s.id + ":resume"}
                disabled={busy != null || s.missingTranscript}
                onClick={() => void onResume(s.id, false)}
              >
                استئناف
              </Button>
              <Button
                variant="ghost"
                size="sm"
                busy={busy === s.id + ":fork"}
                disabled={busy != null || s.missingTranscript}
                onClick={() => void onResume(s.id, true)}
              >
                تفرّع
              </Button>
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function FileTable(props: {
  files: TranscriptRow[];
  busy?: string;
  onResume: (target: string, fork: boolean) => Promise<void>;
}) {
  const { files, busy, onResume } = props;
  if (files.length === 0) {
    return (
      <EmptyState
        title="لا ملفات سيرة"
        body="لم يكتب المحرك أي .jsonl لهذا المشروع بعد. أرسل أول رسالة وستظهر هنا."
      />
    );
  }
  return (
    <table className="k-table">
      <thead>
        <tr>
          <th>المعرّف</th>
          <th>المسار</th>
          <th>الحجم</th>
          <th>آخر كتابة</th>
          <th />
        </tr>
      </thead>
      <tbody>
        {files.map((f) => (
          <tr key={f.path}>
            <td><Mono>{f.sessionId}</Mono></td>
            <td className="k-path"><Mono>{f.path}</Mono></td>
            <td>{fmtBytes(f.bytes)}</td>
            <td><Mono>{shortTime(f.updatedAt)}</Mono></td>
            <td className="k-table__actions">
              <Button
                variant="secondary"
                size="sm"
                busy={busy === f.sessionId + ":resume"}
                disabled={busy != null}
                onClick={() => void onResume(f.sessionId, false)}
              >
                استئناف
              </Button>
              <Button
                variant="ghost"
                size="sm"
                busy={busy === f.sessionId + ":fork"}
                disabled={busy != null}
                onClick={() => void onResume(f.sessionId, true)}
              >
                تفرّع
              </Button>
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} kB`;
  return `${(n / (1024 * 1024)).toFixed(1)} MB`;
}

function shortTime(iso: string): string {
  try {
    return new Date(iso).toLocaleString("ar", {
      dateStyle: "short",
      timeStyle: "short",
    });
  } catch {
    return iso;
  }
}
