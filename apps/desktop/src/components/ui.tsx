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

export function JobProgress({ progress }: { progress: ProgressEvent | null }) {
  if (!progress) return <Progress value={null} />;
  const frac = progress.filesTotal ? progress.filesDone / progress.filesTotal : null;
  return (
    <div className="stack" style={{ gap: 6 }}>
      <Progress value={frac} />
      <div className="row small muted">
        <span>{phaseLabel[progress.phase]}</span>
        <span className="faint">·</span>
        <span>
          {plural(Number(progress.filesDone), "file")}
          {progress.filesTotal !== null ? ` of ${Number(progress.filesTotal).toLocaleString()}` : ""}
        </span>
        {Number(progress.bytesDone) > 0 && <span className="faint">· {formatBytes(Number(progress.bytesDone))} read</span>}
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
