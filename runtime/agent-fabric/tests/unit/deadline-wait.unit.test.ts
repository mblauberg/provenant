import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { afterEach, describe, expect, it, vi } from "vitest";

import {
  DeadlineTimeoutError,
  eventually,
  monotonicWaitClock,
  settle,
  waitForFile,
  waitForProcessExit,
  waitUntil,
  type WaitClock,
} from "../shared/deadline-wait.ts";

/**
 * A deterministic wait port. Sleeping advances the clock, so a whole wait
 * resolves fast while still exercising the real elapsed-time arithmetic.
 * Nothing here depends on how many times the clock is read, which is the
 * coupling the previous `Date.now` spies had.
 */
function testClock(): WaitClock & {
  readonly sleeps: readonly number[];
  elapsedMs: () => number;
  advance: (milliseconds: number) => void;
} {
  let elapsed = 0;
  const sleeps: number[] = [];
  return {
    now: () => elapsed,
    sleep: (milliseconds) => {
      sleeps.push(milliseconds);
      elapsed += milliseconds;
      return Promise.resolve();
    },
    sleeps,
    elapsedMs: () => elapsed,
    advance: (milliseconds) => { elapsed += milliseconds; },
  };
}

/** The walk `src/cli/status.ts` performs over a rejected probe. */
function causeChain(error: unknown): string[] {
  const messages: string[] = [];
  const seen = new Set<Error>();
  let current = error;
  while (current instanceof Error && !seen.has(current)) {
    seen.add(current);
    messages.push(current.message);
    current = current.cause;
  }
  return messages;
}

const temporaryRoots: string[] = [];

afterEach(async () => {
  await Promise.all(temporaryRoots.splice(0).map((root) => rm(root, { recursive: true, force: true })));
});

async function temporaryRoot(): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), "deadline-wait-"));
  temporaryRoots.push(root);
  return root;
}

describe("waitUntil", () => {
  it("returns the first truthy condition result", async () => {
    const clock = testClock();
    let attempts = 0;
    await expect(waitUntil(() => {
      attempts += 1;
      return attempts === 2 ? "ready" : "";
    }, 100, "Test condition", { clock })).resolves.toBe("ready");
    expect(attempts).toBe(2);
    expect(clock.sleeps).toEqual([10]);
  });

  it("checks a true condition once when the timeout is zero", async () => {
    const clock = testClock();
    let attempts = 0;
    await expect(waitUntil(() => {
      attempts += 1;
      return "ready";
    }, 0, "Test condition", { clock })).resolves.toBe("ready");
    expect(attempts).toBe(1);
    expect(clock.sleeps).toEqual([]);
  });

  it("checks a false condition once and never sleeps when the timeout is zero", async () => {
    const clock = testClock();
    let attempts = 0;
    await expect(waitUntil(() => {
      attempts += 1;
      return false;
    }, 0, "Test condition", { clock })).rejects.toBeInstanceOf(DeadlineTimeoutError);
    expect(attempts).toBe(1);
    expect(clock.sleeps).toEqual([]);
  });

  it("returns a condition that becomes true exactly at the deadline", async () => {
    const clock = testClock();
    let attempts = 0;
    let observedAt = -1;
    await expect(waitUntil(() => {
      attempts += 1;
      observedAt = clock.now();
      return clock.now() === 20 ? "ready" : "";
    }, 20, "Test condition", { clock })).resolves.toBe("ready");
    expect(attempts).toBe(3);
    expect(observedAt).toBe(20);
    expect(clock.sleeps).toEqual([10, 10]);
  });

  it("returns a condition whose result arrives after the deadline has passed", async () => {
    const clock = testClock();
    let attempts = 0;
    await expect(waitUntil(() => {
      attempts += 1;
      if (attempts === 1) return "";
      clock.advance(500); // The condition itself outran the budget.
      return "ready";
    }, 20, "Test condition", { clock })).resolves.toBe("ready");
    expect(attempts).toBe(2);
    expect(clock.elapsedMs()).toBe(510);
  });

  it("spends exactly the elapsed budget before giving up", async () => {
    const clock = testClock();
    let attempts = 0;
    await expect(waitUntil(() => {
      attempts += 1;
      return false;
    }, 100, "Test condition", { clock })).rejects.toBeInstanceOf(DeadlineTimeoutError);
    expect(attempts).toBe(11);
    expect(clock.sleeps).toEqual([10, 10, 10, 10, 10, 10, 10, 10, 10, 10]);
    expect(clock.elapsedMs()).toBe(100);
  });

  it("clips its last sleep so it never waits past the deadline", async () => {
    const clock = testClock();
    await expect(waitUntil(() => false, 25, "Test condition", { clock }))
      .rejects.toBeInstanceOf(DeadlineTimeoutError);
    expect(clock.sleeps).toEqual([10, 10, 5]);
    expect(clock.elapsedMs()).toBe(25);
  });

  it("honours a caller's poll interval", async () => {
    const clock = testClock();
    await expect(waitUntil(() => false, 12, "Test condition", { clock, pollIntervalMs: 5 }))
      .rejects.toBeInstanceOf(DeadlineTimeoutError);
    expect(clock.sleeps).toEqual([5, 5, 2]);
  });

  it("reports the description and deadline when a condition times out", async () => {
    const clock = testClock();
    const timeout = waitUntil(() => false, 0, "Test condition", { clock });
    await expect(timeout).rejects.toBeInstanceOf(DeadlineTimeoutError);
    await expect(timeout).rejects.toMatchObject({
      name: "DeadlineTimeoutError",
      description: "Test condition",
      timeoutMs: 0,
      message: "Test condition did not complete within 0ms",
    } satisfies Partial<DeadlineTimeoutError>);
  });

  it("propagates a condition's own rejection without waiting", async () => {
    const clock = testClock();
    await expect(waitUntil(() => {
      throw new Error("condition exploded");
    }, 100, "Test condition", { clock })).rejects.toThrow("condition exploded");
    expect(clock.sleeps).toEqual([]);
  });

  it("times out on a frozen wall clock, because the default port is monotonic", async () => {
    // A Date.now stuck at one reading is exactly what a wall-clock deadline
    // cannot survive: it would never reach its deadline, and would hang.
    const wall = vi.spyOn(Date, "now").mockReturnValue(1_700_000_000_000);
    try {
      await expect(waitUntil(() => false, 20, "Frozen wall clock"))
        .rejects.toBeInstanceOf(DeadlineTimeoutError);
    } finally {
      wall.mockRestore();
    }
  });

  it("times out on a wall clock running backwards", async () => {
    let reading = 1_700_000_000_000;
    const wall = vi.spyOn(Date, "now").mockImplementation(() => {
      reading -= 1_000;
      return reading;
    });
    try {
      await expect(waitUntil(() => false, 20, "Reversing wall clock"))
        .rejects.toBeInstanceOf(DeadlineTimeoutError);
    } finally {
      wall.mockRestore();
    }
  });

  it("defaults to a monotonic, non-decreasing port", async () => {
    const first = monotonicWaitClock.now();
    await monotonicWaitClock.sleep(5);
    expect(monotonicWaitClock.now()).toBeGreaterThanOrEqual(first);
  });
});

describe("eventually", () => {
  it("returns once a retried assertion stops throwing", async () => {
    const clock = testClock();
    let attempts = 0;
    await expect(eventually(() => {
      attempts += 1;
      expect(attempts).toBe(3);
    }, 100, "Retried assertion", { clock })).resolves.toBeUndefined();
    expect(attempts).toBe(3);
    expect(clock.sleeps).toEqual([10, 10]);
  });

  it("rethrows the last assertion failure, not a generic deadline error", async () => {
    const clock = testClock();
    let attempts = 0;
    const failed = eventually(() => {
      attempts += 1;
      throw new Error(`assertion failed on attempt ${String(attempts)}`);
    }, 20, "Retried assertion", { clock });
    await expect(failed).rejects.toThrow("assertion failed on attempt 3");
    expect(attempts).toBe(3);
  });

  it("attaches the deadline to the rethrown failure so the cause chain carries both", async () => {
    const clock = testClock();
    const error = await eventually(() => {
      throw new Error("state never settled");
    }, 20, "Retried assertion", { clock }).then(() => undefined, (reason: unknown) => reason);
    expect(error).not.toBeInstanceOf(DeadlineTimeoutError);
    expect(causeChain(error)).toEqual([
      "state never settled",
      "Retried assertion did not complete within 20ms",
    ]);
    expect((error as Error).cause).toMatchObject({ description: "Retried assertion", timeoutMs: 20 });
  });

  it("keeps a failure's own cause rather than overwriting it", async () => {
    const clock = testClock();
    const original = new Error("outer", { cause: new Error("inner") });
    await expect(eventually(() => { throw original; }, 20, "Retried assertion", { clock }))
      .rejects.toBe(original);
    expect(causeChain(original)).toEqual(["outer", "inner"]);
  });

  it("awaits an asynchronous assertion", async () => {
    const clock = testClock();
    let attempts = 0;
    await expect(eventually(async () => {
      attempts += 1;
      await Promise.resolve();
      if (attempts < 2) throw new Error("not yet");
    }, 100, "Async assertion", { clock })).resolves.toBeUndefined();
    expect(attempts).toBe(2);
  });
});

describe("settle", () => {
  it("holds still for the requested window", async () => {
    const clock = testClock();
    await settle(50, "prove nothing else arrives", { clock });
    expect(clock.sleeps).toEqual([50]);
  });

  it("refuses an unexplained window", async () => {
    const clock = testClock();
    await expect(settle(50, "   ", { clock })).rejects.toThrow("settle requires a reason");
    expect(clock.sleeps).toEqual([]);
  });
});

describe("waitForProcessExit", () => {
  it("returns when a process no longer exists", async () => {
    const kill = vi.spyOn(process, "kill").mockImplementation(() => {
      throw Object.assign(new Error("missing"), { code: "ESRCH" });
    });
    try {
      await expect(waitForProcessExit(123, { timeoutMs: 100 })).resolves.toBeUndefined();
      expect(kill).toHaveBeenCalledWith(123, 0);
    } finally {
      kill.mockRestore();
    }
  });

  it("reports the pid and deadline when a process does not exit", async () => {
    await expect(waitForProcessExit(process.pid, { timeoutMs: 0 })).rejects.toThrow(
      `Process ${String(process.pid)} did not exit (waited 0ms)`,
    );
  });

  it("keeps the deadline error as the cause of the pid report", async () => {
    const error = await waitForProcessExit(process.pid, { timeoutMs: 0 })
      .then(() => undefined, (reason: unknown) => reason);
    expect((error as Error).cause).toBeInstanceOf(DeadlineTimeoutError);
    expect((error as Error).cause).toMatchObject({
      description: `Process ${String(process.pid)} exit`,
      timeoutMs: 0,
    });
    expect(causeChain(error)).toEqual([
      `Process ${String(process.pid)} did not exit (waited 0ms)`,
      `Process ${String(process.pid)} exit did not complete within 0ms`,
    ]);
  });

  it("propagates a kill error that is not ESRCH", async () => {
    const kill = vi.spyOn(process, "kill").mockImplementation(() => {
      throw Object.assign(new Error("not permitted"), { code: "EPERM" });
    });
    try {
      await expect(waitForProcessExit(123, { timeoutMs: 100 })).rejects.toThrow("not permitted");
    } finally {
      kill.mockRestore();
    }
  });
});

describe("waitForFile", () => {
  it("returns the contents once the file appears", async () => {
    const root = await temporaryRoot();
    const path = join(root, "appears.txt");
    const waited = waitForFile(path, { timeoutMs: 5_000 });
    await writeFile(path, "durable\n", "utf8");
    await expect(waited).resolves.toBe("durable\n");
  });

  it("ends the wait on an existing but empty file", async () => {
    const root = await temporaryRoot();
    const path = join(root, "empty.txt");
    await writeFile(path, "", "utf8");
    await expect(waitForFile(path, { timeoutMs: 0 })).resolves.toBe("");
  });

  it("times out with the path in the description when the file never appears", async () => {
    const root = await temporaryRoot();
    const path = join(root, "absent.txt");
    await expect(waitForFile(path, { timeoutMs: 0 })).rejects.toMatchObject({
      name: "DeadlineTimeoutError",
      description: `File ${path}`,
    });
  });

  it("propagates a read error that is not ENOENT", async () => {
    const root = await temporaryRoot();
    await expect(waitForFile(root, { timeoutMs: 0 })).rejects.toMatchObject({ code: "EISDIR" });
  });
});
