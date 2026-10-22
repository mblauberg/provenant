import { existsSync, watch, type FSWatcher } from "node:fs";
import { basename } from "node:path";

/** Status and receipt writes, pointers and new run directories; not the streaming logs. */
const RELEVANT = /\.json$|^result\.md$|^\d{8}-\d{4}-/u;
const SETTLE_MS = 300;

/**
 * Sleep between lane reads until a run or state file changes, with a slow
 * timer as the fallback for changes no file records (a process that died).
 * `dirs` is re-read on every idle so a project registered later is watched.
 */
export function laneIdle(dirs: () => string[]) {
  const watchers = new Map<string, FSWatcher>();
  let changed = false;
  let wake: (() => void) | undefined;
  const ensure = () => {
    for (const dir of dirs()) {
      if (watchers.has(dir) || !existsSync(dir)) continue;
      try {
        const watcher = watch(dir, { recursive: true }, (_event, name) => {
          if (name && !RELEVANT.test(basename(String(name)))) return;
          changed = true;
          wake?.();
        });
        // A watcher that fails leaves the fallback timer to carry the wait.
        watcher.on("error", () => { watcher.close(); watchers.delete(dir); });
        watchers.set(dir, watcher);
      } catch { /* Unwatchable here: the fallback timer covers it. */ }
    }
  };
  return {
    async idle(ms: number): Promise<void> {
      ensure();
      if (!changed) {
        await new Promise<void>((done) => {
          const timer = setTimeout(done, ms);
          wake = () => { clearTimeout(timer); done(); };
        });
        wake = undefined;
      }
      if (changed) {
        // Let a burst of writes finish before the read.
        await new Promise((done) => setTimeout(done, SETTLE_MS));
        changed = false;
      }
    },
    close(): void {
      for (const watcher of watchers.values()) watcher.close();
      watchers.clear();
    },
  };
}
