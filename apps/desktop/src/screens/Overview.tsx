import { useEffect, useState } from "react";

import { Icon } from "../components/Icon";
import { Banner, JobProgress } from "../components/ui";
import { api, errorMessage } from "../ipc/api";
import type { SnapshotSummary } from "../ipc/bindings/SnapshotSummary";
import { baseName, formatBytes, formatRelative, formatShortDay, formatTime, formatWhen, plural } from "../lib/format";
import { useStore } from "../store";
import type { Screen } from "../App";

export function Overview({ go }: { go: (s: Screen) => void }) {
  const { status, progress, generation, toast } = useStore();
  const [recent, setRecent] = useState<SnapshotSummary[]>([]);

  useEffect(() => {
    if (!status?.destinationAvailable) return;
    api.listSnapshots().then((l) => setRecent(l.snapshots.slice(-7).reverse()), () => setRecent([]));
  }, [generation, status?.destinationAvailable]);

  if (!status) return null;
  const running = status.running?.kind === "backup" ? status.running : null;
  const last = status.lastSnapshot;

  let tone: "ok" | "warn" | "bad" | "info" = "ok";
  let title = "Your files are protected";
  let detail = last ? `Last backup ${formatWhen(last.completedAt)}` : "";
  if (running) {
    tone = "info";
    title = "Backing up…";
    detail = "You can keep working. Only changed files are copied.";
  } else if (!status.destinationAvailable) {
    tone = "warn";
    title = "Backup drive not connected";
    detail = "Connect the drive and K-92 will continue automatically.";
  } else if (status.lastError) {
    tone = "bad";
    title = "The last backup did not finish";
    detail = status.lastError.message;
  } else if (!last) {
    tone = "info";
    title = "No backups yet";
    detail = "Run your first backup to start your history.";
  }

  async function backupNow() {
    try {
      await api.startBackup();
    } catch (e) {
      toast({ tone: "bad", title: "Could not start backup", body: errorMessage(e) });
    }
  }

  return (
    <div className="page">
      <div className="page-head">
        <h1>Overview</h1>
      </div>

      <div className="card hero" style={{ marginBottom: 16 }}>
        <div className={`hero-icon ${tone}`}>
          <Icon name={tone === "ok" ? "shield" : tone === "info" ? "timeline" : "shieldAlert"} size={32} />
        </div>
        <div style={{ flex: 1, minWidth: 0 }}>
          <div className="hero-title">{title}</div>
          <div className="muted" style={{ marginTop: 2 }}>{detail}</div>
          {running && <div style={{ marginTop: 12, maxWidth: 520 }}><JobProgress progress={progress ?? running.progress} /></div>}
        </div>
        {running ? (
          <button className="btn" onClick={() => void api.cancelJob("backup")}>Stop</button>
        ) : (
          <button className="btn btn-primary" onClick={() => void backupNow()} disabled={!status.destinationAvailable}>
            Back up now
          </button>
        )}
      </div>

      {status.destinationError && status.destinationError.kind !== "notFound" && (
        <div style={{ marginBottom: 16 }}>
          <Banner tone="bad">{status.destinationError.message}</Banner>
        </div>
      )}

      <div className="grid grid-3" style={{ marginBottom: 16 }}>
        <div className="card">
          <div className="stat-label">Last backup</div>
          <div className="stat-value">{last ? formatWhen(last.completedAt) : "—"}</div>
          {last && <span className="pill pill-ok" style={{ marginTop: 8 }}><span className="dot" />Completed</span>}
        </div>
        <div className="card">
          <div className="stat-label">Next backup</div>
          <div className="stat-value">
            {status.settings.paused ? "Paused" : status.nextBackupAt ? formatRelative(status.nextBackupAt) : "—"}
          </div>
          <div className="small muted" style={{ marginTop: 8 }}>
            {status.settings.intervalMinutes > 0 ? `Every ${plural(status.settings.intervalMinutes, "minute")}` : "Manual only"}
          </div>
        </div>
        <div className="card">
          <div className="stat-label">Protected data</div>
          <div className="stat-value">{last ? formatBytes(Number(last.bytes)) : "—"}</div>
          <div className="small muted" style={{ marginTop: 8 }}>
            {last ? `${plural(Number(last.files), "file")} · ${plural(Number(status.snapshotCount), "backup")}` : ""}
          </div>
        </div>
      </div>

      <div className="grid grid-2">
        <div className="card">
          <div className="card-head">
            <h2>Backup history</h2>
            <button className="btn btn-sm" onClick={() => go("timeline")}>Explore timeline</button>
          </div>
          {recent.length === 0 ? (
            <p className="muted small">Select a backup point to explore your files once your first backup finishes.</p>
          ) : (
            <div className="list">
              {recent.map((s) => (
                <div className="list-row" key={s.id}>
                  <span className="pill pill-ok" style={{ padding: 4 }}><Icon name="check" size={14} /></span>
                  <span style={{ fontWeight: 600, minWidth: 64 }}>{formatShortDay(new Date(s.completedAt))}</span>
                  <span className="muted">{formatTime(new Date(s.completedAt))}</span>
                  <span className="spacer" />
                  <span className="small faint">{Number(s.newBytes) > 0 ? `+${formatBytes(Number(s.newBytes))}` : "no changes"}</span>
                </div>
              ))}
            </div>
          )}
        </div>
        <div className="card">
          <div className="card-head"><h2>What's protected</h2>
            <button className="btn btn-sm" onClick={() => go("settings")}>Change</button>
          </div>
          <div className="list">
            {status.sources.map((s) => (
              <div className="list-row" key={s}>
                <Icon name="folder" className="icon-folder" />
                <div style={{ minWidth: 0 }}>
                  <div style={{ fontWeight: 600 }}>{baseName(s)}</div>
                  <div className="small faint" style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{s}</div>
                </div>
              </div>
            ))}
            <div className="list-row">
              <Icon name="drive" />
              <div style={{ minWidth: 0 }}>
                <div style={{ fontWeight: 600 }}>Backup drive</div>
                <div className="small faint" style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{status.destination}</div>
              </div>
              <span className="spacer" />
              <span className={`pill ${status.destinationAvailable ? "pill-ok" : "pill-warn"}`}>
                <span className="dot" />{status.destinationAvailable ? "Connected" : "Not connected"}
              </span>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
