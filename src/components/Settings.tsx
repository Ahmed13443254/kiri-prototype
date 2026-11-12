// S12 Key vault + S14 Settings & theme.
//
// The key is write-only across this boundary: the screen can set it, clear it,
// and ask whether one is present. It never receives the value back, and the
// vault backend name is whatever the OS actually gave us.

import { useEffect, useRef, useState } from "react";
import { api, type SettingRow, type VaultStatus } from "../lib/engine";
import { Badge, Button, EmptyState, IconButton, Mono } from "./primitives";
import { originLabel } from "../lib/labels";
import { useStore } from "../lib/store";

type Theme = "light" | "dark" | "system";

export function SettingsScreen() {
  const project = useStore((s) => s.activeProject);
  return (
    <main className="k-main k-main--page">
      <section className="k-page">
        <header className="k-page__head">
          <h1>الإعدادات</h1>
          <p className="k-note">مفتاح النموذج في مخزن النظام. لا يمرّ عبر الواجهة عند القراءة.</p>
        </header>
        <VaultCard />
        <ThemeCard />
{project ? <SettingsTable projectId={project.id} /> : (
          <EmptyState title="لا مشروع" body="افتح مجلدًا لعرض إعدادات المشروع المحفوظة." />
        )}
      </section>
    </main>
  );
}

function VaultCard() {
  const [status, setStatus] = useState<VaultStatus>();
  const [draft, setDraft] = useState("");
  const [error, setError] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [reveal, setReveal] = useState(false);
  const revealTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => () => {
    if (revealTimer.current) clearTimeout(revealTimer.current);
  }, []);

  function toggleReveal() {
    if (revealTimer.current) clearTimeout(revealTimer.current);
    if (reveal) {
      setReveal(false);
      return;
    }
    setReveal(true);
    revealTimer.current = setTimeout(() => {
      setReveal(false);
      revealTimer.current = null;
    }, 10_000);
  }

  async function load() {
    try {
      setStatus(await api.vaultStatus());
      setError(undefined);
    } catch (e) {
      setError(String(e));
    }
  }

  useEffect(() => {
    void load();
  }, []);

  async function save() {
    if (!draft.trim()) return;
    setBusy(true);
    try {
      await api.vaultSetKey(draft.trim());
      setDraft("");
      await load();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  async function clear() {
    setBusy(true);
    try {
      await api.vaultClear();
      await load();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="k-card k-card--wide">
      <header className="k-card__head">
        <h2>مفتاح النموذج</h2>
        {status ? (
          <Badge variant={status.present || status.hasEnvKey ? "success" : "danger"}>
            {status.present ? "محفوظ في النظام" : status.hasEnvKey ? "من البيئة" : "غائب"}
          </Badge>
        ) : null}
      </header>
      <p className="k-note">
        يخزّنها {status?.backend ?? "مخزن النظام"}. لا تُكتب في قاعدة البيانات،
        ولا في ملف إعداد، ولا في سجل.
      </p>

      <div className="k-formrow">
        <label className="k-field k-field--grow">
          <span className="k-label">مفتاح جديد (اتركه فارغًا للإلغاء)</span>
          <input
            className="k-input k-input--mono"
            dir="ltr"
            type={reveal ? "text" : "password"}
            autoComplete="off"
            spellCheck={false}
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            placeholder="sk-ant-..."
          />
        </label>
        <IconButton label={reveal ? "إخفاء المفتاح" : "إظهار المفتاح لمدة 10 ثوانٍ"} pressed={reveal} onClick={toggleReveal}>
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none" aria-hidden="true">
            <path d="M1.5 8s2.2-4 6.5-4 6.5 4 6.5 4-2.2 4-6.5 4-6.5-4-6.5-4Z" stroke="currentColor" strokeWidth="1.5" />
            <circle cx="8" cy="8" r="2" stroke="currentColor" strokeWidth="1.5" />
          </svg>
        </IconButton>
        <Button variant="primary" busy={busy} disabled={busy || !draft.trim()} onClick={() => void save()}>
          حفظ
        </Button>
        <Button variant="danger-ghost" busy={busy} disabled={busy || !status?.present} onClick={() => void clear()}>
          مسح
        </Button>
      </div>
      {error ? <div className="k-notice k-notice--error" role="alert">{error}</div> : null}
    </section>
  );
}

function ThemeCard() {
  const [theme, setTheme] = useState<Theme>(() => {
    const t = (document.documentElement.getAttribute("data-theme") ?? "light") as Theme;
    return t;
  });
  useEffect(() => {
    try {
      localStorage.setItem("kiri.theme", theme);
    } catch {
      /* storage can be unavailable; the choice still applies this session */
    }
    document.documentElement.setAttribute("data-theme", theme);
  }, [theme]);

  return (
    <section className="k-card k-card--wide">
      <header className="k-card__head">
        <h2>السمة</h2>
      </header>
      <div className="k-formrow">
        {(["light", "dark", "system"] as Theme[]).map((t) => {
          const label = t === "light" ? "فاتحة" : t === "dark" ? "داكنة" : "حسب النظام";
          return (
            <Button
              key={t}
              variant={theme === t ? "primary" : "secondary"}
              onClick={() => setTheme(t)}
              title={label}
            >
              {label}
            </Button>
          );
        })}
      </div>
    </section>
  );
}

function SettingsTable({ projectId }: { projectId: string }) {
  const [rows, setRows] = useState<SettingRow[]>([]);
  const [error, setError] = useState<string>();
  const [key, setKey] = useState("");
  const [value, setValue] = useState("");

  async function save() {
    if (!key.trim()) return;
    try {
      await api.setSetting(projectId, key.trim(), value);
      setKey("");
      setValue("");
      await load();
    } catch (e) {
      setError(String(e));
    }
  }

  async function load() {
    try {
      const next = await api.listSettings(projectId);
      setRows(next);
      setError(undefined);
    } catch (e) {
      setError(String(e));
    }
  }

  useEffect(() => {
    void load();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [projectId]);

  return (
    <section className="k-card k-card--wide">
      <header className="k-card__head">
        <h2>إعدادات المشروع</h2>
      </header>
      <div className="k-formrow">
        <label className="k-field">
          <span className="k-label">المفتاح</span>
          <input className="k-input k-input--mono" dir="ltr" value={key} onChange={(event) => setKey(event.target.value)} />
        </label>
        <label className="k-field k-field--grow">
          <span className="k-label">القيمة</span>
          <input className="k-input k-input--mono" dir="ltr" value={value} onChange={(event) => setValue(event.target.value)} />
        </label>
        <Button variant="secondary" disabled={!key.trim()} onClick={() => void save()}>حفظ</Button>
      </div>
      {error ? <div className="k-notice k-notice--error" role="alert">{error}</div> : null}
      {rows.length === 0 ? (
        <p className="k-note">لا إعدادات مخصّصة محفوظة لهذا المشروع؛ يعمل المحرك على القيم الافتراضية.</p>
      ) : (
        <table className="k-table">
          <thead>
            <tr><th>المفتاح</th><th>القيمة</th><th>المصدر</th><th>الملف</th></tr>
          </thead>
          <tbody>
            {rows.map((r, i) => (
              <tr key={r.key + i}>
                <td><Mono>{r.key}</Mono></td>
                <td><Mono>{r.value}</Mono></td>
                <td><Badge variant="neutral">{originLabel(r.source)}</Badge></td>
                <td className="k-path">{r.filePath ? <Mono>{r.filePath}</Mono> : <span className="k-note">—</span>}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}
