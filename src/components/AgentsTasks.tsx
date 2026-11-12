// S10 Agents, background tasks & MCP.
//
// Recon F05: the task panel is fed by the host's task registry, never by "assume
// there is one"; an empty list still renders, and a resumed session still lists
// its tasks. Every row here has a stop control with a real target.

import { useEffect, useState } from "react";
import { api, type SubagentRow, type TaskRow } from "../lib/engine";
import { Badge, Button, EmptyState, Mono } from "./primitives";
import { statusLabel } from "../lib/labels";
import { useStore } from "../lib/store";

interface McpServerRow {
  name?: string;
  transport?: string;
  status?: string;
  last_error?: string | null;
  tools?: { name?: string; description?: string }[];
}

export function AgentsTasks() {
  const session = useStore((s) => s.activeSession);
  const stream = useStore((s) => s.stream);
  const [tasks, setTasks] = useState<TaskRow[]>([]);
  const [agents, setAgents] = useState<SubagentRow[]>([]);
  const [error, setError] = useState<string>();
  const [stopping, setStopping] = useState<string>();

  useEffect(() => {
    let alive = true;
    async function load() {
      if (!session) {
        setTasks([]);
        setAgents([]);
        return;
      }
      try {
        const [t, a] = await Promise.all([
          api.listTasks(session.id),
          api.listSubagents(session.id),
        ]);
        if (alive) {
          setTasks(t);
          setAgents(a);
          setError(undefined);
        }
      } catch (e) {
        if (alive) setError(String(e));
      }
    }
    void load();
    const timer = window.setInterval(() => void load(), 3000);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, [session?.id]);

  async function stop(id: string) {
    if (!session) return;
    setStopping(id);
    try {
      await api.stopTask(session.id, id);
    } catch (e) {
      setError(String(e));
    } finally {
      setStopping(undefined);
    }
  }

  if (!session) {
    return (
      <main className="k-main k-main--page k-main--center">
        <section className="k-page">
          <EmptyState title="لا جلسة مفتوحة" body="اختر جلسة من الشاشة الأولى لعرض مهامها الخلفية ووكلائها وخوادم MCP." />
        </section>
      </main>
    );
  }

  const init: any = stream.init ?? {};
  const mcpServers: McpServerRow[] = Array.isArray(init.mcp_servers) ? init.mcp_servers : [];

  return (
    <main className="k-main k-main--page">
      <section className="k-page">
        <header className="k-page__head">
          <h1>المهام والوكلاء و MCP</h1>
          <p className="k-note">
            الجدول يسرد ما يعمل الآن في هذه الجلسة. القوائم الفارغة تُعرض كما هي
            — لا لوحة تختفي عند غياب البيانات.
          </p>
        </header>

        {error ? <div className="k-notice k-notice--error" role="alert">{error}</div> : null}

        <h2 className="k-section-title">المهام الخلفية</h2>
        {tasks.length === 0 ? (
          <EmptyState title="لا مهام خلفية" body="عندما يشغّل المحرك أمر Bash في الخلفية أو مراقبًا، يظهر هنا مع زر إيقاف." />
        ) : (
          <table className="k-table">
            <thead>
              <tr>
                <th>الوصف</th>
                <th>النوع</th>
                <th>الحالة</th>
                <th>بدأت</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {tasks.map((t) => (
                <tr key={t.id}>
                  <td>{t.subject}</td>
                  <td><Badge variant="neutral"><Mono>{t.kind}</Mono></Badge></td>
                  <td><Badge variant={badgeFor(t.status)}>{statusLabel(t.status)}</Badge></td>
                  <td><Mono>{shortTime(t.createdAt)}</Mono></td>
                  <td className="k-table__actions">
                    <Button
                      variant="danger-ghost"
                      size="sm"
                      busy={stopping === t.id}
                      disabled={stopping != null || isTerminal(t.status)}
                      onClick={() => void stop(t.id)}
                    >
                      إيقاف
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}

        <h2 className="k-section-title">الوكلاء</h2>
        {agents.length === 0 ? (
          <EmptyState title="لا وكلاء" body="حين يفوّض المحرك مهمة لأجنت فرعي، يظهر هنا مع تقريره عند الانتهاء." />
        ) : (
          <ul className="k-agents" role="list">
            {agents.map((a) => (
              <li key={a.id} className="k-agent">
                <header className="k-agent__head">
                  <Mono>{a.agentName ?? a.agentType}</Mono>
                  <Badge variant={badgeFor(a.status)}>{statusLabel(a.status)}</Badge>
                  {a.isolation ? <Badge variant="info">{a.isolation}</Badge> : null}
                </header>
                <p className="k-agent__meta">
                  <Mono>{a.model ?? "—"}</Mono> · <Mono>{fmtCost(a.costMicros)}</Mono> ·{" "}
                  <Mono>{shortTime(a.createdAt)}</Mono>
                </p>
                {a.result ? <pre className="k-agent__result" dir="auto">{a.result.slice(0, 1200)}</pre> : null}
              </li>
            ))}
          </ul>
        )}

        <h2 className="k-section-title">خوادم MCP</h2>
        {mcpServers.length === 0 ? (
          <EmptyState
            title="لا خوادم MCP"
            body="تظهر هنا الخوادم التي حمّلها المحرك عند بدء الجلسة، مع أدواتها وحالتها."
          />
        ) : (
          <table className="k-table">
            <thead>
              <tr>
                <th>الخادم</th>
                <th>النقل</th>
                <th>الحالة</th>
                <th>الأدوات</th>
                <th>آخر خطأ</th>
              </tr>
            </thead>
            <tbody>
              {mcpServers.map((server, index) => (
                <tr key={`${server.name ?? "server"}-${index}`}>
                  <td><Mono>{server.name ?? "—"}</Mono></td>
                  <td><Badge variant="neutral"><Mono>{server.transport ?? "—"}</Mono></Badge></td>
                  <td>
                    <Badge variant={mcpBadge(server.status)}>
                      {statusLabel(server.status ?? "disconnected")}
                    </Badge>
                  </td>
                  <td>
                    {server.tools?.length ? (
                      <details className="k-thinking">
                        <summary>{server.tools.length} أداة</summary>
                        <ul className="k-filelist">
                          {server.tools.map((tool) => (
                            <li key={tool.name}><Mono>{tool.name ?? "—"}</Mono></li>
                          ))}
                        </ul>
                      </details>
                    ) : (
                      <span className="k-note">—</span>
                    )}
                  </td>
                  <td className="k-path">
                    {server.last_error ? <Mono>{server.last_error}</Mono> : <span className="k-note">—</span>}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
    </main>
  );
}

function mcpBadge(status?: string): string {
  switch (status) {
    case "connected":
      return "success";
    case "error":
      return "danger";
    case "connecting":
      return "running";
    default:
      return "neutral";
  }
}

function isTerminal(s: string): boolean {
  return s === "completed" || s === "failed" || s === "stopped" || s === "deleted";
}

function badgeFor(s: string): string {
  switch (s) {
    case "running":
    case "in_progress":
      return "running";
    case "failed":
    case "error":
    case "stopped":
      return "danger";
    case "completed":
    case "done":
      return "success";
    default:
      return "neutral";
  }
}

function fmtCost(micros: number): string {
  return "$" + (micros / 1_000_000).toFixed(micros < 1_000_000 ? 4 : 2);
}

function shortTime(iso: string): string {
  try {
    return new Date(iso).toLocaleTimeString("ar", { timeStyle: "short" });
  } catch {
    return iso;
  }
}
