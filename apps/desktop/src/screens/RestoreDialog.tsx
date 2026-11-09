import { open } from "@tauri-apps/plugin-dialog";
import { useState } from "react";

import { Icon } from "../components/Icon";
import { Banner, Dialog, JobProgress } from "../components/ui";
import { api, errorMessage } from "../ipc/api";
import type { ConflictPolicy } from "../ipc/bindings/ConflictPolicy";
import type { RestorePlan } from "../ipc/bindings/RestorePlan";
import type { RestoreReport } from "../ipc/bindings/RestoreReport";
import type { RestoreRequestDto } from "../ipc/bindings/RestoreRequestDto";
import type { SourceDto } from "../ipc/bindings/SourceDto";
import { formatBytes, formatWhen, plural } from "../lib/format";
import { useStore } from "../store";

export interface RestoreItem {
  source: number;
  path: string;
  name: string;
}

type Step = "where" | "review" | "running" | "done";

export function RestoreDialog({ snapshotId, snapshotTime, items, sources, onClose }: {
  snapshotId: string;
  snapshotTime: string;
  items: RestoreItem[];
  sources: SourceDto[];
  onClose: () => void;
}) {
  const { progress } = useStore();
  const [step, setStep] = useState<Step>("where");
  const [mode, setMode] = useState<"original" | "folder">("folder");
  const [folder, setFolder] = useState<string | null>(null);
  const [plan, setPlan] = useState<RestorePlan | null>(null);
  const [policy, setPolicy] = useState<ConflictPolicy>("rename");
  const [report, setReport] = useState<RestoreReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const request = (p: ConflictPolicy): RestoreRequestDto => ({
    snapshotId,
    items: items.map(({ source, path }) => ({ source, path })),
    targetFolder: mode === "folder" ? folder : null,
    policy: p,
  });

  const originals = [...new Set(items.map((i) => sources.find((s) => s.id === i.source)?.path ?? "?"))];
  const realConflicts = plan?.conflicts.filter((c) => c.kind !== "identical") ?? [];
  const identical = (plan?.conflicts.length ?? 0) - realConflicts.length;

  async function review() {
    setBusy(true);
    setError(null);
    try {
      setPlan(await api.planRestore(request("fail")));
      setStep("review");
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  async function run() {
    setStep("running");
    setError(null);
    try {
      setReport(await api.runRestore(request(realConflicts.length > 0 ? policy : "fail")));
      setStep("done");
    } catch (e) {
      setError(errorMessage(e));
      setStep("review");
    }
  }

  async function pickFolder() {
    const p = await open({ directory: true, multiple: false, title: "Restore into…" });
    if (typeof p === "string") setFolder(p);
  }

  const what = items.length === 1 ? `“${items[0]?.name || "everything"}”` : plural(items.length, "item");

  let footer;
  if (step === "where") {
    footer = (
      <>
        <button className="btn" onClick={onClose}>Cancel</button>
        <button className="btn btn-primary" disabled={busy || (mode === "folder" && !folder)} onClick={() => void review()}>
          {busy ? "Checking…" : "Next"}
        </button>
      </>
    );
  } else if (step === "review") {
    footer = (
      <>
        <button className="btn" onClick={() => setStep("where")}>Back</button>
        <button className="btn btn-primary" onClick={() => void run()}>
          {plan && plan.files > 0 ? `Restore ${plural(plan.files, "file")}` : "Restore"}
        </button>
      </>
    );
  } else if (step === "running") {
    footer = <button className="btn" onClick={() => void api.cancelJob("restore")}>Stop</button>;
  } else {
    footer = <button className="btn btn-primary" onClick={onClose}>Done</button>;
  }

  return (
    <Dialog title={`Restore ${what}`} onClose={step === "running" ? () => {} : onClose} footer={footer}>
      <p className="muted" style={{ marginTop: -6 }}>From the backup of {formatWhen(snapshotTime)}.</p>

      {step === "where" && (
        <div className="stack" style={{ gap: 10 }}>
          <label className="choice">
            <input type="radio" name="target" checked={mode === "folder"} onChange={() => setMode("folder")} />
            <div style={{ flex: 1, minWidth: 0 }}>
              <div style={{ fontWeight: 600 }}>To a folder I choose</div>
              <div className="small muted">Safest. Your current files are not touched.</div>
              {mode === "folder" && (
                <button className="btn btn-sm" style={{ marginTop: 8 }} onClick={() => void pickFolder()}>
                  <Icon name="folder" size={16} /> {folder ?? "Choose folder…"}
                </button>
              )}
            </div>
          </label>
          <label className="choice">
            <input type="radio" name="target" checked={mode === "original"} onChange={() => setMode("original")} />
            <div style={{ minWidth: 0 }}>
              <div style={{ fontWeight: 600 }}>To the original location</div>
              <div className="small muted" style={{ overflowWrap: "anywhere" }}>{originals.join(", ")}</div>
              <div className="small muted">You'll be asked before any existing file is changed.</div>
            </div>
          </label>
        </div>
      )}

      {step === "review" && plan && (
        <div className="stack">
          <div className="card" style={{ boxShadow: "none", background: "var(--surface-2)" }}>
            <div>{plural(Number(plan.files), "file")}{Number(plan.dirs) > 0 ? ` in ${plural(Number(plan.dirs), "folder")}` : ""} · {formatBytes(Number(plan.bytes))}</div>
            <div className="small muted" style={{ overflowWrap: "anywhere" }}>Into {plan.targetRoot}</div>
          </div>
          {identical > 0 && <div className="small muted">{plural(identical, "file")} already match the backup and will be left as they are.</div>}
          {realConflicts.length > 0 && (
            <>
              <Banner tone="warn">
                <strong>{plural(realConflicts.length, "file")} already exist{realConflicts.length === 1 ? "s" : ""} with different content.</strong>
                <div className="small" style={{ marginTop: 4 }}>
                  {realConflicts.slice(0, 4).map((c) => <div key={c.targetPath} style={{ overflowWrap: "anywhere" }}>{c.targetPath}</div>)}
                  {realConflicts.length > 4 && <div>and {realConflicts.length - 4} more</div>}
                </div>
              </Banner>
              <div className="stack" style={{ gap: 8 }} role="radiogroup" aria-label="When a file already exists">
                {([
                  ["rename", "Keep both", "Restored copies get “(restored …)” added to their name."],
                  ["skip", "Keep my current files", "Only restore files that are missing."],
                  ["overwrite", "Replace with the backup", "Current versions of these files will be replaced."],
                ] as const).map(([value, title, desc]) => (
                  <label className="choice" key={value}>
                    <input type="radio" name="policy" checked={policy === value} onChange={() => setPolicy(value)} />
                    <div>
                      <div style={{ fontWeight: 600, color: value === "overwrite" ? "var(--bad)" : undefined }}>{title}</div>
                      <div className="small muted">{desc}</div>
                    </div>
                  </label>
                ))}
              </div>
            </>
          )}
        </div>
      )}

      {step === "running" && <JobProgress progress={progress} />}

      {step === "done" && report && (
        <div className="stack">
          {report.failed.length === 0 ? (
            <div className="row">
              <span className="hero-icon ok" style={{ width: 44, height: 44, borderRadius: 12 }}><Icon name="check" /></span>
              <div>
                <div style={{ fontWeight: 600 }}>Restored {plural(Number(report.restored), "file")}</div>
                <div className="small muted">Every file was checked against the backup after writing.</div>
              </div>
            </div>
          ) : (
            <Banner tone="bad">
              <strong>{plural(report.failed.length, "file")} could not be restored.</strong>
              <div className="small" style={{ marginTop: 4 }}>
                {report.failed.slice(0, 5).map((f) => <div key={f.path} style={{ overflowWrap: "anywhere" }}>{f.path}: {f.message}</div>)}
              </div>
            </Banner>
          )}
          <div className="small muted">
            {[
              Number(report.alreadyPresent) > 0 && `${plural(Number(report.alreadyPresent), "file")} already up to date`,
              Number(report.skippedConflicts) > 0 && `${plural(Number(report.skippedConflicts), "file")} kept as they were`,
              Number(report.renamed) > 0 && `${plural(Number(report.renamed), "copy", "copies")} saved with a new name`,
              Number(report.overwritten) > 0 && `${plural(Number(report.overwritten), "file")} replaced`,
            ].filter(Boolean).join(" · ")}
          </div>
        </div>
      )}

      {error && <div style={{ marginTop: 12 }}><Banner tone="bad">{error}</Banner></div>}
    </Dialog>
  );
}
