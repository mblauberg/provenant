import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, it, vi } from "vitest";

import { waitForFile, waitForProcessExit, waitUntil } from "../shared/deadline-wait.ts";

describe("deadline waits", () => {
  it("returns the first truthy condition result", async () => {
    let attempts = 0;
    await expect(waitUntil(async () => {
      attempts += 1;
      return attempts === 2 ? "ready" : "";
    }, 100, "Test condition")).resolves.toBe("ready");
    expect(attempts).toBe(2);
  });

  it("reports the description and deadline when a condition times out", async () => {
    await expect(waitUntil(async () => false, 0, "Test condition")).rejects.toThrow(
      "Test condition did not complete within 0ms",
    );
  });

  it("returns when a file exists", async () => {
    const directory = await mkdtemp(join(tmpdir(), "deadline-wait-"));
    const filePath = join(directory, "ready");
    try {
      await writeFile(filePath, "ready\n");
      await expect(waitForFile(filePath, { timeoutMs: 100 })).resolves.toBeUndefined();
    } finally {
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("reports the file path and deadline when a file does not appear", async () => {
    await expect(waitForFile("/missing/deadline-wait-file", { timeoutMs: 0 })).rejects.toThrow(
      "File did not appear: /missing/deadline-wait-file (waited 0ms)",
    );
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
