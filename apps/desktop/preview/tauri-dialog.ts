// Preview alias for "@tauri-apps/plugin-dialog": returns sample Windows paths.
export async function open(opts: { directory?: boolean; multiple?: boolean; title?: string }): Promise<string | string[] | null> {
  if (opts.multiple) return ["C:\\Users\\Ada\\Documents", "C:\\Users\\Ada\\Pictures"];
  return opts.title?.startsWith("Restore") ? "C:\\Users\\Ada\\Desktop\\Recovered" : "E:\\";
}
