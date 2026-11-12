// S11 Hooks & skills console.
//
// These are the engine's extension surfaces: a hook fires on a lifecycle event
// (a PreToolUse command that exits 2 denies the call), and a skill is a
// SKILL.md listed by name + description and loaded in full only when asked.
// Nothing here is guessed -- the host returns what the engine reported on
// `initialize`, and the screen says so when it has no live session to ask.

import { useState } from "react";
import { Badge, EmptyState, Mono } from "./primitives";
import { useStore } from "../lib/store";

interface HookRow {
  event: string;
  matcher?: string | null;
  hookType: string;
  command?: string | null;
  url?: string | null;
  timeoutS: number;
  enabled: boolean;
  sourcePath?: string | null;
}

interface SkillRow {
  name: string;
  description: string;
  sourcePath?: string | null;
  allowedTools?: string[];
}

interface PluginRow {
  name: string;
  version?: string | null;
  enabled: boolean;
  path?: string | null;
}

export function Consoles() {
  const session = useStore((s) => s.activeSession);
  const stream = useStore((s) => s.stream);
  const [tab, setTab] = useState<"hooks" | "skills" | "plugins">("hooks");

  // The engine reports its effective extension surfaces in `system:init`.
  // The screen shows exactly what the running engine loaded -- not the files
  // we assume it read.
  const init: any = (stream.init ?? {}) as any;
  const hooks: HookRow[] = init.hooks ?? [];
  const skills: SkillRow[] = init.skills ?? [];
  const plugins: PluginRow[] = init.plugins ?? [];
  const hasInit = Object.keys(init).length > 0;

  return (
    <main className="k-main k-main--page">
      <section className="k-page">
        <header className="k-page__head">
          <h1>الخطافات والمهارات</h1>
          <p className="k-note">
            كلها تُحمّل من <Mono>.kiri/</Mono> عند بدء الجلسة. تعديل ملف على القرص
            يحتاج إعادة تهيئة (<Mono>reinitialize</Mono>) ليسري على الجلسة الحيّة.
          </p>
        </header>

        {session && !hasInit ? (
          <div className="k-notice k-notice--warning" role="status">
            لا توجد بيانات تهيئة محفوظة لهذه الجلسة (مثلاً بعد إعادة تشغيل
            التطبيق). ابدأ جلسة جديدة لعرض ما حمّله المحرك فعليًا.
          </div>
        ) : null}

        <div className="k-tabs" role="tablist" aria-label="لوحة التوسعات">
          <Tab id="hooks" current={tab} onPick={setTab}>الخطافات</Tab>
          <Tab id="skills" current={tab} onPick={setTab}>المهارات</Tab>
          <Tab id="plugins" current={tab} onPick={setTab}>الإضافات</Tab>
        </div>

        {!session ? (
          <EmptyState title="لا جلسة مفتوحة" body="افتح جلسة ثم عُد: اللوحة تعرض ما حمّله المحرك فعليًا." />
        ) : null}

        {session && tab === "hooks" ? (
          hooks.length === 0 ? (
            <EmptyState title="لا خطافات" body="أضف خطافًا في settings.json تحت المفتاح hooks، أو في .kiri/settings.local.json." />
          ) : (
            <HooksTable hooks={hooks} />
          )
        ) : null}

        {session && tab === "skills" ? (
          skills.length === 0 ? (
            <EmptyState title="لا مهارات" body="يمكن للمحرك قراءة SKILL.md من .kiri/skills/ ومن --plugin-dir." />
          ) : (
            <SkillsTable skills={skills} />
          )
        ) : null}

        {session && tab === "plugins" ? (
          plugins.length === 0 ? (
            <EmptyState title="لا إضافات" body="أضف إضافة بـ --plugin-dir أو من ملف الإعدادات." />
          ) : (
            <PluginsTable plugins={plugins} />
          )
        ) : null}
      </section>
    </main>
  );
}

function Tab(props: {
  id: string;
  current: string;
  onPick: (id: any) => void;
  children: React.ReactNode;
}) {
  const selected = props.current === props.id;
  return (
    <button
      type="button"
      role="tab"
      aria-selected={selected}
      className={`k-tab ${selected ? "is-selected" : ""}`}
      onClick={() => props.onPick(props.id)}
    >
      {props.children}
    </button>
  );
}

function HooksTable({ hooks }: { hooks: HookRow[] }) {
  return (
    <table className="k-table">
      <thead>
        <tr>
          <th>الحدث</th>
          <th>المطابق</th>
          <th>النوع</th>
          <th>الأمر / URL</th>
          <th>المهلة</th>
          <th>المصدر</th>
        </tr>
      </thead>
      <tbody>
        {hooks.map((h, i) => (
          <tr key={i}>
            <td><Mono>{h.event}</Mono></td>
            <td>{h.matcher ? <Mono>{h.matcher}</Mono> : <span className="k-note">الكل</span>}</td>
            <td><Badge variant={h.hookType === "command" ? "info" : h.hookType === "http" ? "warning" : "success"}>{h.hookType}</Badge></td>
            <td className="k-path"><Mono>{h.command ?? h.url ?? "—"}</Mono></td>
            <td>{h.timeoutS}s</td>
            <td>{h.sourcePath ? <Mono>{shortPath(h.sourcePath)}</Mono> : <span className="k-note">—</span>}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function SkillsTable({ skills }: { skills: SkillRow[] }) {
  return (
    <ul className="k-cards" role="list">
      {skills.map((s) => (
        <li key={s.name} className="k-card">
          <h3>{s.name}</h3>
          {s.description ? <p>{s.description}</p> : null}
          {s.allowedTools?.length ? (
            <p className="k-note">
              أدوات مسموحة: {s.allowedTools.join("، ")}
            </p>
          ) : null}
          {s.sourcePath ? <Mono>{s.sourcePath}</Mono> : null}
        </li>
      ))}
    </ul>
  );
}

function PluginsTable({ plugins }: { plugins: PluginRow[] }) {
  return (
    <table className="k-table">
      <thead>
        <tr>
          <th>الاسم</th>
          <th>الإصدار</th>
          <th>الحالة</th>
          <th>المسار</th>
        </tr>
      </thead>
      <tbody>
        {plugins.map((p, i) => (
          <tr key={p.name + i}>
            <td>{p.name}</td>
            <td><Mono>{p.version ?? "—"}</Mono></td>
            <td><Badge variant={p.enabled ? "success" : "neutral"}>{p.enabled ? "مفعّلة" : "معطّلة"}</Badge></td>
            <td className="k-path"><Mono>{p.path ?? "—"}</Mono></td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function shortPath(p: string): string {
  const parts = p.split(/[\\/]/);
  return parts.slice(-2).join("/");
}
