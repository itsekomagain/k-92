// Typed wrappers around the Tauri commands in src-tauri/src/commands.rs.
// All data types come from ./bindings, generated from Rust by ts-rs (`npm run bindings`).

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type { ActivityEntry } from "./bindings/ActivityEntry";
import type { AppSettings } from "./bindings/AppSettings";
import type { AppStatus } from "./bindings/AppStatus";
import type { BrowseEntry } from "./bindings/BrowseEntry";
import type { DestinationInfo } from "./bindings/DestinationInfo";
import type { ErrorDto } from "./bindings/ErrorDto";
import type { FileVersion } from "./bindings/FileVersion";
import type { JobEvent } from "./bindings/JobEvent";
import type { JobKind } from "./bindings/JobKind";
import type { RestorePlan } from "./bindings/RestorePlan";
import type { RestoreReport } from "./bindings/RestoreReport";
import type { RestoreRequestDto } from "./bindings/RestoreRequestDto";
import type { SnapshotList } from "./bindings/SnapshotList";
import type { StorageInfo } from "./bindings/StorageInfo";

export class K92Error extends Error {
  readonly kind: ErrorDto["kind"];
  constructor(dto: ErrorDto) {
    super(dto.message);
    this.kind = dto.kind;
  }
}

function isErrorDto(e: unknown): e is ErrorDto {
  return typeof e === "object" && e !== null && "kind" in e && "message" in e;
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    if (isErrorDto(e)) throw new K92Error(e);
    throw new K92Error({ kind: "io", message: String(e) });
  }
}

export const api = {
  getStatus: () => call<AppStatus>("get_status"),
  inspectDestination: (path: string, sources: string[]) =>
    call<DestinationInfo>("inspect_destination", { path, sources }),
  configure: (sources: string[], destination: string) =>
    call<AppStatus>("configure", { sources, destination }),
  updateSettings: (settings: AppSettings) => call<AppStatus>("update_settings", { settings }),
  updateSources: (sources: string[]) => call<AppStatus>("update_sources", { sources }),
  startBackup: () => call<null>("start_backup"),
  cancelJob: (kind: JobKind | null) => call<null>("cancel_job", { kind }),
  listSnapshots: () => call<SnapshotList>("list_snapshots"),
  listDir: (snapshotId: string, source: number, path: string) =>
    call<BrowseEntry[]>("list_dir", { snapshotId, source, path }),
  search: (snapshotId: string, query: string) =>
    call<BrowseEntry[]>("search_snapshot", { snapshotId, query }),
  fileVersions: (sourcePath: string, path: string) =>
    call<FileVersion[]>("file_versions", { sourcePath, path }),
  planRestore: (request: RestoreRequestDto) => call<RestorePlan>("plan_restore", { request }),
  runRestore: (request: RestoreRequestDto) => call<RestoreReport>("run_restore", { request }),
  startVerify: (deep: boolean) => call<null>("start_verify", { deep }),
  getStorage: () => call<StorageInfo>("get_storage"),
  getActivity: () => call<ActivityEntry[]>("get_activity"),
};

export function onJobEvent(handler: (e: JobEvent) => void): Promise<UnlistenFn> {
  return listen<JobEvent>("k92://job", (ev) => handler(ev.payload));
}

export function errorMessage(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}
