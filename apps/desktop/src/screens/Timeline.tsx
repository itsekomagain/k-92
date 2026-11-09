import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";

import { Icon } from "../components/Icon";
import { Banner, Empty } from "../components/ui";
import { api, errorMessage } from "../ipc/api";
import type { BrowseEntry } from "../ipc/bindings/BrowseEntry";
import type { FileVersion } from "../ipc/bindings/FileVersion";
import type { SnapshotList } from "../ipc/bindings/SnapshotList";
import type { SnapshotSummary } from "../ipc/bindings/SnapshotSummary";
import { dayLabel, formatBytes, formatMs, formatShortDay, formatTime, formatWhen, plural } from "../lib/format";
import { useStore } from "../store";
import { RestoreDialog, type RestoreItem } from "./RestoreDialog";

const key = (e: { source: number; path: string }) => `${e.source}:${e.path}`;

export function Timeline() {
  const { generation, status } = useStore();
  const [list, setList] = useState<SnapshotList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);

  useEffect(() => {
    if (!status?.destinationAvailable) return;
    api.listSnapshots().then(
      (l) => {
        setList(l);
        setError(null);
        setSelectedId((cur) => (cur && l.snapshots.some((s) => s.id === cur) ? cur : l.snapshots.at(-1)?.id ?? null));
      },
      (e) => setError(errorMessage(e)),
    );
  }, [generation, status?.destinationAvailable]);

  const snapshots = list?.snapshots ?? [];
  const selected = snapshots.find((s) => s.id === selectedId) ?? null;
  const days = useMemo(() => groupByDay(snapshots), [snapshots]);

  function step(delta: number) {
    const i = snapshots.findIndex((s) => s.id === selectedId);
    const next = snapshots[Math.min(snapshots.length - 1, Math.max(0, i + delta))];
    if (next) setSelectedId(next.id);
  }
  const onRailKey = (e: KeyboardEvent) => {
    if (e.key === "ArrowUp" || e.key === "ArrowRight") { e.preventDefault(); step(1); }
    if (e.key === "ArrowDown" || e.key === "ArrowLeft") { e.preventDefault(); step(-1); }
  };

  if (status && !status.destinationAvailable) {
    return <div className="page"><h1>Timeline</h1><div className="card" style={{ marginTop: 20 }}><Empty icon="drive" title="Connect your backup drive">Your history is stored on {status.destination ?? "your backup drive"}.</Empty></div></div>;
  }

  return (
    <div className="page-wide">
      <div className="tl">
        <aside className="tl-rail" aria-label="Backups">
          <div className="tl-rail-head">
            <h1 style={{ fontSize: 22 }}>Timeline</h1>
            <div className="small muted">{plural(snapshots.length, "backup")}{list && list.damaged.length > 0 ? ` · ${list.damaged.length} damaged` : ""}</div>
          </div>
          <div className="tl-scroll" role="listbox" aria-label="Backup points" tabIndex={0} onKeyDown={onRailKey}>
            {days.map(([label, items]) => (
              <div className="tl-day" key={label}>
                <div className="tl-day-label">{label}</div>
                {items.map((s) => (
                  <button
                    key={s.id}
                    className="tl-point"
                    role="option"
                    aria-selected={s.id === selectedId}
                    tabIndex={-1}
                    onClick={() => setSelectedId(s.id)}
                  >
                    <span className="tl-time">{formatTime(new Date(s.completedAt))}</span>
                    <span className="small faint">{Number(s.newBytes) > 0 ? `+${formatBytes(Number(s.newBytes))}` : "no changes"}</span>
                    {Number(s.skipped) > 0 && <span className="pill pill-warn" title={`${s.skipped} items skipped`} style={{ marginLeft: "auto", padding: "1px 6px" }}>{Number(s.skipped)}</span>}
                  </button>
                ))}
              </div>
            ))}
            {list?.damaged.map((d) => (
              <div key={d.id} className="small" style={{ color: "var(--bad)", padding: "8px 10px" }} title={d.error}>Damaged backup {d.id.slice(0, 15)}</div>
            ))}
          </div>
        </aside>

        <section className="browser" aria-label="Files in selected backup">
          {error && <div style={{ padding: 16 }}><Banner tone="bad">{error}</Banner></div>}
          {snapshots.length > 0 && <Scrubber snapshots={snapshots} selectedId={selectedId} onSelect={setSelectedId} onStep={step} />}
          {selected ? (
            <Browser key={selected.id} snapshot={selected} onJump={setSelectedId} />
          ) : (
            <Empty icon="timeline" title={list ? "No backups yet" : "Loading…"}>Your backup history will appear here.</Empty>
          )}
        </section>
      </div>
    </div>
  );
}

function groupByDay(snaps: SnapshotSummary[]): [string, SnapshotSummary[]][] {
  const out = new Map<string, SnapshotSummary[]>();
  for (const s of [...snaps].reverse()) {
    const label = dayLabel(new Date(s.completedAt));
    const arr = out.get(label) ?? [];
    arr.push(s);
    out.set(label, arr);
  }
  return [...out.entries()];
}

/** Horizontal history ribbon: one tick per backup, positioned by time. */
function Scrubber({ snapshots, selectedId, onSelect, onStep }: {
  snapshots: SnapshotSummary[];
  selectedId: string | null;
  onSelect: (id: string) => void;
  onStep: (d: number) => void;
}) {
  const times = snapshots.map((s) => new Date(s.completedAt).getTime());
  const min = times[0] ?? 0;
  const max = times[times.length - 1] ?? 0;
  const span = Math.max(1, max - min);
  const first = snapshots[0];
  const last = snapshots[snapshots.length - 1];
  return (
    <div className="scrubber">
      <div
        className="scrubber-track"
        role="slider"
        tabIndex={0}
        aria-label="Backup time"
        aria-valuemin={0}
        aria-valuemax={snapshots.length - 1}
        aria-valuenow={Math.max(0, snapshots.findIndex((s) => s.id === selectedId))}
        aria-valuetext={selectedId ? formatWhen(snapshots.find((s) => s.id === selectedId)?.completedAt) : ""}
        onKeyDown={(e) => {
          if (e.key === "ArrowLeft") { e.preventDefault(); onStep(-1); }
          if (e.key === "ArrowRight") { e.preventDefault(); onStep(1); }
        }}
      >
        <div className="scrubber-line" />
        {snapshots.map((s, i) => (
          <button
            key={s.id}
            className="scrubber-tick"
            tabIndex={-1}
            aria-selected={s.id === selectedId}
            title={formatWhen(s.completedAt)}
            style={{ left: snapshots.length === 1 ? "50%" : `${((times[i]! - min) / span) * 100}%` }}
            onClick={() => onSelect(s.id)}
          />
        ))}
      </div>
      <div className="scrubber-labels">
        <span>{first ? formatShortDay(new Date(first.completedAt)) : ""}</span>
        <span>{last ? `${formatShortDay(new Date(last.completedAt))}, ${formatTime(new Date(last.completedAt))}` : ""}</span>
      </div>
    </div>
  );
}

function Browser({ snapshot, onJump }: { snapshot: SnapshotSummary; onJump: (id: string) => void }) {
  const [source, setSource] = useState(snapshot.sources[0]?.id ?? 0);
  const [path, setPath] = useState("");
  const [entries, setEntries] = useState<BrowseEntry[] | null>(null);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<BrowseEntry[] | null>(null);
  const [checked, setChecked] = useState<Map<string, BrowseEntry>>(new Map());
  const [focused, setFocused] = useState<BrowseEntry | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [restoring, setRestoring] = useState<RestoreItem[] | null>(null);
  const searchTimer = useRef<number | undefined>(undefined);

  useEffect(() => {
    let live = true;
    setEntries(null);
    api.listDir(snapshot.id, source, path).then(
      (e) => { if (live) { setEntries(e); setError(null); } },
      (e) => {
        if (!live) return;
        // The folder may not exist in this backup (e.g. created later): fall back to the root.
        if (path !== "") setPath(""); else setError(errorMessage(e));
      },
    );
    return () => { live = false; };
  }, [snapshot.id, source, path]);

  useEffect(() => {
    window.clearTimeout(searchTimer.current);
    if (!query.trim()) { setResults(null); return; }
    searchTimer.current = window.setTimeout(() => {
      api.search(snapshot.id, query).then(setResults, (e) => setError(errorMessage(e)));
    }, 200);
  }, [query, snapshot.id]);

  const shown = results ?? entries ?? [];
  const src = snapshot.sources.find((s) => s.id === source);
  const crumbs = path ? path.split("/") : [];

  function toggle(e: BrowseEntry) {
    setChecked((m) => {
      const n = new Map(m);
      if (n.has(key(e))) n.delete(key(e)); else n.set(key(e), e);
      return n;
    });
  }

  function open(e: BrowseEntry) {
    if (e.kind === "dir") {
      setQuery("");
      setSource(e.source);
      setPath(e.path);
      setFocused(null);
    } else {
      setFocused(e);
    }
  }

  const selectedItems: RestoreItem[] = [...checked.values()].map((e) => ({ source: e.source, path: e.path, name: e.name }));
  const restoreTarget: RestoreItem[] =
    selectedItems.length > 0 ? selectedItems
      : focused ? [{ source: focused.source, path: focused.path, name: focused.name }]
      : [{ source, path, name: crumbs.at(-1) ?? src?.label ?? "" }];

  return (
    <>
      <div className="browser-bar">
        {snapshot.sources.length > 1 && (
          <select className="input" value={source} onChange={(e) => { setSource(Number(e.target.value)); setPath(""); setChecked(new Map()); }} aria-label="Backed-up folder">
            {snapshot.sources.map((s) => <option key={s.id} value={s.id}>{s.label}</option>)}
          </select>
        )}
        <nav className="crumbs" aria-label="Folder path">
          <button className="crumb" onClick={() => { setPath(""); setQuery(""); }}>{src?.label ?? "Files"}</button>
          {crumbs.map((c, i) => (
            <span key={i} className="row" style={{ gap: 2 }}>
              <Icon name="chevron" size={14} className="faint" />
              <button className="crumb" onClick={() => setPath(crumbs.slice(0, i + 1).join("/"))}>{c}</button>
            </span>
          ))}
        </nav>
        <span className="spacer" />
        <div className="row" style={{ position: "relative" }}>
          <span style={{ position: "absolute", left: 9, top: 8 }} className="faint"><Icon name="search" size={16} /></span>
          <input className="input" style={{ paddingLeft: 32, width: 240 }} placeholder="Search this backup" value={query} onChange={(e) => setQuery(e.target.value)} aria-label="Search this backup" />
        </div>
      </div>

      {error && <div style={{ padding: 16 }}><Banner tone="bad">{error}</Banner></div>}

      <div className="browser-body">
        <div className="files">
          {entries === null && !results ? (
            <Empty icon="folder" title="Loading…" />
          ) : shown.length === 0 ? (
            <Empty icon={results ? "search" : "folder"} title={results ? "No matches" : "This folder is empty"} />
          ) : (
            <table className="ft">
              <thead>
                <tr>
                  <th style={{ width: 36 }} aria-label="Select" />
                  <th>Name</th>
                  <th>Date modified</th>
                  <th style={{ textAlign: "right" }}>Size</th>
                </tr>
              </thead>
              <tbody>
                {shown.map((e) => (
                  <tr
                    key={key(e)}
                    aria-selected={focused !== null && key(focused) === key(e)}
                    onClick={() => setFocused(e)}
                    onDoubleClick={() => open(e)}
                    onKeyDown={(ev) => {
                      if (ev.key === "Enter") open(e);
                      if (ev.key === " ") { ev.preventDefault(); toggle(e); }
                    }}
                    tabIndex={0}
                  >
                    <td><input type="checkbox" checked={checked.has(key(e))} onChange={() => toggle(e)} onClick={(ev) => ev.stopPropagation()} aria-label={`Select ${e.name}`} /></td>
                    <td>
                      <div className="name">
                        <Icon name={e.kind === "dir" ? "folder" : "file"} className={e.kind === "dir" ? "icon-folder" : "icon-file"} />
                        <span title={e.path}>{results ? e.path : e.name}</span>
                      </div>
                    </td>
                    <td className="muted">{formatMs(e.modifiedMs)}</td>
                    <td className="num">{e.kind === "file" ? formatBytes(e.size) : ""}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
        <Details entry={focused} sourcePath={src?.path ?? ""} snapshotId={snapshot.id} onJump={onJump} />
      </div>

      <div className="action-bar">
        <span className="small muted">
          {checked.size > 0 ? `${plural(checked.size, "item")} selected` : `Backup from ${formatWhen(snapshot.completedAt)}`}
        </span>
        {checked.size > 0 && <button className="btn btn-ghost btn-sm" onClick={() => setChecked(new Map())}>Clear</button>}
        <span className="spacer" />
        <button className="btn btn-primary" onClick={() => setRestoring(restoreTarget)}>
          <Icon name="restore" size={16} />
          {checked.size > 0 ? "Restore selected" : focused ? `Restore “${focused.name}”` : path ? "Restore this folder" : "Restore everything"}
        </button>
      </div>

      {restoring && (
        <RestoreDialog
          snapshotId={snapshot.id}
          snapshotTime={snapshot.completedAt}
          items={restoring}
          sources={snapshot.sources}
          onClose={() => { setRestoring(null); setChecked(new Map()); }}
        />
      )}
    </>
  );
}

function Details({ entry, sourcePath, snapshotId, onJump }: {
  entry: BrowseEntry | null;
  sourcePath: string;
  snapshotId: string;
  onJump: (id: string) => void;
}) {
  const [versions, setVersions] = useState<FileVersion[] | null>(null);
  useEffect(() => {
    setVersions(null);
    if (!entry || entry.kind !== "file") return;
    let live = true;
    api.fileVersions(sourcePath, entry.path).then((v) => live && setVersions(v), () => live && setVersions([]));
    return () => { live = false; };
  }, [entry, sourcePath]);

  if (!entry) {
    return <aside className="details"><div className="small muted">Select a file to see its details and earlier versions. Double-click a folder to open it.</div></aside>;
  }
  return (
    <aside className="details" aria-label="Details">
      <div className="row" style={{ alignItems: "flex-start" }}>
        <Icon name={entry.kind === "dir" ? "folder" : "file"} size={28} className={entry.kind === "dir" ? "icon-folder" : "icon-file"} />
        <div style={{ minWidth: 0 }}>
          <div style={{ fontWeight: 600, overflowWrap: "anywhere" }}>{entry.name}</div>
          <div className="small faint" style={{ overflowWrap: "anywhere" }}>{entry.path}</div>
        </div>
      </div>
      {entry.kind === "file" && (
        <>
          <div className="small" style={{ marginTop: 14, display: "grid", gridTemplateColumns: "auto 1fr", gap: "4px 12px" }}>
            <span className="muted">Size</span><span>{formatBytes(entry.size)}</span>
            <span className="muted">Modified</span><span>{formatMs(entry.modifiedMs)}</span>
          </div>
          <h2 style={{ marginTop: 20, marginBottom: 8 }}>Versions</h2>
          {versions === null ? (
            <div className="small muted">Looking through your history…</div>
          ) : (
            <div className="stack" style={{ gap: 6 }}>
              {versions.map((v) => {
                const current = v.hash === entry.hash;
                return (
                  <button
                    key={v.snapshotId}
                    className={`version${current ? " current" : ""}`}
                    onClick={() => onJump(v.snapshotId)}
                    disabled={v.snapshotId === snapshotId}
                    title="Show the backup that contains this version"
                    style={{ cursor: "pointer", textAlign: "left" }}
                  >
                    <span>
                      <div style={{ fontWeight: 600 }}>{formatWhen(v.snapshotTime)}</div>
                      <div className="small faint">{current ? "This version" : `${plural(Number(v.snapshotCount), "backup")}`}</div>
                    </span>
                    <span className="small muted">{formatBytes(Number(v.size))}</span>
                  </button>
                );
              })}
            </div>
          )}
        </>
      )}
    </aside>
  );
}
