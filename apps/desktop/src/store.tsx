// App-wide state: backend status, live job progress and toasts. The backend is the source of
// truth; this store only caches what `get_status` and job events report.

import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from "react";

import { api, errorMessage, onJobEvent } from "./ipc/api";
import type { AppStatus } from "./ipc/bindings/AppStatus";
import type { JobEvent } from "./ipc/bindings/JobEvent";
import type { ProgressEvent } from "./ipc/bindings/ProgressEvent";

export interface Toast {
  id: number;
  tone: "ok" | "warn" | "bad" | "info";
  title: string;
  body?: string;
}

interface Store {
  status: AppStatus | null;
  loadError: string | null;
  progress: ProgressEvent | null;
  /** Increments whenever a job finishes, so screens can reload their data. */
  generation: number;
  refresh: () => Promise<void>;
  setStatus: (s: AppStatus) => void;
  toast: (t: Omit<Toast, "id">) => void;
  toasts: Toast[];
  dismiss: (id: number) => void;
}

const Ctx = createContext<Store | null>(null);

export function useStore(): Store {
  const s = useContext(Ctx);
  if (!s) throw new Error("useStore outside provider");
  return s;
}

const outcomeTone = { success: "ok", warning: "warn", failed: "bad", cancelled: "info" } as const;
const jobTitle = { backup: "Backup", restore: "Restore", verify: "Verification", prune: "Cleanup" } as const;

export function StoreProvider({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<AppStatus | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [progress, setProgress] = useState<ProgressEvent | null>(null);
  const [generation, setGeneration] = useState(0);
  const [toasts, setToasts] = useState<Toast[]>([]);
  const nextId = useRef(1);

  const refresh = useCallback(async () => {
    try {
      setStatus(await api.getStatus());
      setLoadError(null);
    } catch (e) {
      setLoadError(errorMessage(e));
    }
  }, []);

  const toast = useCallback((t: Omit<Toast, "id">) => {
    const id = nextId.current++;
    setToasts((all) => [...all.slice(-3), { ...t, id }]);
    window.setTimeout(() => setToasts((all) => all.filter((x) => x.id !== id)), 7000);
  }, []);

  const dismiss = useCallback((id: number) => setToasts((all) => all.filter((x) => x.id !== id)), []);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), 5000);
    const unlisten = onJobEvent((e: JobEvent) => {
      if (e.type === "started") {
        setProgress(null);
        void refresh();
      } else if (e.type === "progress") {
        setProgress(e.progress);
      } else {
        setProgress(null);
        setGeneration((g) => g + 1);
        void refresh();
        if (e.kind !== "restore") {
          toast({ tone: outcomeTone[e.outcome], title: `${jobTitle[e.kind]} ${e.outcome === "success" ? "complete" : e.outcome}`, body: e.summary });
        }
      }
    });
    return () => {
      window.clearInterval(timer);
      void unlisten.then((f) => f());
    };
  }, [refresh, toast]);

  const value = useMemo(
    () => ({ status, loadError, progress, generation, refresh, setStatus, toast, toasts, dismiss }),
    [status, loadError, progress, generation, refresh, toast, toasts, dismiss],
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}
