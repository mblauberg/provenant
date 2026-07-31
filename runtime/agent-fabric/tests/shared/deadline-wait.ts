import { readFile } from "node:fs/promises";

/**
 * The one wait port for this package's tests.
 *
 * `now` is elapsed-time-only: callers may subtract two readings, and may not
 * interpret a reading as a civil time. The default is `performance.now`, which
 * is monotonic and so cannot be dragged backwards by a clock adjustment mid
 * wait. Production civil-time `Clock` ports (`src/domain/types.ts`) are a
 * different concept and deliberately stay separate: persistence and expiry need
 * wall time, waiting needs monotonic time.
 */
export type WaitClock = Readonly<{
  now: () => number;
  sleep: (milliseconds: number) => Promise<void>;
}>;

export type WaitOptions = Readonly<{
  clock?: WaitClock;
  pollIntervalMs?: number;
}>;

export const monotonicWaitClock: WaitClock = {
  now: () => performance.now(),
  sleep: (milliseconds) =>
    new Promise<void>((resolve) => setTimeout(resolve, milliseconds)),
};

const DEFAULT_POLL_INTERVAL_MS = 10;

export class DeadlineTimeoutError extends Error {
  constructor(
    readonly description: string,
    readonly timeoutMs: number,
  ) {
    super(`${description} did not complete within ${String(timeoutMs)}ms`);
    this.name = "DeadlineTimeoutError";
  }
}

/**
 * The sole waiter. Every other wait helper in this package delegates here, so
 * that polling cadence, the deadline decision and the timeout diagnostic have
 * exactly one definition.
 *
 * The condition is always evaluated at least once, including at `timeoutMs` 0.
 *
 * The runtime test is **truthiness**, so a condition may signal "not yet" with
 * any falsy value. The declared `NonNullable<T>` therefore states less than the
 * check enforces: it strips only `null` and `undefined`. A caller must not ask
 * this to return a falsy value, because it would be indistinguishable from
 * "not yet" and would wait out the deadline instead.
 */
export async function waitUntil<T>(
  condition: () => Promise<T> | T,
  timeoutMs = 5_000,
  description = "Condition",
  options: WaitOptions = {},
): Promise<NonNullable<T>> {
  const clock = options.clock ?? monotonicWaitClock;
  const pollIntervalMs = options.pollIntervalMs ?? DEFAULT_POLL_INTERVAL_MS;
  const startedAt = clock.now();
  for (;;) {
    const result = await condition();
    if (result) return result;
    const remainingMs = timeoutMs - (clock.now() - startedAt);
    if (remainingMs <= 0) throw new DeadlineTimeoutError(description, timeoutMs);
    await clock.sleep(Math.min(pollIntervalMs, remainingMs));
  }
}

/**
 * Retry an assertion until it stops throwing.
 *
 * On timeout this rethrows the *last assertion failure*, not the deadline error,
 * because the assertion's own diff is the useful diagnostic. The deadline error
 * is attached as `cause` so the waited-for duration stays recoverable.
 */
export async function eventually(
  assertion: () => Promise<void> | void,
  timeoutMs = 8_000,
  description = "Assertion",
  options: WaitOptions = {},
): Promise<void> {
  let lastFailure: unknown;
  let everFailed = false;
  try {
    await waitUntil(async () => {
      try {
        await assertion();
        return true;
      } catch (error: unknown) {
        lastFailure = error;
        everFailed = true;
        return false;
      }
    }, timeoutMs, description, options);
  } catch (error: unknown) {
    if (!(error instanceof DeadlineTimeoutError) || !everFailed) throw error;
    if (lastFailure instanceof Error && lastFailure.cause === undefined) {
      lastFailure.cause = error;
    }
    throw lastFailure;
  }
}

/**
 * A deliberate quiescence window: hold still for a fixed duration to prove that
 * something does *not* happen, or to let a fixture reach a state that publishes
 * no signal worth polling.
 *
 * This is not a waiter and must never stand in for one. The mandatory `reason`
 * keeps every such window greppable and justified.
 */
export async function settle(
  milliseconds: number,
  reason: string,
  options: WaitOptions = {},
): Promise<void> {
  if (reason.trim().length === 0) throw new Error("settle requires a reason");
  await (options.clock ?? monotonicWaitClock).sleep(milliseconds);
}

export async function waitForProcessExit(
  pid: number,
  options: WaitOptions & Readonly<{ timeoutMs?: number }> = {},
): Promise<void> {
  const timeoutMs = options.timeoutMs ?? 5_000;
  const description = `Process ${String(pid)} exit`;
  try {
    await waitUntil(() => {
      try {
        process.kill(pid, 0);
        return false;
      } catch (error: unknown) {
        if ((error as NodeJS.ErrnoException).code === "ESRCH") return true;
        throw error;
      }
    }, timeoutMs, description, options);
  } catch (error: unknown) {
    if (error instanceof DeadlineTimeoutError) {
      throw new Error(
        `Process ${String(pid)} did not exit (waited ${String(timeoutMs)}ms)`,
        { cause: error },
      );
    }
    throw error;
  }
}

/**
 * Resolve with a file's contents once it exists.
 *
 * The contents are carried in a wrapper so that an existing but empty file ends
 * the wait, exactly as a direct read would.
 */
export async function waitForFile(
  path: string,
  options: WaitOptions & Readonly<{ timeoutMs?: number }> = {},
): Promise<string> {
  const found = await waitUntil(async () => {
    try {
      return { contents: await readFile(path, "utf8") };
    } catch (error: unknown) {
      if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
      return undefined;
    }
  }, options.timeoutMs ?? 5_000, `File ${path}`, options);
  return found.contents;
}
