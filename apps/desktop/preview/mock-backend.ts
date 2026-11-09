// Browser-only stand-in for the Rust backend, used to publish a clickable UI preview.
// It answers the same commands as src-tauri/src/commands.rs with SAMPLE data. Never shipped.

import type { AppSettings } from "../src/ipc/bindings/AppSettings";
import type { ActivityEntry } from "../src/ipc/bindings/ActivityEntry";
import type { AppStatus } from "../src/ipc/bindings/AppStatus";
import type { BrowseEntry } from "../src/ipc/bindings/BrowseEntry";
import type { DestinationInfo } from "../src/ipc/bindings/DestinationInfo";
import type { FileVersion } from "../src/ipc/bindings/FileVersion";
import type { JobEvent } from "../src/ipc/bindings/JobEvent";
import type { JobKind } from "../src/ipc/bindings/JobKind";
import type { RestorePlan } from "../src/ipc/bindings/RestorePlan";
import type { RestoreReport } from "../src/ipc/bindings/RestoreReport";
import type { RestoreRequestDto } from "../src/ipc/bindings/RestoreRequestDto";
import type { SnapshotList } from "../src/ipc/bindings/SnapshotList";
import type { SnapshotSummary } from "../src/ipc/bindings/SnapshotSummary";
import type { SourceDto } from "../src/ipc/bindings/SourceDto";
import type { StorageInfo } from "../src/ipc/bindings/StorageInfo";

// ---------------------------------------------------------------- event bus

type Listener = (payload: JobEvent) => void;
const listeners = new Set<Listener>();
export function subscribe(fn: Listener): () => void {
  listeners.add(fn);
  return () => listeners.delete(fn);
}
function emit(e: JobEvent) {
  listeners.forEach((l) => l(e));
}

// ---------------------------------------------------------------- sample world

const SOURCES: SourceDto[] = [
  { id: 0, path: "C:\\Users\\Ada\\Documents", label: "Documents" },
  { id: 1, path: "C:\\Users\\Ada\\Pictures", label: "Pictures" },
];
const DEST = "E:\\K-92 Backup";
const HOUR = 3_600_000;
const DAY = 24 * HOUR;
const MB = 1024 * 1024;

type Node = { name: string; size?: number; ageDays?: number; children?: Node[] };
const f = (name: string, size: number, ageDays = 3): Node => ({ name, size, ageDays });
const d = (name: string, children: Node[]): Node => ({ name, children });

const trees: Record<number, Node[]> = {
  0: [
    d("Finance", [f("Budget 2026.xlsx", 184_320, 0), f("Tax return 2025.pdf", 2.4 * MB, 40), f("Receipts.xlsx", 96_000, 12)]),
    d("University", [f("Thesis draft.docx", 1.1 * MB, 1), f("References.bib", 48_000, 9), d("Figures", [f("chart-01.png", 420_000, 6), f("chart-02.png", 388_000, 6)])]),
    d("Projects", [d("K-92 notes", [f("ideas.md", 6_200, 2), f("roadmap.md", 9_800, 2)]), f("Kitchen renovation.pptx", 8.7 * MB, 21)]),
    d("Empty folder", []),
    f("Résumé – Ada Lovelace.docx", 210_000, 30),
    f("Shopping list.txt", 640, 0),
  ],
  1: [
    d("2026", [d("Lisbon trip", [f("IMG_4102.jpg", 4.1 * MB, 18), f("IMG_4103.jpg", 3.8 * MB, 18), f("IMG_4117.heic", 2.9 * MB, 18)]), d("Family", [f("Birthday.jpg", 5.2 * MB, 4)])]),
    f("Wallpaper.png", 6.3 * MB, 60),
  ],
};

const iso = (t: number) => new Date(t).toISOString();
function snapId(t: number, n: number) {
  const s = new Date(t).toISOString().replace(/[-:]/g, "").replace(".", "");
  return `${s.slice(0, 15)}${s.slice(15, 18)}Z-${(0x1a2b3c4d + n * 7919).toString(16).slice(-8)}`;
}

let snapshots: SnapshotSummary[] = (() => {
  const now = Date.now();
  const top = new Date(now);
  top.setMinutes(30, 0, 0);
  let latest = top.getTime();
  if (latest > now) latest -= HOUR;
  const times: number[] = [];
  for (let h = 0; h < 9; h++) times.push(latest - h * HOUR); // today, hourly
  for (let dd = 1; dd <= 13; dd++) {
    times.push(latest - dd * DAY + 2 * HOUR);
    if (dd < 4) times.push(latest - dd * DAY - 3 * HOUR);
  }
  return times.sort((a, b) => a - b).map((t, i) => ({
    id: snapId(t, i),
    startedAt: iso(t - 40_000),
    completedAt: iso(t),
    sources: SOURCES,
    files: 21 + (i > 20 ? 1 : 0),
    dirs: 11,
    bytes: Math.round(58.2 * MB + i * 37_000),
    newBytes: i === 0 ? Math.round(58.2 * MB) : i % 3 === 0 ? 0 : 180_000 + ((i * 7919) % 900_000),
    skipped: i === 28 ? 1 : 0,
  }));
})();

const snapIndex = (id: string) => snapshots.findIndex((s) => s.id === id);
const snapTime = (id: string) => new Date(snapshots[snapIndex(id)]?.completedAt ?? Date.now()).getTime();

/** "Thesis draft.docx" was deleted 3 backups ago, to show restoring a deleted file. */
function present(path: string, idx: number) {
  return !(path === "University/Thesis draft.docx" && idx >= snapshots.length - 3);
}

function find(source: number, path: string): Node | null {
  if (path === "") return { name: "", children: trees[source] ?? [] };
  let cur: Node[] = trees[source] ?? [];
  let node: Node | null = null;
  for (const part of path.split("/")) {
    node = cur.find((n) => n.name === part) ?? null;
    if (!node) return null;
    cur = node.children ?? [];
  }
  return node;
}

function toEntry(source: number, path: string, n: Node, snapshotId: string): BrowseEntry {
  const t = snapTime(snapshotId);
  const budgetBump = n.name === "Budget 2026.xlsx" ? snapIndex(snapshotId) * 512 : 0;
  return {
    source,
    name: n.name,
    path,
    kind: n.children ? "dir" : "file",
    size: n.children ? 0 : Math.round((n.size ?? 0) + budgetBump),
    modifiedMs: Math.round(t - (n.ageDays ?? 0) * DAY - 25 * 60_000),
    hash: n.children ? null : `${(path.length * 2654435761 + budgetBump).toString(16).padStart(8, "0")}${"0".repeat(56)}`,
  };
}

function walk(source: number, nodes: Node[], prefix: string, out: { path: string; node: Node }[]) {
  for (const n of nodes) {
    const p = prefix ? `${prefix}/${n.name}` : n.name;
    out.push({ path: p, node: n });
    if (n.children) walk(source, n.children, p, out);
  }
}

// ---------------------------------------------------------------- app state

let configured = !location.hash.includes("setup");
let sources = SOURCES.map((s) => s.path);
let paused = false;
let interval = 60;
let extra: Pick<AppSettings, "exclude" | "skipCommonJunk" | "maxFileBytes"> = {
  exclude: ["C:\\Users\\Ada\\Documents\\Old ISOs", "*.iso"],
  skipCommonJunk: true,
  maxFileBytes: null,
};
let running: AppStatus["running"] = null;
const activity: ActivityEntry[] = [
  { at: iso(Date.now() - 2 * HOUR), kind: "backup", outcome: "success", summary: "Backed up 22 files (2 changed)", durationMs: 4100 },
  { at: iso(Date.now() - 3 * HOUR), kind: "backup", outcome: "success", summary: "Backed up 22 files (1 changed)", durationMs: 3800 },
  { at: iso(Date.now() - 26 * HOUR), kind: "restore", outcome: "success", summary: "Restored 3 files (verified)", durationMs: 900 },
  { at: iso(Date.now() - 2 * DAY), kind: "backup", outcome: "warning", summary: "Backed up 21 files (0 changed), 1 skipped", durationMs: 3500 },
  { at: iso(Date.now() - 5 * DAY), kind: "verify", outcome: "success", summary: "Verified 30 snapshots and 25 stored files (full check)", durationMs: 61000 },
];

window.addEventListener("hashchange", () => {
  configured = !location.hash.includes("setup");
  // A finished restore event makes the store refresh without showing a toast.
  emit({ type: "finished", kind: "restore", outcome: "success", summary: "", error: null });
});

function status(): AppStatus {
  const last = snapshots.at(-1) ?? null;
  return {
    configured,
    sources,
    destination: configured ? DEST : null,
    destinationAvailable: true,
    destinationError: null,
    lastSnapshot: last,
    snapshotCount: snapshots.length,
    lastError: null,
    running,
    nextBackupAt: paused || interval === 0 ? null : iso(new Date(last?.completedAt ?? Date.now()).getTime() + interval * 60_000),
    settings: { intervalMinutes: interval, paused, ...extra },
  };
}

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Plays a job: started → progress ticks → finished. */
async function play(kind: JobKind, files: number, phase: "copying" | "restoring" | "verifying", ms: number, summary: string) {
  running = { kind, startedAt: iso(Date.now()), progress: null };
  emit({ type: "started", kind });
  const names = ["Finance/Budget 2026.xlsx", "University/References.bib", "Projects/K-92 notes/ideas.md", "2026/Family/Birthday.jpg", "Shopping list.txt"];
  const steps = 12;
  for (let i = 1; i <= steps; i++) {
    await wait(ms / steps);
    const progress = {
      phase,
      filesDone: Math.round((files * i) / steps),
      filesTotal: files,
      bytesDone: Math.round((i / steps) * 6.4 * MB),
      bytesTotal: phase === "copying" ? Math.round(6.4 * MB) : null,
      currentPath: names[i % names.length] ?? null,
    };
    running = { ...running, progress };
    emit({ type: "progress", kind, progress });
  }
  running = null;
  activity.unshift({ at: iso(Date.now()), kind, outcome: "success", summary, durationMs: ms });
  emit({ type: "finished", kind, outcome: "success", summary, error: null });
}

// ---------------------------------------------------------------- commands

type Args = Record<string, unknown> | undefined;

export async function invoke<T>(cmd: string, args?: Args): Promise<T> {
  await wait(60);
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const a = (args ?? {}) as Record<string, any>;
  const r = handle(cmd, a);
  return (r instanceof Promise ? await r : r) as T;
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
function handle(cmd: string, a: Record<string, any>): unknown {
  switch (cmd) {
    case "get_status":
      return status();
    case "inspect_destination": {
      const info: DestinationInfo = {
        path: a.path, exists: true, writable: true, hasRepository: false,
        snapshotCount: null, lastBackupAt: null, sameVolumeAsSource: String(a.path).startsWith("C:"), problem: null,
      };
      return info;
    }
    case "configure":
      configured = true;
      sources = a.sources;
      if (location.hash) history.replaceState(null, "", location.pathname);
      return status();
    case "update_settings": {
      const s = a.settings as unknown as AppSettings;
      interval = s.intervalMinutes;
      paused = s.paused;
      extra = { exclude: s.exclude, skipCommonJunk: s.skipCommonJunk, maxFileBytes: s.maxFileBytes };
      return status();
    }
    case "update_sources":
      sources = a.sources;
      return status();
    case "start_backup": {
      if (running) throw { kind: "busy", message: "A backup is already running." };
      void play("backup", 22, "copying", 3600, "Backed up 22 files (1 changed)").then(() => {
        const t = Date.now();
        const prev = snapshots.at(-1)!;
        snapshots = [...snapshots, { ...prev, id: snapId(t, snapshots.length), startedAt: iso(t - 3600), completedAt: iso(t), newBytes: 185_344, skipped: 0 }];
      });
      return null;
    }
    case "cancel_job":
      return null;
    case "list_snapshots": {
      const l: SnapshotList = { snapshots, damaged: [] };
      return l;
    }
    case "list_dir": {
      const idx = snapIndex(a.snapshotId);
      const node = find(a.source, a.path);
      if (!node?.children) throw { kind: "notFound", message: `${a.path} is not in this backup` };
      const prefix = a.path as string;
      return node.children
        .map((n) => ({ n, p: prefix ? `${prefix}/${n.name}` : n.name }))
        .filter(({ p }) => present(p, idx))
        .map(({ n, p }) => toEntry(a.source, p, n, a.snapshotId))
        .sort((x, y) => (x.kind === y.kind ? x.name.localeCompare(y.name) : x.kind === "dir" ? -1 : 1));
    }
    case "search_snapshot": {
      const q = String(a.query).toLowerCase().trim();
      const idx = snapIndex(a.snapshotId);
      const out: BrowseEntry[] = [];
      for (const s of SOURCES) {
        const all: { path: string; node: Node }[] = [];
        walk(s.id, trees[s.id] ?? [], "", all);
        for (const { path, node } of all) {
          if (node.name.toLowerCase().includes(q) && present(path, idx)) out.push(toEntry(s.id, path, node, a.snapshotId));
        }
      }
      return out;
    }
    case "file_versions": {
      const n = snapshots.length;
      const picks = a.path === "Finance/Budget 2026.xlsx" ? [n - 1, n - 3, n - 9, n - 16]
        : a.path === "University/Thesis draft.docx" ? [n - 4, n - 12] : [Math.max(0, n - 20)];
      const vs: FileVersion[] = picks.filter((i) => i >= 0).map((i, k, arr) => {
        const s = snapshots[i]!;
        const e = toEntry(0, a.path, find(0, a.path) ?? { name: "", size: 1000 }, s.id);
        return { snapshotId: s.id, snapshotTime: s.completedAt, snapshotCount: k === 0 ? n - i : (arr[k - 1]! - i), size: e.size, modifiedMs: e.modifiedMs, hash: e.hash ?? "" };
      });
      return vs;
    }
    case "plan_restore": {
      const req = a.request as unknown as RestoreRequestDto;
      let files = 0, dirs = 0, bytes = 0;
      for (const it of req.items) {
        const node = find(it.source, it.path);
        if (!node) continue;
        const all: { path: string; node: Node }[] = [];
        if (node.children) { dirs++; walk(it.source, node.children, it.path, all); } else all.push({ path: it.path, node });
        for (const x of all) {
          if (x.node.children) dirs++;
          else { files++; bytes += x.node.size ?? 0; }
        }
      }
      const toOriginal = req.targetFolder === null;
      const plan: RestorePlan = {
        files, dirs, bytes: Math.round(bytes),
        targetRoot: toOriginal ? (SOURCES[req.items[0]?.source ?? 0]?.path ?? "") : `${req.targetFolder}`,
        conflicts: toOriginal
          ? req.items.filter((i) => !i.path.includes("Thesis")).slice(0, 2).map((i) => ({
              targetPath: `${SOURCES[i.source]?.path}\\${i.path.replaceAll("/", "\\")}`,
              kind: i.path.includes("Shopping") ? "identical" : "exists",
            }))
          : [],
      };
      return plan;
    }
    case "run_restore": {
      const req = a.request as unknown as RestoreRequestDto;
      return (async () => {
        const plan = handle("plan_restore", { request: req }) as RestorePlan;
        const real = plan.conflicts.filter((c) => c.kind === "exists");
        if (req.policy === "fail" && real.length) {
          const blocked: RestoreReport = { restored: 0, dirsCreated: 0, alreadyPresent: 0, skippedConflicts: 0, renamed: 0, overwritten: 0, bytes: 0, conflicts: real, failed: [] };
          return blocked;
        }
        await play("restore", plan.files, "restoring", 1500, `Restored ${plan.files} files (verified)`);
        const identical = plan.conflicts.length - real.length;
        const report: RestoreReport = {
          restored: plan.files - identical - (req.policy === "skip" ? real.length : 0),
          dirsCreated: plan.dirs,
          alreadyPresent: identical,
          skippedConflicts: req.policy === "skip" ? real.length : 0,
          renamed: req.policy === "rename" ? real.length : 0,
          overwritten: req.policy === "overwrite" ? real.length : 0,
          bytes: plan.bytes,
          conflicts: real,
          failed: [],
        };
        return report;
      })();
    }
    case "start_verify":
      void play("verify", 25, "verifying", 2500, `Verified ${snapshots.length} snapshots and 25 stored files`);
      return null;
    case "get_storage": {
      const s: StorageInfo = { path: DEST, repositoryBytes: Math.round(71.6 * MB), snapshotCount: snapshots.length, latestLogicalBytes: snapshots.at(-1)?.bytes ?? 0 };
      return s;
    }
    case "get_activity":
      return activity;
    default:
      throw { kind: "invalidRequest", message: `unknown command ${cmd}` };
  }
}
