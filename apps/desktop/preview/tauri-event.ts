// Preview alias for "@tauri-apps/api/event".
import type { JobEvent } from "../src/ipc/bindings/JobEvent";
import { subscribe } from "./mock-backend";

export type UnlistenFn = () => void;

export async function listen<T>(_event: string, handler: (e: { payload: T }) => void): Promise<UnlistenFn> {
  return subscribe((payload: JobEvent) => handler({ payload: payload as unknown as T }));
}
