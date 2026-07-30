import { describe, expect, it, vi } from "vitest";

import {
  DeadlineTimeoutError,
  waitForProcessExit,
  waitUntil,
} from "../shared/deadline-wait.ts";

describe("deadline waits", () => {
  it("returns the first truthy condition result", async () => {
    let attempts = 0;
    await expect(waitUntil(async () => {
      attempts += 1;
      return attempts === 2 ? "ready" : "";
    }, 100, "Test condition")).resolves.toBe("ready");
    expect(attempts).toBe(2);
  });

  it("checks a true condition once when the timeout is zero", async () => {
    let attempts = 0;
    await expect(waitUntil(async () => {
      attempts += 1;
      return "ready";
    }, 0, "Test condition")).resolves.toBe("ready");
    expect(attempts).toBe(1);
  });

  it("returns a condition that becomes true exactly at the deadline", async () => {
    const now = vi.spyOn(Date, "now")
      .mockReturnValueOnce(0)
      .mockReturnValueOnce(0)
      .mockReturnValueOnce(0)
      .mockReturnValue(10);
    let attempts = 0;
    let observedAt = -1;
    try {
      await expect(waitUntil(async () => {
        attempts += 1;
        if (attempts !== 2) return "";
        observedAt = Date.now();
        return "ready";
      }, 10, "Test condition")).resolves.toBe("ready");
      expect(attempts).toBe(2);
      expect(observedAt).toBe(10);
    } finally {
      now.mockRestore();
    }
  });

  it("returns a condition that becomes true after the deadline", async () => {
    const now = vi.spyOn(Date, "now")
      .mockReturnValueOnce(0)
      .mockReturnValueOnce(0)
      .mockReturnValueOnce(0)
      .mockReturnValue(11);
    let attempts = 0;
    let observedAt = -1;
    try {
      await expect(waitUntil(async () => {
        attempts += 1;
        if (attempts !== 2) return "";
        observedAt = Date.now();
        return "ready";
      }, 10, "Test condition")).resolves.toBe("ready");
      expect(attempts).toBe(2);
      expect(observedAt).toBe(11);
    } finally {
      now.mockRestore();
    }
  });

  it("reports the description and deadline when a condition times out", async () => {
    let attempts = 0;
    const timeout = waitUntil(async () => {
      attempts += 1;
      return false;
    }, 0, "Test condition");
    await expect(timeout).rejects.toBeInstanceOf(DeadlineTimeoutError);
    await expect(timeout).rejects.toMatchObject({
      name: "DeadlineTimeoutError",
      description: "Test condition",
      timeoutMs: 0,
      message: "Test condition did not complete within 0ms",
    } satisfies Partial<DeadlineTimeoutError>);
    expect(attempts).toBe(1);
  });

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
});
