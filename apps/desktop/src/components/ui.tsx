import { useEffect, useRef, type ReactNode } from "react";

import type { ProgressEvent } from "../ipc/bindings/ProgressEvent";
import { formatBytes, plural } from "../lib/format";
import { Icon, type IconName } from "./Icon";

export function Progress({ value }: { value: number | null }) {
  return (
    <div
      className={`progress${value === null ? " indeterminate" : ""}`}
      role="progressbar"
      aria-valuemin={0}
      aria-valuemax={100}
      {...(value !== null ? { "aria-valuenow": Math.round(value * 100) } : {})}
    >
      <div style={value === null ? undefined : { width: `${Math.max(2, value * 100)}%` }} />
    </div>
  );
}

const phaseLabel: Record<ProgressEvent["phase"], string> = {
  preparing: "Preparing…",
  scanning: "Looking for changes…",
  copying: "Backing up",
  committing: "Saving backup…",
  restoring: "Restoring",
  verifying: "Verifying",
};

/** Estimates time left from the copy rate, once there is enough data to be meaningful. */
function useTimeLeft(progress: ProgressEvent | null): number | null {
  const first = useRef<{ t: number; bytes: number } | null>(null);
  if (!progress || progress.phase !== "copying" || !progress.bytesTotal) {
    first.current = null;
    return null;
  }
  const now = Date.now();
  const done = Number(progress.bytesDone);
  if (!first.current || done < first.current.bytes) first.current = { t: now, bytes: done };
  const secs = (now - first.current.t) / 1000;
  const rate = (done - first.current.bytes) / Math.max(secs, 1e-3);
  if (secs < 15 || rate <= 0) return null;
  return Math.max(0, (Number(progress.bytesTotal) - done) / rate);
}

function formatDuration(s: number): string {
  if (s < 90) return "about a minute";
  const m = Math.round(s / 60);
  if (m < 60) return `about ${m} minutes`;
  const h = Math.floor(m / 60);
  const rest = m % 60;
  return `about ${h} h${rest >= 5 ? ` ${rest} min` : ""}`;
}

export function JobProgress({ progress }: { progress: ProgressEvent | null }) {
  const left = useTimeLeft(progress);
  if (!progress) return <Progress value={null} />;
  const bytesTotal = progress.bytesTotal === null ? null : Number(progress.bytesTotal);
  const bytesDone = Number(progress.bytesDone);
  // Bytes drive the bar when known (a 4 GB video should count for more than a text file).
  const frac =
    bytesTotal !== null && bytesTotal > 0 && progress.phase === "copying"
      ? Math.min(1, bytesDone / bytesTotal)
      : progress.filesTotal
        ? progress.filesDone / progress.filesTotal
        : null;
  const scanning = progress.phase === "scanning";
  return (
    <div className="stack" style={{ gap: 6 }}>
      <Progress value={frac} />
      <div className="row small muted" style={{ flexWrap: "wrap" }}>
        <span>{phaseLabel[progress.phase]}</span>
        <span className="faint">·</span>
        <span>
          {scanning
            ? `${plural(Number(progress.filesDone), "file")} found`
            : plural(Number(progress.filesDone), "file")}
          {!scanning && progress.filesTotal !== null ? ` of ${Number(progress.filesTotal).toLocaleString()}` : ""}
        </span>
        {bytesTotal !== null && bytesTotal > 0 ? (
          <span className="faint">
            · {formatBytes(bytesDone)} of {formatBytes(bytesTotal)} to copy
          </span>
        ) : (
          bytesDone > 0 && <span className="faint">· {formatBytes(bytesDone)} read</span>
        )}
        {left !== null && <span className="faint">· {formatDuration(left)} left</span>}
      </div>
      {progress.currentPath && (
        <div className="small faint" style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
          {progress.currentPath}
        </div>
      )}
    </div>
  );
}

export function Banner({ tone, icon, children }: { tone: "warn" | "bad" | "info"; icon?: IconName; children: ReactNode }) {
  return (
    <div className={`banner banner-${tone}`} role={tone === "bad" ? "alert" : "status"}>
      <Icon name={icon ?? (tone === "info" ? "info" : "alert")} />
      <div style={{ flex: 1 }}>{children}</div>
    </div>
  );
}

export function Empty({ icon, title, children }: { icon: IconName; title: string; children?: ReactNode }) {
  return (
    <div className="empty">
      <Icon name={icon} size={40} />
      <div style={{ fontWeight: 600, color: "var(--text)" }}>{title}</div>
      {children && <div className="small" style={{ marginTop: 4 }}>{children}</div>}
    </div>
  );
}

/** Modal dialog with focus management and Escape to close. */
export function Dialog({ title, onClose, children, footer, labelledBy = "dialog-title" }: {
  title: string;
  onClose: () => void;
  children: ReactNode;
  footer: ReactNode;
  labelledBy?: string;
}) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null;
    ref.current?.querySelector<HTMLElement>("button, input, select")?.focus();
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => {
      window.removeEventListener("keydown", onKey);
      prev?.focus();
    };
  }, [onClose]);
  return (
    <div className="scrim" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="dialog" role="dialog" aria-modal="true" aria-labelledby={labelledBy} ref={ref}>
        <div className="dialog-body">
          <h2 id={labelledBy} style={{ fontSize: 20, marginBottom: 14 }}>{title}</h2>
          {children}
        </div>
        <div className="dialog-foot">{footer}</div>
      </div>
    </div>
  );
}
