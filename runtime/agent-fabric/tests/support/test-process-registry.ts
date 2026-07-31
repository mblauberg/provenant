import { DeadlineTimeoutError, waitUntil } from "../shared/deadline-wait.ts";

const tracked = new Map<number, string>();
let exitHookInstalled = false;

function processExists(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

function installExitHook(): void {
  if (exitHookInstalled) return;
  exitHookInstalled = true;
  process.once("exit", () => {
    for (const pid of tracked.keys()) {
      try {
        process.kill(pid, "SIGKILL");
      } catch {
        // Already gone.
      }
    }
  });
  for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"] as const) {
    process.once(signal, () => {
      for (const pid of tracked.keys()) {
        try {
          process.kill(pid, "SIGKILL");
        } catch {
          // Already gone.
        }
      }
      process.exit(signal === "SIGINT" ? 130 : signal === "SIGTERM" ? 143 : 129);
    });
  }
}

export function trackTestProcess(pid: number, label: string): void {
  installExitHook();
  tracked.set(pid, label);
}

export function untrackTestProcess(pid: number): void {
  tracked.delete(pid);
}

export async function terminateTrackedTestProcess(pid: number): Promise<void> {
  if (!tracked.has(pid)) return;
  if (processExists(pid)) {
    try {
      process.kill(pid, "SIGTERM");
    } catch {
      // It exited between the probe and signal.
    }
    try {
      await waitUntil(() => !processExists(pid), 500, `Tracked process ${String(pid)} exit`);
    } catch (error: unknown) {
      // A process that outlives its grace window is escalated below, not reported.
      if (!(error instanceof DeadlineTimeoutError)) throw error;
    }
    if (processExists(pid)) {
      try {
        process.kill(pid, "SIGKILL");
      } catch {
        // It exited between the probe and signal.
      }
    }
  }
  tracked.delete(pid);
}
