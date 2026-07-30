export async function waitUntil<T>(
  condition: () => Promise<T>,
  timeoutMs = 5_000,
  description = "Condition",
): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const result = await condition();
    if (result) return result;
    const remainingMs = deadline - Date.now();
    if (remainingMs > 0) {
      await new Promise<void>((resolve) => setTimeout(resolve, Math.min(10, remainingMs)));
    }
  }
  throw new Error(`${description} did not complete within ${String(timeoutMs)}ms`);
}

export async function waitForFile(
  filePath: string,
  options: Readonly<{ timeoutMs?: number }> = {},
): Promise<void> {
  const timeoutMs = options.timeoutMs ?? 5_000;
  const description = `File ${filePath}`;
  try {
    await waitUntil(async () => {
      try {
        await access(filePath);
        return true;
      } catch (error: unknown) {
        if ((error as NodeJS.ErrnoException).code === "ENOENT") return false;
        throw error;
      }
    }, timeoutMs, description);
  } catch (error: unknown) {
    if (
      error instanceof Error &&
      error.message === `${description} did not complete within ${String(timeoutMs)}ms`
    ) {
      throw new Error(`File did not appear: ${filePath} (waited ${String(timeoutMs)}ms)`);
    }
    throw error;
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
    if (
      error instanceof Error &&
      error.message === `${description} did not complete within ${String(timeoutMs)}ms`
    ) {
      throw new Error(`Process ${String(pid)} did not exit (waited ${String(timeoutMs)}ms)`);
    }
    throw error;
  }
}
import { access } from "node:fs/promises";
