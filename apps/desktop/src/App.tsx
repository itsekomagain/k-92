import { useEffect, useState } from "react";

import { Icon, type IconName } from "./components/Icon";
import { Banner } from "./components/ui";
import { Onboarding } from "./screens/Onboarding";
import { ActivityScreen, RestoreScreen, SettingsScreen, StorageScreen, type Theme } from "./screens/Other";
import { Overview } from "./screens/Overview";
import { Timeline } from "./screens/Timeline";
import { useStore } from "./store";

export type Screen = "overview" | "timeline" | "storage" | "restore" | "activity" | "settings";

const nav: { id: Screen; label: string; icon: IconName }[] = [
  { id: "overview", label: "Overview", icon: "overview" },
  { id: "timeline", label: "Timeline", icon: "timeline" },
  { id: "restore", label: "Restore", icon: "restore" },
  { id: "storage", label: "Storage", icon: "storage" },
  { id: "activity", label: "Activity", icon: "activity" },
  { id: "settings", label: "Settings", icon: "settings" },
];

function loadTheme(): Theme {
  try {
    const t = localStorage.getItem("k92.theme");
    return t === "light" || t === "dark" ? t : "system";
  } catch {
    return "system";
  }
}

export function App() {
  const { status, loadError, toasts, dismiss } = useStore();
  const [screen, setScreen] = useState<Screen>("overview");
  const [theme, setTheme] = useState<Theme>(loadTheme);

  useEffect(() => {
    if (theme === "system") delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = theme;
    try { localStorage.setItem("k92.theme", theme); } catch { /* per-device convenience only */ }
  }, [theme]);

  // Keyboard: Ctrl+1…6 switches screens.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.ctrlKey) return;
      const n = Number(e.key);
      const target = nav[n - 1];
      if (target) { e.preventDefault(); setScreen(target.id); }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  if (!status) {
    return <div className="onboard">{loadError ? <Banner tone="bad">{loadError}</Banner> : null}</div>;
  }
  if (!status.configured) return <><Onboarding /><Toasts toasts={toasts} dismiss={dismiss} /></>;

  const protectedState = status.running?.kind === "backup" ? ["info", "Backing up"]
    : !status.destinationAvailable ? ["warn", "Drive not connected"]
    : status.lastError ? ["bad", "Needs attention"]
    : status.lastSnapshot ? ["ok", "Protected"] : ["info", "Not yet backed up"];

  return (
    <div className="app">
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-mark"><Icon name="timeline" size={20} /></div>
          <div>
            <div className="brand-name">K-92</div>
            <div className="brand-sub">Backup &amp; Recovery</div>
          </div>
        </div>
        <nav className="nav" aria-label="Main">
          {nav.map((n) => (
            <button key={n.id} className="nav-item" aria-current={screen === n.id ? "page" : undefined} onClick={() => setScreen(n.id)}>
              <Icon name={n.icon} />{n.label}
            </button>
          ))}
        </nav>
        <div className="sidebar-foot">
          <span className={`pill pill-${protectedState[0]}`}><span className="dot" />{protectedState[1]}</span>
        </div>
      </aside>
      <main className="main">
        {screen === "overview" && <Overview go={setScreen} />}
        {screen === "timeline" && <Timeline />}
        {screen === "restore" && <RestoreScreen />}
        {screen === "storage" && <StorageScreen />}
        {screen === "activity" && <ActivityScreen />}
        {screen === "settings" && <SettingsScreen theme={theme} setTheme={setTheme} />}
      </main>
      <Toasts toasts={toasts} dismiss={dismiss} />
    </div>
  );
}

function Toasts({ toasts, dismiss }: { toasts: ReturnType<typeof useStore>["toasts"]; dismiss: (id: number) => void }) {
  const icon = { ok: "check", warn: "alert", bad: "alert", info: "info" } as const;
  const color = { ok: "var(--ok)", warn: "var(--warn)", bad: "var(--bad)", info: "var(--accent)" } as const;
  return (
    <div className="toast-wrap" aria-live="polite">
      {toasts.map((t) => (
        <div className="toast" key={t.id} role="status">
          <span style={{ color: color[t.tone] }}><Icon name={icon[t.tone]} /></span>
          <div style={{ flex: 1 }}>
            <div style={{ fontWeight: 600 }}>{t.title}</div>
            {t.body && <div className="small muted">{t.body}</div>}
          </div>
          <button className="btn btn-ghost btn-sm" aria-label="Dismiss" onClick={() => dismiss(t.id)}><Icon name="x" size={14} /></button>
        </div>
      ))}
    </div>
  );
}
