import { useEffect, useState } from "react";
import { useStore, wireEngine } from "../lib/store";
import { ChatWorkspace } from "./ChatWorkspace";
import { Rules } from "./Rules";
import { DiffReview } from "./DiffReview";
import { ResumePicker } from "./ResumePicker";
import { AgentsTasks } from "./AgentsTasks";
import { Consoles } from "./Consoles";
import { ContextMemory } from "./ContextMemory";
import { SettingsScreen } from "./Settings";

export type ScreenId = "s01" | "s06" | "s08" | "s09" | "s10" | "s11" | "s13" | "s14";

const NAV: { id: ScreenId; label: string; description: string; icon: string }[] = [
  { id: "s01", label: "المحادثات", description: "مساحة العمل", icon: "chat" },
  { id: "s09", label: "استئناف جلسة", description: "الجلسات المحفوظة", icon: "history" },
  { id: "s08", label: "مراجعة التغييرات", description: "الفروقات والملفات", icon: "diff" },
  { id: "s10", label: "المهام والوكلاء", description: "العمل الجاري", icon: "agents" },
  { id: "s13", label: "السياق والذاكرة", description: "معرفة المشروع", icon: "memory" },
  { id: "s06", label: "القواعد", description: "الصلاحيات", icon: "shield" },
  { id: "s11", label: "التوسعات", description: "المهارات وMCP", icon: "extensions" },
  { id: "s14", label: "الإعدادات", description: "التفضيلات والمظهر", icon: "settings" },
];

function NavIcon({ name }: { name: string }) {
  const paths: Record<string, string> = {
    chat: "M4 5.5A2.5 2.5 0 0 1 6.5 3h11A2.5 2.5 0 0 1 20 5.5v8a2.5 2.5 0 0 1-2.5 2.5H10l-5 4v-4.8a2.5 2.5 0 0 1-1-2V5.5Z",
    history: "M4 7v5h5M4.8 11a7.5 7.5 0 1 0 .8-5.2L4 8M12 7v5l3 2",
    diff: "M8 3H4a1 1 0 0 0-1 1v4m13-5h4a1 1 0 0 1 1 1v4M3 16v4a1 1 0 0 0 1 1h4m13-5v4a1 1 0 0 1-1 1h-4M8 8l8 8m0-8-8 8",
    agents: "M9 5a3 3 0 1 0 0-6 3 3 0 0 0 0 6Zm9 5a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5ZM3 21v-2a5 5 0 0 1 10 0v2H3Zm12 0v-1a6 6 0 0 0-1-3 4 4 0 0 1 7 3v1h-6Z",
    memory: "M9 4a3 3 0 0 0-3 3v1a3 3 0 0 0-2 5 3 3 0 0 0 3 5 3 3 0 0 0 5 2V4a3 3 0 0 0-3 0Zm6 0a3 3 0 0 1 3 3v1a3 3 0 0 1 2 5 3 3 0 0 1-3 5 3 3 0 0 1-5 2M9 9H6m3 5H5m10-5h3m-3 5h4",
    shield: "M12 3 4 6v5c0 5 3.4 8.4 8 10 4.6-1.6 8-5 8-10V6l-8-3Zm-3 9 2 2 4-4",
    extensions: "M8 3v5H3v5h5v5h5v-5h5V8h-5V3H8Z",
    settings: "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8Zm0-5v3m0 12v3M3 12h3m12 0h3M5.6 5.6l2.1 2.1m8.6 8.6 2.1 2.1m0-12.8-2.1 2.1m-8.6 8.6-2.1 2.1",
  };
  return (
    <svg className="k-nav__icon" viewBox="0 0 24 24" fill="none" aria-hidden="true">
      <path d={paths[name]} stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round" />
    </svg>
  );
}

export function Shell() {
  const route = useStore((s) => s.screen);
  const setScreen = useStore((s) => s.setScreen);
  const pickProject = useStore((s) => s.pickProject);
  const activeProject = useStore((s) => s.activeProject);
  const lagged = useStore((s) => s.lagged);
  const [navigationOpen, setNavigationOpen] = useState(false);

  useEffect(() => {
    void useStore.getState().bootstrap();
    let unwire: (() => void) | undefined;
    void wireEngine()
      .then((fn) => { unwire = fn; })
      .catch(() => {
        if (!useStore.getState().error) {
          useStore.setState({ error: "تعذّر الوصول إلى مضيف kiri. هذه الواجهة تعمل داخل تطبيق kiri فقط." });
        }
      });
    return () => unwire?.();
  }, []);

  function navigate(id: ScreenId) {
    setScreen(id);
    setNavigationOpen(false);
  }

  return (
    <div className="k-app k-app--shell">
      {navigationOpen ? <button className="k-nav-scrim" aria-label="إغلاق القائمة" onClick={() => setNavigationOpen(false)} /> : null}
      <aside className={`k-navigation ${navigationOpen ? "is-open" : ""}`}>
        <div className="k-brand">
          <span className="k-brand__mark" aria-hidden="true">k</span>
          <span className="k-brand__text"><strong>kiri</strong><small>مساحة عملك الذكية</small></span>
        </div>

        <nav className="k-nav" aria-label="التنقل الرئيسي">
          <span className="k-nav__label">مساحة العمل</span>
          {NAV.map((item) => (
            <button
              key={item.id}
              type="button"
              className={`k-navbtn ${route === item.id ? "is-active" : ""}`}
              aria-current={route === item.id ? "page" : undefined}
              title={item.description}
              onClick={() => navigate(item.id)}
            >
              <NavIcon name={item.icon} />
              <span>{item.label}</span>
              {item.id === "s10" && lagged > 0 ? <span className="k-nav__count">{lagged}</span> : null}
            </button>
          ))}
        </nav>

        <div className="k-navigation__footer">
          <span className="k-nav__label">المشروع الحالي</span>
          <button type="button" className="k-project-switch" onClick={() => void pickProject()}>
            <span className="k-project-switch__icon" aria-hidden="true">
              <svg viewBox="0 0 24 24" fill="none"><path d="M3 7a2 2 0 0 1 2-2h5l2 2h7a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7Z" stroke="currentColor" strokeWidth="1.6" /></svg>
            </span>
            <span><strong>{activeProject?.name ?? "فتح مشروع"}</strong><small>{activeProject ? "تغيير مجلد المشروع" : "اختر مجلدًا للبدء"}</small></span>
          </button>
        </div>
      </aside>

      <div className="k-workspace">
        {route === "s01" ? <ChatWorkspace onOpenNavigation={() => setNavigationOpen(true)} /> : null}
        {route === "s06" ? <Rules /> : null}
        {route === "s08" ? <DiffReview /> : null}
        {route === "s09" ? <ResumePicker /> : null}
        {route === "s10" ? <AgentsTasks /> : null}
        {route === "s11" ? <Consoles /> : null}
        {route === "s13" ? <ContextMemory /> : null}
        {route === "s14" ? <SettingsScreen /> : null}
      </div>
    </div>
  );
}
