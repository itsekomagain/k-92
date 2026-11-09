// Builds the browser preview of the desktop UI with a mocked backend (`npm run preview:build`).
import { fileURLToPath } from "node:url";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

const here = (p: string) => fileURLToPath(new URL(p, import.meta.url));

export default defineConfig({
  root: here("."),
  plugins: [react()],
  resolve: {
    alias: {
      "@tauri-apps/api/core": here("./tauri-core.ts"),
      "@tauri-apps/api/event": here("./tauri-event.ts"),
      "@tauri-apps/plugin-dialog": here("./tauri-dialog.ts"),
    },
  },
  build: { outDir: here("../dist-preview"), emptyOutDir: true, assetsInlineLimit: 100_000_000, cssCodeSplit: false, modulePreload: false },
});
