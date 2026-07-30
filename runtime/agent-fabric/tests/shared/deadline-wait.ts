export class DeadlineTimeoutError extends Error {
  constructor(
    readonly description: string,
    readonly timeoutMs: number,
  ) {
    super(`${description} did not complete within ${String(timeoutMs)}ms`);
    this.name = "DeadlineTimeoutError";
  }
}

export async function waitUntil<T>(
  condition: () => Promise<T>,
  timeoutMs = 5_000,
  description = "Condition",
): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    const result = await condition();
    if (result) return result;
    if (Date.now() >= deadline) {
      throw new DeadlineTimeoutError(description, timeoutMs);
    }
    const remainingMs = deadline - Date.now();
    if (remainingMs > 0) {
      await new Promise<void>((resolve) => setTimeout(resolve, Math.min(10, remainingMs)));
    }
  }
}

export async function waitForProcessExit(
  pid: number,
  options: Readonly<{ timeoutMs?: number }> = {},
): Promise<void> {
  const timeoutMs = options.timeoutMs ?? 5_000;
  const description = `Process ${String(pid)} exit`;
  try {
    await waitUntil(async () => {
      try {
        process.kill(pid, 0);
        return false;
      } catch (error: unknown) {
        if ((error as NodeJS.ErrnoException).code === "ESRCH") return true;
        throw error;
      }
    }, timeoutMs, description);
  } catch (error: unknown) {
    if (error instanceof DeadlineTimeoutError) {
      throw new Error(`Process ${String(pid)} did not exit (waited ${String(timeoutMs)}ms)`);
    }
    throw error;
  }
}
