import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { randomBytes } from "node:crypto";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setImmediate as realSetImmediate } from "node:timers";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it, vi } from "vitest";

import { openFabric } from "../../../src/index.ts";
import { openLocalLifecycleReceiptAuthority } from "../../../src/lifecycle/local-receipt-authority.ts";
import {
  DAEMON_SHUTDOWN_FABRIC_CLOSE_TIMEOUT,
  DAEMON_SHUTDOWN_FABRIC_CLOSE_TIMEOUT_MS,
  waitWithShutdownDeadline,
} from "../../../src/lifecycle/shutdown-deadline.ts";
import * as shutdownFinalizer from "../../../src/daemon/shutdown-finalizer.ts";
import { createCurrentSessionRun } from "../../support/current-session-testkit.ts";
import { DAEMON_ROOT_AUTHORITY } from "../../support/daemon-testkit.ts";
import { waitForFile, waitForProcessExit } from "../../shared/deadline-wait.ts";

const roots: string[] = [];

afterEach(async () => {
  vi.useRealTimers();
  await Promise.allSettled(roots.splice(0).map(async (root) => await rm(root, { recursive: true, force: true })));
});

async function provisionLifecycleReceiptAuthority(stateDirectory: string): Promise<void> {
  await mkdir(stateDirectory, { recursive: true, mode: 0o700 });
  const database = new Database(join(stateDirectory, "lifecycle-receipts.sqlite3"));
  database.exec(await readFile(
    new URL("../../../schemas/lifecycle-receipt-authority-v1.sql", import.meta.url),
    "utf8",
  ));
  database.prepare("INSERT INTO authority_metadata VALUES(1,1,?)").run("shutdown-test-authority");
  database.close();
  await chmod(join(stateDirectory, "lifecycle-receipts.sqlite3"), 0o600);
  await writeFile(join(stateDirectory, "lifecycle-receipts.hmac.key"), randomBytes(32), { mode: 0o600 });
}

describe("shutdown deadline behaviour", () => {
  it("consumes a pending rejection after the shutdown deadline wins", async () => {
    vi.useFakeTimers();
    const unhandledRejection = vi.fn();
    process.on("unhandledRejection", unhandledRejection);
    try {
      const pending = Promise.withResolvers<void>();
      const pendingCatch = vi.spyOn(pending.promise, "catch");
      const waiting = waitWithShutdownDeadline(
        pending.promise,
        10,
        DAEMON_SHUTDOWN_FABRIC_CLOSE_TIMEOUT,
        "fabric close timed out",
      ).then(
        () => undefined,
        (error: unknown) => error,
      );

      expect(pendingCatch).toHaveBeenCalledTimes(1);
      await vi.advanceTimersByTimeAsync(10);
      await expect(waiting).resolves.toMatchObject({ code: DAEMON_SHUTDOWN_FABRIC_CLOSE_TIMEOUT });
      pending.reject(new Error("late cleanup failure"));
      await vi.runAllTicks();
      expect(unhandledRejection).not.toHaveBeenCalled();
    } finally {
      process.off("unhandledRejection", unhandledRejection);
      vi.useRealTimers();
    }
  });

  it("rejects when pending shutdown work misses its deadline", async () => {
    vi.useFakeTimers();
    try {
      const outcome = Promise.race([
        waitWithShutdownDeadline(
          new Promise<void>(() => undefined),
          10,
          DAEMON_SHUTDOWN_FABRIC_CLOSE_TIMEOUT,
          "fabric close timed out",
        ).catch((error: unknown) => error),
        new Promise<{ code: string }>((resolve) => setTimeout(() => resolve({ code: "TEST_TIMEOUT" }), 100)),
      ]);

      await vi.advanceTimersByTimeAsync(100);
      await expect(outcome).resolves.toMatchObject({ code: DAEMON_SHUTDOWN_FABRIC_CLOSE_TIMEOUT });
    } finally {
      vi.useRealTimers();
    }
  });

  it("tears down a wedged adapter child and closes the database after a drain timeout", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-shutdown-deadline-"));
    roots.push(root);
    const databasePath = join(root, "fabric.sqlite3");
    const adapterPidPath = join(root, "adapter.pid");
    const adapterPath = join(root, "wedged-adapter.mjs");
    await writeFile(adapterPath, `
      import { createInterface } from "node:readline";
      import { writeFileSync } from "node:fs";
      writeFileSync(${JSON.stringify(adapterPidPath)}, String(process.pid));
      const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
      input.on("line", (line) => {
        const request = JSON.parse(line);
        if (request.method === "capabilities") {
          process.stdout.write(JSON.stringify({
            id: request.id,
            result: { protocolVersion: 1, operations: ["capabilities", "dispatch"], actionJournal: true },
          }) + "\\n");
        }
      });
    `);

    let dispatch: Promise<unknown> | undefined;
    let closing: Promise<void> | undefined;
    let adapterPid: number | undefined;
    const fabric = await openFabric({
      databasePath,
      workspaceRoots: [root],
      adapters: {
        wedged: {
          command: [process.execPath, adapterPath],
          environment: {},
        },
      },
    });
    try {
      const run = await createCurrentSessionRun({
        databasePath,
        workspaceRoot: root,
        runId: "shutdown-deadline-test",
        chair: {
          agentId: "chair",
          authority: {
            ...DAEMON_ROOT_AUTHORITY,
            disclosure: { level: "scoped", scopes: ["local", "approved-provider"] },
          },
        },
      });
      const chair = fabric.connect(run.chairCapability);
      dispatch = chair.dispatchProviderAction({
        certifyingReview: null,
        adapterId: "wedged",
        actionId: "shutdown-deadline:wedged-action",
        operation: "steer",
        payload: { instruction: "hold this request" },
        commandId: "shutdown-deadline:dispatch",
      });
      void dispatch.catch(() => undefined);
      await waitForFile(adapterPidPath);
      adapterPid = Number(await readFile(adapterPidPath, "utf8"));

      vi.useFakeTimers();
      closing = fabric.close();
      const closingOutcome = closing.then(
        () => ({ kind: "resolved" as const }),
        (error: unknown) => ({ kind: "rejected" as const, error }),
      );
      let closingSettled = false;
      void closingOutcome.then(() => { closingSettled = true; });
      for (let attempt = 0; !closingSettled && attempt < 4; attempt += 1) {
        await vi.advanceTimersByTimeAsync(DAEMON_SHUTDOWN_FABRIC_CLOSE_TIMEOUT_MS + 1_000);
        await new Promise<void>((resolve) => realSetImmediate(resolve));
      }

      const outcome = await closingOutcome;
      expect(outcome).toMatchObject({
        kind: "rejected",
        error: { code: "DAEMON_SHUTDOWN_FABRIC_CLOSE_TIMEOUT" },
      });
      expect(adapterPid).toBeDefined();
      await waitForProcessExit(adapterPid as number, {
        timeoutMs: 2_000,
        clock: {
          now: () => performance.now(),
          sleep: async () => await new Promise<void>((resolve) => realSetImmediate(resolve)),
        },
      });
      await expect(chair.getMailboxState()).rejects.toThrow();
    } finally {
      vi.useRealTimers();
      if (adapterPid !== undefined) {
        try { process.kill(adapterPid, "SIGKILL"); } catch { /* already stopped */ }
      }
      if (closing !== undefined) {
        await Promise.race([
          closing.catch(() => undefined),
          new Promise<void>((resolve) => setTimeout(resolve, 1_000)),
        ]);
      } else {
        await Promise.race([
          fabric.close().catch(() => undefined),
          new Promise<void>((resolve) => setTimeout(resolve, 1_000)),
        ]);
      }
      await dispatch?.catch(() => undefined);
    }
  }, 10_000);

  it("closes the lifecycle receipt authority when fabric close fails", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-shutdown-authority-"));
    roots.push(root);
    const stateDirectory = join(root, "state");
    await provisionLifecycleReceiptAuthority(stateDirectory);
    const authority = openLocalLifecycleReceiptAuthority({
      stateDirectory,
      expectedAuthorityId: "shutdown-test-authority",
    });
    const closeFabricWithAuthority = Reflect.get(
      shutdownFinalizer,
      "closeFabricWithLifecycleReceiptAuthority",
    ) as ((input: {
      closeFabric(): Promise<void>;
      closeAuthority(): void;
    }) => Promise<void>) | undefined;
    expect(closeFabricWithAuthority).toEqual(expect.any(Function));
    if (closeFabricWithAuthority === undefined) return;

    const failure = new Error("fabric close failed");
    await expect(closeFabricWithAuthority({
      closeFabric: async () => { throw failure; },
      closeAuthority: () => authority.close(),
    })).rejects.toBe(failure);
    await expect(authority.admitScope({
      schemaVersion: 1,
      projectId: "project",
      projectSessionId: "session",
      runId: "run",
      authorityId: "shutdown-test-authority",
      admissionDigest: `sha256:${"a".repeat(64)}`,
      admittedAt: 1,
    })).rejects.toThrow();
  });
});
