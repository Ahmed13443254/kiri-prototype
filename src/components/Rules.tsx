// S06 Rules editor: the allow / deny / ask rules the permission pipeline
// evaluates before it ever asks the user for a call.
//
// The rule spelling is the engine's own: `Bash(npm test:*)`, `Read(*)`,
// `Edit`. This screen writes rows the host stores; it does not invent a second
// matcher. A rule added here is visible to the very next tool call.

import { useEffect, useState } from "react";
import { api, type RuleRow } from "../lib/engine";
import { useStore } from "../lib/store";
import { Badge, Button, EmptyState, Mono } from "./primitives";
import { originLabel } from "../lib/labels";

const SCOPES = ["session", "local", "project", "user", "managed"] as const;

export function Rules() {
  const project = useStore((s) => s.activeProject);
  const [rows, setRows] = useState<RuleRow[]>([]);
  const [error, setError] = useState<string>();
  const [ruleType, setRuleType] = useState<"allow" | "deny" | "ask">("allow");
  const [tool, setTool] = useState("Bash");
  const [pattern, setPattern] = useState("");
  const [scope, setScope] = useState<string>("local");
  const [busy, setBusy] = useState(false);

  async function reload() {
    if (!project) return;
    try {
      setRows(await api.listRules(project.id));
      setError(undefined);
    } catch (e) {
      setError(String(e));
    }
  }

  useEffect(() => {
    void reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [project?.id]);

  async function add() {
    if (!project) return;
    const toolName = tool.trim();
    if (!toolName) {
      setError("اسم الأداة مطلوب.");
      return;
    }
    setBusy(true);
    try {
      await api.upsertRule({
        projectId: project.id,
        ruleType,
        tool: toolName,
        pattern: pattern.trim() ? pattern.trim() : undefined,
        scopeName: scope,
        origin: "user",
        enabled: true,
      });
      await reload();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function toggle(r: RuleRow) {
    if (!project) return;
    try {
      await api.upsertRule({
        projectId: project.id,
        ruleType: r.ruleType,
        tool: r.tool,
        pattern: r.pattern ?? undefined,
        scopeName: r.scope,
        origin: r.origin,
        enabled: !r.enabled,
      });
      await reload();
    } catch (e) {
      setError(String(e));
    }
  }

  async function remove(r: RuleRow) {
    if (!project) return;
    try {
      await api.deleteRule(project.id, r.id);
      await reload();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <main className="k-main k-main--page">
      <section className="k-page">
        <header className="k-page__head">
          <h1>قواعد الصلاحيات</h1>
          <p className="k-note">
            <Mono>Deny</Mono> يفوز على <Mono>hook</Mono> وعلى <Mono>allow</Mono>. و
            <Mono>Ask</Mono> يرفع الطلب إلى الواجهة حتى في <Mono>bypass</Mono>.
          </p>
        </header>

        {error ? (
          <div className="k-notice k-notice--error" role="alert">
            {error}
          </div>
        ) : null}

        <div className="k-formrow">
          <label className="k-field">
            <span className="k-label">النوع</span>
            <select className="k-select" value={ruleType} onChange={(e) => setRuleType(e.target.value as any)}>
              <option value="allow">allow</option>
              <option value="deny">deny</option>
              <option value="ask">ask</option>
            </select>
          </label>
          <label className="k-field">
            <span className="k-label">الأداة</span>
            <input className="k-input" dir="ltr" value={tool} onChange={(e) => setTool(e.target.value)} placeholder="Bash" />
          </label>
          <label className="k-field k-field--grow">
            <span className="k-label">النمط (مثال: npm test* أو *)</span>
            <input className="k-input k-input--mono" dir="ltr" value={pattern} onChange={(e) => setPattern(e.target.value)} placeholder="npm test*" />
          </label>
          <label className="k-field">
            <span className="k-label">النطاق</span>
            <select className="k-select" value={scope} onChange={(e) => setScope(e.target.value)}>
              {SCOPES.map((s) => (
                <option key={s} value={s}>{originLabel(s)}</option>
              ))}
            </select>
          </label>
          <Button variant="primary" busy={busy} onClick={() => void add()} disabled={!project || busy}>
            إضافة قاعدة
          </Button>
        </div>

        {rows.length === 0 ? (
          <EmptyState title="لا قواعد بعد" body="استخدم «السماح دائمًا» في بطاقة الموافقة، أو أضف قاعدة هنا بكتابة الأداة والنمط." />
        ) : (
          <table className="k-table">
            <thead>
              <tr>
                <th>النوع</th>
                <th>الأداة</th>
                <th>النمط</th>
                <th>النطاق</th>
                <th>المصدر</th>
                <th>الحالة</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {rows.map((r) => (
                <tr key={r.id}>
                  <td><Badge variant={r.ruleType === "allow" ? "success" : r.ruleType === "deny" ? "danger" : "warning"}>{r.ruleType}</Badge></td>
                  <td><Mono>{r.tool}</Mono></td>
                  <td><Mono>{r.pattern ?? "*"}</Mono></td>
                  <td>{originLabel(r.scope)}</td>
                  <td><Badge variant={r.origin === "always_allow" ? "info" : "neutral"}>{originLabel(r.origin)}</Badge></td>
                  <td>{r.enabled ? "مفعّلة" : "معطّلة"}</td>
                  <td className="k-table__actions">
                    <Button variant="ghost" size="sm" onClick={() => void toggle(r)}>
                      {r.enabled ? "تعطيل" : "تفعيل"}
                    </Button>
                    <Button variant="danger-ghost" size="sm" onClick={() => void remove(r)}>
                      حذف
                    </Button>
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

