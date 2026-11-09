import { open } from "@tauri-apps/plugin-dialog";
import { useEffect, useState } from "react";

import { Icon } from "../components/Icon";
import { Banner } from "../components/ui";
import { api, errorMessage } from "../ipc/api";
import type { DestinationInfo } from "../ipc/bindings/DestinationInfo";
import { formatWhen, plural } from "../lib/format";
import { useStore } from "../store";

type Step = 0 | 1 | 2 | 3;

export function Onboarding() {
  const { setStatus, toast } = useStore();
  const [step, setStep] = useState<Step>(0);
  const [sources, setSources] = useState<string[]>([]);
  const [dest, setDest] = useState<string | null>(null);
  const [info, setInfo] = useState<DestinationInfo | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!dest) return;
    let live = true;
    api.inspectDestination(dest, sources).then(
      (i) => live && setInfo(i),
      (e) => live && setError(errorMessage(e)),
    );
    return () => {
      live = false;
    };
  }, [dest, sources]);

  async function addFolders() {
    const picked = await open({ directory: true, multiple: true, title: "Choose folders to back up" });
    const list = picked === null ? [] : Array.isArray(picked) ? picked : [picked];
    setSources((s) => [...new Set([...s, ...list])]);
  }

  async function chooseDest() {
    const picked = await open({ directory: true, multiple: false, title: "Choose your backup drive or folder" });
    if (typeof picked === "string") {
      setInfo(null);
      setError(null);
      setDest(picked);
    }
  }

  async function finish() {
    if (!dest) return;
    setBusy(true);
    setError(null);
    try {
      const status = await api.configure(sources, dest);
      setStatus(status);
      await api.startBackup();
      toast({ tone: "info", title: "First backup started", body: "You can keep using your PC. K-92 works in the background." });
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setBusy(false);
    }
  }

  const destOk = info !== null && info.exists && info.writable && info.problem === null;

  return (
    <div className="onboard">
      <div className="card onboard-card" style={{ padding: 32 }}>
        <div className="steps" aria-hidden="true">
          {[0, 1, 2, 3].map((i) => <span key={i} className={i <= step ? "on" : ""} />)}
        </div>

        {step === 0 && (
          <div className="stack" style={{ gap: 16 }}>
            <div className="hero-icon info" style={{ width: 72, height: 72 }}><Icon name="timeline" size={36} /></div>
            <h1>Welcome to K-92</h1>
            <p className="muted" style={{ fontSize: 15, margin: 0 }}>
              Your PC. Your history. Your way back.
            </p>
            <p style={{ margin: 0 }}>
              K-92 keeps hourly copies of your files on an external drive. If you delete or overwrite something,
              pick a moment on the timeline and bring it back. Everything stays on your own drives: no account,
              no cloud.
            </p>
            <div className="row" style={{ marginTop: 8 }}>
              <span className="spacer" />
              <button className="btn btn-primary btn-lg" onClick={() => setStep(1)}>Get started</button>
            </div>
          </div>
        )}

        {step === 1 && (
          <div className="stack" style={{ gap: 14 }}>
            <h1>What should K-92 protect?</h1>
            <p className="muted" style={{ margin: 0 }}>Choose the folders that hold your files, such as Documents, Pictures or Desktop.</p>
            <div className="stack" style={{ gap: 8 }}>
              {sources.map((s) => (
                <div className="folder-row" key={s}>
                  <Icon name="folder" className="icon-folder" />
                  <span className="path" title={s}>{s}</span>
                  <button className="btn btn-ghost btn-sm" aria-label={`Remove ${s}`} onClick={() => setSources((all) => all.filter((x) => x !== s))}>
                    <Icon name="x" size={16} />
                  </button>
                </div>
              ))}
              <button className="btn" onClick={() => void addFolders()} style={{ alignSelf: "flex-start" }}>
                <Icon name="plus" size={16} /> Add folders
              </button>
            </div>
            <div className="row" style={{ marginTop: 8 }}>
              <button className="btn btn-ghost" onClick={() => setStep(0)}>Back</button>
              <span className="spacer" />
              <button className="btn btn-primary" disabled={sources.length === 0} onClick={() => setStep(2)}>Next</button>
            </div>
          </div>
        )}

        {step === 2 && (
          <div className="stack" style={{ gap: 14 }}>
            <h1>Where should backups go?</h1>
            <p className="muted" style={{ margin: 0 }}>
              Use an external drive. A backup on the same disk as your files will not survive that disk failing.
            </p>
            <button className="folder-row" onClick={() => void chooseDest()} style={{ cursor: "pointer", textAlign: "left" }}>
              <Icon name="drive" />
              <span className="path">{dest ?? "Choose a drive or folder…"}</span>
              <Icon name="chevron" size={16} />
            </button>
            {info?.hasRepository && (
              <Banner tone="info">
                <strong>Existing K-92 backup found.</strong> {plural(Number(info.snapshotCount ?? 0), "backup")}, last on{" "}
                {formatWhen(info.lastBackupAt)}. K-92 will keep using it, so your history carries over.
              </Banner>
            )}
            {info?.sameVolumeAsSource && !info.problem && (
              <Banner tone="warn">
                <strong>This is the same disk as your files.</strong> It protects against accidental deletion,
                but not against the disk failing. An external drive is strongly recommended.
              </Banner>
            )}
            {info?.problem && <Banner tone="bad">{info.problem}</Banner>}
            <div className="row" style={{ marginTop: 8 }}>
              <button className="btn btn-ghost" onClick={() => setStep(1)}>Back</button>
              <span className="spacer" />
              <button className="btn btn-primary" disabled={!destOk} onClick={() => setStep(3)}>Next</button>
            </div>
          </div>
        )}

        {step === 3 && dest && (
          <div className="stack" style={{ gap: 14 }}>
            <h1>Ready to protect your files</h1>
            <div className="card" style={{ boxShadow: "none", background: "var(--surface-2)" }}>
              <div className="stack" style={{ gap: 10 }}>
                <div className="row"><Icon name="folder" className="icon-folder" /><span>{plural(sources.length, "folder")} to back up</span></div>
                <div className="row"><Icon name="drive" /><span className="selectable" style={{ overflowWrap: "anywhere" }}>{dest}</span></div>
                <div className="row"><Icon name="timeline" /><span>Every hour while the drive is connected</span></div>
              </div>
            </div>
            <p className="small muted" style={{ margin: 0 }}>
              The first backup copies everything and may take a while. After that, only changes are saved.
            </p>
            {error && <Banner tone="bad">{error}</Banner>}
            <div className="row" style={{ marginTop: 8 }}>
              <button className="btn btn-ghost" onClick={() => setStep(2)} disabled={busy}>Back</button>
              <span className="spacer" />
              <button className="btn btn-primary btn-lg" onClick={() => void finish()} disabled={busy}>
                {busy ? "Setting up…" : "Start first backup"}
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
