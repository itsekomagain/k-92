import { open } from "@tauri-apps/plugin-dialog";
import { useEffect, useRef, useState } from "react";

import { Icon } from "../components/Icon";
import { Banner, Empty, JobProgress } from "../components/ui";
import { api, errorMessage } from "../ipc/api";
import type { ActivityEntry } from "../ipc/bindings/ActivityEntry";
import type { AppStatus } from "../ipc/bindings/AppStatus";
import type { BrowseEntry } from "../ipc/bindings/BrowseEntry";
import type { SnapshotSummary } from "../ipc/bindings/SnapshotSummary";
import type { StorageInfo } from "../ipc/bindings/StorageInfo";
import { baseName, formatBytes, formatMs, formatWhen } from "../lib/format";
import { useStore } from "../store";
import { RestoreDialog, type RestoreItem } from "./RestoreDialog";

// ------------------------------------------------------------------------------------------------
// Restore: "find my file" flow for people who don't want to browse the timeline.

export function RestoreScreen() {
  const { generation, status } = useStore();
  const [snapshots, setSnapshots] = useState<SnapshotSummary[]>([]);
  const [snapId, setSnapId] = useState<string>("");
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<BrowseEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [restoring, setRestoring] = useState<RestoreItem[] | null>(null);
  const timer = useRef<number | undefined>(undefined);

  useEffect(() => {
    if (!status?.destinationAvailable) return;
    api.listSnapshots().then((l) => {
      const rev = [...l.snapshots].reverse();
      setSnapshots(rev);
      setSnapId((cur) => (cur && rev.some((s) => s.id === cur) ? cur : rev[0]?.id ?? ""));
    }, (e) => setError(errorMessage(e)));
  }, [generation, status?.destinationAvailable]);

  useEffect(() => {
    window.clearTimeout(timer.current);
    if (!query.trim() || !snapId) { setHits(null); return; }
    timer.current = window.setTimeout(() => {
      api.search(snapId, query).then(setHits, (e) => setError(errorMessage(e)));
    }, 200);
  }, [query, snapId]);

  const snap = snapshots.find((s) => s.id === snapId);
  if (status && !status.destinationAvailable) {
    return <div className="page"><h1>Restore</h1><div className="card" style={{ marginTop: 20 }}><Empty icon="drive" title="Connect your backup drive" /></div></div>;
  }

  return (
    <div className="page">
      <div className="page-head">
        <div>
          <h1>Restore</h1>
          <div className="muted">Find a file by name and bring it back.</div>
        </div>
      </div>
      <div className="card stack">
        <div className="row" style={{ flexWrap: "wrap" }}>
          <div className="row" style={{ position: "relative", flex: 1, minWidth: 240 }}>
            <span style={{ position: "absolute", left: 10, top: 9 }} className="faint"><Icon name="search" size={16} /></span>
            <input className="input" autoFocus style={{ paddingLeft: 34, width: "100%" }} placeholder="File or folder name" value={query} onChange={(e) => setQuery(e.target.value)} aria-label="File or folder name" />
          </div>
          <select className="input" value={snapId} onChange={(e) => setSnapId(e.target.value)} aria-label="Backup to search">
            {snapshots.map((s, i) => <option key={s.id} value={s.id}>{i === 0 ? `Latest (${formatWhen(s.completedAt)})` : formatWhen(s.completedAt)}</option>)}
          </select>
        </div>
        {error && <Banner tone="bad">{error}</Banner>}
        {hits === null ? (
          <div className="small muted">Tip: a deleted file is still in older backups. Pick an earlier date if it doesn't show up in the latest one.</div>
        ) : hits.length === 0 ? (
          <Empty icon="search" title="Nothing found in this backup">Try an earlier backup from the list above.</Empty>
        ) : (
          <div className="list">
            {hits.map((h) => (
              <div className="list-row" key={`${h.source}:${h.path}`}>
                <Icon name={h.kind === "dir" ? "folder" : "file"} className={h.kind === "dir" ? "icon-folder" : "icon-file"} />
                <div style={{ minWidth: 0, flex: 1 }}>
                  <div style={{ fontWeight: 600 }}>{h.name}</div>
                  <div className="small faint" style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                    {snap?.sources.find((s) => s.id === h.source)?.label} / {h.path}
                  </div>
                </div>
                <span className="small muted">{h.kind === "file" ? `${formatBytes(h.size)} · ${formatMs(h.modifiedMs)}` : "Folder"}</span>
                <button className="btn btn-sm" onClick={() => setRestoring([{ source: h.source, path: h.path, name: h.name }])}>Restore</button>
              </div>
            ))}
          </div>
        )}
      </div>
      {restoring && snap && (
        <RestoreDialog snapshotId={snap.id} snapshotTime={snap.completedAt} items={restoring} sources={snap.sources} onClose={() => setRestoring(null)} />
      )}
    </div>
  );
}

// ------------------------------------------------------------------------------------------------

export function StorageScreen() {
  const { status, generation, progress, toast } = useStore();
  const [info, setInfo] = useState<StorageInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!status?.destinationAvailable) return;
    api.getStorage().then(setInfo, (e) => setError(errorMessage(e)));
  }, [generation, status?.destinationAvailable]);

  const verifying = status?.running?.kind === "verify";
  async function verify(deep: boolean) {
    try { await api.startVerify(deep); } catch (e) { toast({ tone: "bad", title: "Could not start check", body: errorMessage(e) }); }
  }

  return (
    <div className="page">
      <div className="page-head"><h1>Storage</h1></div>
      {!status?.destinationAvailable && <Banner tone="warn">The backup drive is not connected.</Banner>}
      {error && <Banner tone="bad">{error}</Banner>}
      <div className="grid grid-3" style={{ marginBottom: 16, marginTop: 4 }}>
        <div className="card"><div className="stat-label">Space used by backups</div><div className="stat-value">{info ? formatBytes(info.repositoryBytes) : "—"}</div></div>
        <div className="card"><div className="stat-label">Size of your files</div><div className="stat-value">{info ? formatBytes(info.latestLogicalBytes) : "—"}</div><div className="small muted" style={{ marginTop: 6 }}>in the latest backup</div></div>
        <div className="card"><div className="stat-label">Backups kept</div><div className="stat-value">{info ? Number(info.snapshotCount).toLocaleString() : "—"}</div></div>
      </div>
      <div className="card stack">
        <div className="card-head" style={{ marginBottom: 0 }}>
          <div>
            <h2>Backup health</h2>
            <div className="small muted">Checks that every backup can be restored. The full check reads all stored data and can take a while.</div>
          </div>
        </div>
        {verifying ? (
          <JobProgress progress={progress} />
        ) : (
          <div className="row">
            <button className="btn" onClick={() => void verify(false)} disabled={!status?.destinationAvailable}>Quick check</button>
            <button className="btn" onClick={() => void verify(true)} disabled={!status?.destinationAvailable}>Full check</button>
          </div>
        )}
        <div className="small faint selectable" style={{ overflowWrap: "anywhere" }}>{info?.path ?? status?.destination}</div>
      </div>
    </div>
  );
}

// ------------------------------------------------------------------------------------------------

const outcomePill = { success: "pill-ok", warning: "pill-warn", failed: "pill-bad", cancelled: "pill-info" } as const;
const kindLabel = { backup: "Backup", restore: "Restore", verify: "Health check", prune: "Cleanup" } as const;

export function ActivityScreen() {
  const { generation } = useStore();
  const [items, setItems] = useState<ActivityEntry[] | null>(null);
  useEffect(() => { void api.getActivity().then(setItems, () => setItems([])); }, [generation]);
  return (
    <div className="page">
      <div className="page-head"><h1>Activity</h1></div>
      <div className="card">
        {items === null ? null : items.length === 0 ? (
          <Empty icon="activity" title="No activity yet" />
        ) : (
          <div className="list">
            {items.map((a, i) => (
              <div className="list-row" key={`${a.at}-${i}`}>
                <span className={`pill ${outcomePill[a.outcome]}`} style={{ minWidth: 92, justifyContent: "center" }}>{kindLabel[a.kind]}</span>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div>{a.summary}</div>
                  <div className="small faint">{formatWhen(a.at)} · {(Number(a.durationMs) / 1000).toFixed(1)} s</div>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

// ------------------------------------------------------------------------------------------------

export type Theme = "system" | "light" | "dark";

export function SettingsScreen({ theme, setTheme }: { theme: Theme; setTheme: (t: Theme) => void }) {
  const { status, setStatus, toast } = useStore();
  if (!status) return null;
  const s = status.settings;

  async function save(p: Promise<AppStatus>) {
    try { setStatus(await p); } catch (e) { toast({ tone: "bad", title: "Could not save", body: errorMessage(e) }); }
  }
  async function addFolder() {
    const picked = await open({ directory: true, multiple: true, title: "Add folders to back up" });
    const list = picked === null ? [] : Array.isArray(picked) ? picked : [picked];
    if (list.length) await save(api.updateSources([...new Set([...status!.sources, ...list])]));
  }

  return (
    <div className="page">
      <div className="page-head"><h1>Settings</h1></div>
      <div className="stack" style={{ gap: 16 }}>
        <div className="card stack">
          <h2>Schedule</h2>
          <div className="row" style={{ flexWrap: "wrap" }}>
            <label htmlFor="interval" className="muted">Back up automatically</label>
            <select id="interval" className="input" value={s.intervalMinutes} onChange={(e) => void save(api.updateSettings({ ...s, intervalMinutes: Number(e.target.value) }))}>
              <option value={15}>Every 15 minutes</option>
              <option value={30}>Every 30 minutes</option>
              <option value={60}>Every hour</option>
              <option value={240}>Every 4 hours</option>
              <option value={1440}>Once a day</option>
              <option value={0}>Only when I click Back up now</option>
            </select>
            <span className="spacer" />
            <button className="btn" onClick={() => void save(api.updateSettings({ ...s, paused: !s.paused }))}>
              <Icon name={s.paused ? "play" : "pause"} size={16} />{s.paused ? "Resume backups" : "Pause backups"}
            </button>
          </div>
          <div className="small muted">
            While K-92's background service is not yet installed, scheduled backups run only while this app is open.
          </div>
        </div>

        <div className="card stack">
          <div className="card-head" style={{ marginBottom: 0 }}><h2>Folders to back up</h2>
            <button className="btn btn-sm" onClick={() => void addFolder()}><Icon name="plus" size={14} />Add</button>
          </div>
          {status.sources.map((p) => (
            <div className="folder-row" key={p}>
              <Icon name="folder" className="icon-folder" />
              <span className="path" title={p}><strong>{baseName(p)}</strong> <span className="faint small">{p}</span></span>
              <button className="btn btn-ghost btn-sm" disabled={status.sources.length === 1} aria-label={`Stop backing up ${p}`}
                onClick={() => void save(api.updateSources(status.sources.filter((x) => x !== p)))}>
                <Icon name="x" size={16} />
              </button>
            </div>
          ))}
          <div className="small muted">Removing a folder stops future backups of it. Earlier backups keep their copies.</div>
        </div>

        <div className="card stack">
          <h2>Appearance</h2>
          <div className="row" role="radiogroup" aria-label="Theme">
            {(["system", "light", "dark"] as const).map((t) => (
              <label key={t} className="choice" style={{ padding: "8px 14px" }}>
                <input type="radio" name="theme" checked={theme === t} onChange={() => setTheme(t)} />
                {t === "system" ? "Match Windows" : t === "light" ? "Light" : "Dark"}
              </label>
            ))}
          </div>
        </div>

        <div className="card stack">
          <h2>About</h2>
          <div className="small muted">K-92 0.1.0 · Your PC. Your history. Your way back.</div>
          <div className="small muted">K-92 never uploads your files and has no telemetry. Backups are readable without this app using the portable <span className="mono">k92</span> tool.</div>
        </div>
      </div>
    </div>
  );
}

