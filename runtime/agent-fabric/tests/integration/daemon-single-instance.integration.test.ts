import { link, mkdir, mkdtemp, rm, stat, symlink, writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, it } from "vitest";

import { startFabricDaemon } from "../../src/index.ts";
import { forceStartFabricDaemonForTests } from "../../src/daemon/client.ts";
import { waitForProcessExit } from "../shared/deadline-wait.ts";

const cleanup: Array<() => Promise<void>> = [];
const launcherHelper = fileURLToPath(new URL("../support/daemon-launcher-helper.ts", import.meta.url));

afterEach(async () => {
  await Promise.allSettled(cleanup.splice(0).reverse().map((close) => close()));
});

describe("daemon single-instance ownership", () => {
  it("attaches a second caller to the same elected Unix-socket owner", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-single-instance-"));
    const options = {
      databasePath: join(directory, "state", "fabric.sqlite3"),
      stateDirectory: join(directory, "state"),
      runtimeDirectory: join(directory, "runtime"),
      socketPath: join(directory, "runtime", "fabric.sock"),
    };
    const first = await startFabricDaemon(options);
    cleanup.push(async () => {
      await first.stop();
      await rm(directory, { recursive: true, force: true });
    });

    const attached = await startFabricDaemon(options);
    cleanup.push(async () => await attached.stop());
    expect(attached.pid).toBe(first.pid);
    expect(first.pid).toBeGreaterThan(0);
    await attached.stop();
    expect(() => process.kill(first.pid, 0)).not.toThrow();
  });

  it("retains test-only raw process cleanup coverage for one database through different sockets", async () => {
    const directory = await mkdtemp(join(tmpdir(), "afdb-"));
    const databasePath = join(directory, "state", "fabric.sqlite3");
    const first = await forceStartFabricDaemonForTests({
      databasePath,
      stateDirectory: join(directory, "state"),
      runtimeDirectory: join(directory, "a"),
      socketPath: join(directory, "a", "f.sock"),
    });
    cleanup.push(async () => {
      await first.stop();
      await rm(directory, { recursive: true, force: true });
    });
    expect((await stat(`${databasePath}.daemon.lock.sqlite3`)).isFile()).toBe(true);

    await expect(forceStartFabricDaemonForTests({
      databasePath,
      stateDirectory: join(directory, "state"),
      runtimeDirectory: join(directory, "b"),
      socketPath: join(directory, "b", "f.sock"),
    })).rejects.toMatchObject({ code: "DAEMON_ALREADY_RUNNING" });
  });

  it("refuses a second daemon opened through a symlink alias of the same database", async () => {
    const directory = await mkdtemp(join(tmpdir(), "afdb-alias-"));
    const databasePath = join(directory, "state", "fabric.sqlite3");
    const first = await startFabricDaemon({
      databasePath,
      stateDirectory: join(directory, "state"),
      runtimeDirectory: join(directory, "a"),
      socketPath: join(directory, "a", "f.sock"),
    });
    cleanup.push(async () => {
      await first.stop();
      await rm(directory, { recursive: true, force: true });
    });
    const aliasPath = join(directory, "fabric-alias.sqlite3");
    await symlink(databasePath, aliasPath);

    await expect(startFabricDaemon({
      databasePath: aliasPath,
      stateDirectory: join(directory, "state"),
      runtimeDirectory: join(directory, "b"),
      socketPath: join(directory, "b", "f.sock"),
    })).rejects.toMatchObject({ code: "DAEMON_DATABASE_PATH_UNSAFE" });

    expect(first.pid).toBeGreaterThan(0);
  });

  it("rejects dangling-symlink and hard-link database aliases", async () => {
    const directory = await mkdtemp(join(tmpdir(), "afdb-unsafe-alias-"));
    const stateDirectory = join(directory, "state");
    await mkdir(stateDirectory, { mode: 0o700 });
    const danglingTarget = join(stateDirectory, "not-created.sqlite3");
    const danglingAlias = join(directory, "dangling.sqlite3");
    await symlink(danglingTarget, danglingAlias);
    await expect(startFabricDaemon({
      databasePath: danglingAlias,
      stateDirectory,
      runtimeDirectory: join(directory, "dangling-runtime"),
      socketPath: join(directory, "dangling-runtime", "fabric.sock"),
    })).rejects.toMatchObject({ code: "DAEMON_DATABASE_PATH_UNSAFE" });

    const databasePath = join(stateDirectory, "fabric.sqlite3");
    const first = await startFabricDaemon({
      databasePath,
      stateDirectory,
      runtimeDirectory: join(directory, "a"),
      socketPath: join(directory, "a", "fabric.sock"),
    });
    const hardAlias = join(directory, "hard.sqlite3");
    await link(databasePath, hardAlias);
    try {
      await expect(startFabricDaemon({
        databasePath: hardAlias,
        stateDirectory,
        runtimeDirectory: join(directory, "b"),
        socketPath: join(directory, "b", "fabric.sock"),
      })).rejects.toMatchObject({ code: "DAEMON_DATABASE_PATH_UNSAFE" });
    } finally {
      await first.stop();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("elects exactly one owner when contenders recover the same stale database lock", async () => {
    const directory = await mkdtemp(join(tmpdir(), "afdb-stale-race-"));
    const stateDirectory = join(directory, "state");
    const databasePath = join(stateDirectory, "fabric.sqlite3");
    await mkdir(stateDirectory, { recursive: true, mode: 0o700 });
    await writeFile(`${databasePath}.daemon.lock`, `${JSON.stringify({ pid: 2_147_483_647, token: "stale" })}\n`);
    const attempts = await Promise.allSettled(Array.from({ length: 12 }, (_, index) => `contender-${index}`).map(async (name) => await forceStartFabricDaemonForTests({
      databasePath,
      stateDirectory,
      runtimeDirectory: join(directory, name),
      socketPath: join(directory, name, "fabric.sock"),
    })));
    const winners = attempts.filter((result) => result.status === "fulfilled");
    const losers = attempts.filter((result) => result.status === "rejected");
    try {
      expect(winners).toHaveLength(1);
      expect(losers).toHaveLength(11);
    } finally {
      await Promise.allSettled(winners.map(async (result) => {
        if (result.status === "fulfilled") await result.value.stop();
      }));
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("keeps ownership in the released daemon child after its launcher is killed", async () => {
    const directory = await mkdtemp(join(tmpdir(), "afdb-launcher-death-"));
    const options = {
      databasePath: join(directory, "state", "fabric.sqlite3"),
      stateDirectory: join(directory, "state"),
      runtimeDirectory: join(directory, "runtime"),
      socketPath: join(directory, "runtime", "fabric.sock"),
    };
    const launcher = spawn(process.execPath, ["--import", "tsx", launcherHelper], {
      cwd: fileURLToPath(new URL("../..", import.meta.url)),
      env: { ...process.env, DAEMON_LAUNCHER_OPTIONS: JSON.stringify(options) },
      stdio: ["ignore", "pipe", "pipe"],
    });
    if (launcher.stdout === null) throw new Error("launcher test stdout is unavailable");
    const lines = createInterface({ input: launcher.stdout, crlfDelay: Infinity });
    const line = await new Promise<string>((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error("launcher helper did not report its daemon")), 10_000);
      lines.once("line", (value) => {
        clearTimeout(timeout);
        resolve(value);
      });
      launcher.once("exit", (code) => reject(new Error(`launcher helper exited early: ${String(code)}`)));
    });
    const value: unknown = JSON.parse(line);
    if (
      typeof value !== "object" || value === null ||
      !("pid" in value) || typeof value.pid !== "number" ||
      !("released" in value) || value.released !== true
    ) {
      throw new Error("launcher helper did not confirm released daemon ownership");
    }
    const daemonPid = value.pid;
    launcher.kill("SIGKILL");
    await new Promise<void>((resolve) => launcher.once("exit", () => resolve()));
    try {
      const attached = await startFabricDaemon(options);
      expect(attached.pid).toBe(daemonPid);
      await attached.stop();
      expect(() => process.kill(daemonPid, 0)).not.toThrow();
    } finally {
      try { process.kill(daemonPid, "SIGTERM"); } catch { /* already stopped */ }
      try {
        await waitForProcessExit(daemonPid, { timeoutMs: 5_000 });
      } catch (error: unknown) {
        console.warn(
          `Failed to wait for process exit: ${
            error instanceof Error ? error.message : String(error)
          }`,
        );
      }
      lines.close();
      await rm(directory, { recursive: true, force: true });
    }
  });
});
