// S13 Context & memory meter.
//
// The two honest meters: the engine's own `getContextUsage` breakdown (by
// category, skilliented and tool) and the cost rollup the host computes from
// `usage` rows. Neither is estimated in the renderer.

import { useEffect, useState } from "react";
import { api, type CostRollup } from "../lib/engine";
import { Badge, Button, EmptyState, Mono } from "./primitives";
import { formatCost } from "../lib/labels";
import { useStore } from "../lib/store";

interface ContextUsage {
  total_tokens?: number;
  context_window?: number;
  percent?: number;
  categories?: Record<string, number>;
  skills?: Record<string, number>;
  tools?: Record<string, number>;
  estimated?: boolean;
}

export function ContextMemory() {
  const session = useStore((s) => s.activeSession);
  const project = useStore((s) => s.activeProject);
  const [usage, setUsage] = useState<ContextUsage>();
  const [memory, setMemory] = useState<{ path: string; scope: string }[]>([]);
  const [cost, setCost] = useState<CostRollup>();
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);

  async function refresh() {
    if (!session || !project) return;
    setBusy(true);
    try {
      const [next, files, rollup] = await Promise.all([
        api.contextUsage(session.id) as Promise<ContextUsage>,
        api.listMemory(project.id).catch(() => []),
        api.costRollup(project.id).catch(() => undefined),
      ]);
      setUsage(next);
      setMemory(files);
      setCost(rollup);
      setError(undefined);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  useEffect(() => {
    void refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [session?.id]);

  async function compact() {
    if (!session) return;
    setBusy(true);
    try {
      await api.compactSession(session.id);
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  const pct = usage?.percent ?? 0;
  const sessionMicros =
    cost?.sessions.find((row) => row.sessionId === session?.id)?.costMicros ??
    session?.costMicros ??
    0;

  return (
    <main className="k-main k-main--page">
      <section className="k-page">
        <header className="k-page__head">
          <h1>السياق والذاكرة</h1>
          <p className="k-note">
            نسبةً إلى نافذة النموذج. المصدر هو المحرك (<Mono>getContextUsage</Mono>)،
            لا عدّ محلي في الواجهة.
          </p>
        </header>

        {error ? <div className="k-notice k-notice--error" role="alert">{error}</div> : null}

        {!session ? (
          <EmptyState title="لا جلسة مفتوحة" body="افتح جلسة لعرض استهلاك نافذة السياق." />
        ) : (
          <>
            <section className="k-meter-block" aria-label="استهلاك السياق">
              {usage ? (
                <div
                  className="k-meter"
                  role="meter"
                  aria-valuemin={0}
                  aria-valuemax={usage.context_window ?? 1}
                  aria-valuenow={usage.total_tokens ?? 0}
                  aria-valuetext={`${pct}% من النافذة، ${fmt(usage.total_tokens)} من ${fmt(usage.context_window)} رمز`}
                  aria-label="استهلاك السياق"
                >
                  <span className={pct >= 80 ? "k-meter__fill k-meter__fill--near" : "k-meter__fill"} style={{ inlineSize: `${Math.min(100, pct)}%` }} />
                </div>
              ) : (
                <div className="k-meter" aria-label="استهلاك السياق غير متاح">
                  <span className="k-meter__fill" style={{ inlineSize: "0%" }} />
                </div>
              )}
              <p className="k-meta">
                <Mono>{`${pct}% · ${fmt(usage?.total_tokens)} / ${fmt(usage?.context_window)} رمز`}</Mono>
                {pct >= 80 ? <Badge variant="warning">قرب الحد</Badge> : null}
                {usage?.estimated ? <Badge variant="neutral">تقديري</Badge> : null}
              </p>
              <Button variant="secondary" size="sm" busy={busy} onClick={() => void refresh()}>
                تحديث
              </Button>
              <Button variant="ghost" size="sm" busy={busy} onClick={() => void compact()}>
                ضغط السياق
              </Button>
            </section>

            <section className="k-card">
              <header className="k-card__head">
                <h2>التكلفة</h2>
              </header>
              <table className="k-table k-table--compact">
                <tbody>
                  <tr>
                    <th scope="row">هذه الجلسة</th>
                    <td className="k-num"><Mono>{formatCost(sessionMicros)}</Mono></td>
                  </tr>
                  <tr>
                    <th scope="row">المشروع</th>
                    <td className="k-num"><Mono>{formatCost(cost?.projectMicros ?? 0)}</Mono></td>
                  </tr>
                </tbody>
              </table>
              <p className="k-note">تُحسب من صفوف الاستخدام في الفهرس، لا من جمع عائم في الواجهة.</p>
            </section>

            <h2 className="k-section-title">حسب الفئة</h2>
            <BreakdownTable rows={usage?.categories} />

            <h2 className="k-section-title">حسب المهارة</h2>
            <BreakdownTable rows={usage?.skills} />

            <h2 className="k-section-title">حسب الأداة</h2>
            <BreakdownTable rows={usage?.tools} />

            <h2 className="k-section-title">ملفات الذاكرة</h2>
            {memory.length === 0 ? (
              <p className="k-note">لا ملفات CLAUDE.md أو KIRI.md أو AGENTS.md مكتشفة لهذا المشروع.</p>
            ) : (
              <ul className="k-filelist" role="list">
                {memory.map((file) => (
                  <li key={file.path}>
                    <Mono>{file.path}</Mono>
                    <Badge variant="neutral">{file.scope === "user" ? "مستخدم" : "مشروع"}</Badge>
                  </li>
                ))}
              </ul>
            )}
          </>
        )}
      </section>
    </main>
  );
}

function BreakdownTable({ rows }: { rows?: Record<string, number> }) {
  const entries = Object.entries(rows ?? {}).sort((a, b) => b[1] - a[1]);
  if (entries.length === 0) {
    return <p className="k-note">لا بيانات بعد.</p>;
  }
  const max = Math.max(...entries.map((e) => e[1]), 1);
  return (
    <table className="k-table k-table--compact">
      <tbody>
        {entries.map(([k, v]) => (
          <tr key={k}>
            <td className="k-mono" dir="ltr">{k}</td>
            <td className="k-barcell">
              <span className="k-bar" style={{ inlineSize: `${(v / max) * 100}%` }} />
            </td>
            <td className="k-num">{v.toLocaleString("ar")}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function fmt(n?: number): string {
  if (!n) return "—";
  if (n >= 1_000_000) return (n / 1_000_000).toFixed(1) + "M";
  if (n >= 1000) return (n / 1000).toFixed(n >= 10_000 ? 0 : 1) + "k";
  return String(n);
}
