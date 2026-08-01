import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

import { startFabricDaemon } from "../../src/index.ts";
import { waitUntil } from "../shared/deadline-wait.ts";
import { createMcpFixture } from "../support/mcp-testkit.ts";

const HARNESS_ROOT = fileURLToPath(new URL("../../../..", import.meta.url));
const RETAINED_LIFECYCLE_WORKER = fileURLToPath(new URL("../support/retained-lifecycle-worker.ts", import.meta.url));

function processExists(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function stopTestCreatedProcess(pid: number, label: string): Promise<void> {
  if (!processExists(pid)) return;
  try { process.kill(pid, "SIGTERM"); } catch { /* already gone */ }
  try {
    await waitUntil(() => !processExists(pid), 500, `${label} graceful exit`);
    return;
  } catch {
    try { process.kill(pid, "SIGKILL"); } catch { /* already gone */ }
    await waitUntil(() => !processExists(pid), 2_000, `${label} forced exit`);
  }
}

describe.sequential("agent-fabric test fixture cleanup", () => {
  it("rolls back daemon and proxy processes when MCP fixture construction fails", async () => {
    let daemonPid: number | undefined;
    let proxyPid: number | undefined;

    await expect(createMcpFixture(
      "run-mcp-partial-cleanup",
      { chair: "cleanup-chair", peer: "cleanup-peer" },
      {
        chairProxyStarted(input) {
          daemonPid = input.daemonPid;
          proxyPid = input.proxyPid;
          throw new Error("injected fixture construction failure");
        },
      },
    )).rejects.toThrow("injected fixture construction failure");

    expect(daemonPid).toBeTypeOf("number");
    expect(proxyPid).toBeTypeOf("number");
    if (daemonPid === undefined || proxyPid === undefined) {
      throw new Error("fixture failure hook did not capture child process IDs");
    }
    expect(processExists(daemonPid)).toBe(false);
    expect(processExists(proxyPid)).toBe(false);
  });

  it("starts the source daemon when its caller cwd is the harness root", async () => {
    const directory = await mkdtemp(join(tmpdir(), "agent-fabric-daemon-cwd-"));
    const originalCwd = process.cwd();
    let daemon: Awaited<ReturnType<typeof startFabricDaemon>> | undefined;
    try {
      process.chdir(HARNESS_ROOT);
      daemon = await startFabricDaemon({
        databasePath: join(directory, "state", "fabric.sqlite3"),
        stateDirectory: join(directory, "state"),
        runtimeDirectory: join(directory, "runtime"),
        socketPath: join(directory, "runtime", "fabric.sock"),
      });
      expect(daemon.pid).toBeGreaterThan(0);
    } finally {
      process.chdir(originalCwd);
      await daemon?.stop();
      await rm(directory, { recursive: true, force: true });
    }
  });

  it("removes a retained daemon when its owning worker is interrupted", async () => {
    const ownerDirectory = await mkdtemp(join(tmpdir(), "agent-fabric-retained-owner-"));
    const handoffPath = join(ownerDirectory, "handoff.json");
    let worker: ReturnType<typeof spawn> | undefined;
    let daemonPid: number | undefined;
    let daemonDirectory: string | undefined;
    let daemonSocketPath: string | undefined;
    let daemonStateDirectory: string | undefined;
    let daemonRuntimeDirectory: string | undefined;
    let assertionError: unknown;
    let cleanupError: unknown;
    let processExitObserved = false;

    try {
      worker = spawn(process.execPath, ["--import", "tsx", RETAINED_LIFECYCLE_WORKER], {
        cwd: HARNESS_ROOT,
        env: { ...process.env, RETAINED_LIFECYCLE_HANDOFF: handoffPath },
        stdio: ["ignore", "ignore", "pipe"],
      });
      let workerStderr = "";
      worker.stderr?.setEncoding("utf8");
      worker.stderr?.on("data", (chunk: string) => { workerStderr += chunk; });

      try {
        await waitUntil(() => existsSync(handoffPath), 30_000, "retained lifecycle worker handoff");
        const handoff = JSON.parse(await readFile(handoffPath, "utf8")) as {
          daemonPid: number;
          directory: string;
          runtimeDirectory: string;
          stateDirectory: string;
          socketPath: string;
        };
        daemonPid = handoff.daemonPid;
        daemonDirectory = handoff.directory;
        daemonRuntimeDirectory = handoff.runtimeDirectory;
        daemonStateDirectory = handoff.stateDirectory;
        daemonSocketPath = handoff.socketPath;
        expect(Number.isInteger(daemonPid)).toBe(true);
        expect(daemonPid).toBeGreaterThan(0);
        expect(existsSync(daemonSocketPath)).toBe(true);
        expect(existsSync(daemonStateDirectory)).toBe(true);
        expect(processExists(daemonPid)).toBe(true);

        expect(worker.kill("SIGTERM")).toBe(true);
        await new Promise<void>((resolve, reject) => {
          worker?.once("exit", () => resolve());
          worker?.once("error", reject);
        });
        processExitObserved = true;
        await waitUntil(() => !processExists(daemonPid as number), 2_000, `retained daemon ${String(daemonPid)} exit`);
        expect(processExists(daemonPid)).toBe(false);
      } catch (error: unknown) {
        assertionError = workerStderr.length === 0 ? error : new Error(`${String(error)}\n${workerStderr}`);
      }
    } finally {
      if (worker !== undefined && worker.exitCode === null && worker.signalCode === null) {
        try {
          worker.kill("SIGKILL");
          await new Promise<void>((resolve) => worker?.once("exit", () => resolve()));
        } catch (error: unknown) {
          cleanupError = error;
        }
      }
      if (daemonPid !== undefined && processExists(daemonPid)) {
        try {
          await stopTestCreatedProcess(daemonPid, `retained daemon ${String(daemonPid)} fallback`);
        } catch (error: unknown) {
          cleanupError = error;
        }
      }
      await rm(daemonDirectory ?? ownerDirectory, { recursive: true, force: true });
      if (daemonDirectory !== undefined) await rm(ownerDirectory, { recursive: true, force: true });
    }

    expect(assertionError).toBeUndefined();
    expect(cleanupError).toBeUndefined();
    expect(processExitObserved, "the inner worker must actually exit before custody is assessed").toBe(true);
    expect(daemonPid).toBeTypeOf("number");
    if (daemonPid !== undefined) expect(processExists(daemonPid), `daemon process remains after cleanup: ${String(daemonPid)}`).toBe(false);
    expect(daemonDirectory).toBeTypeOf("string");
    if (daemonDirectory !== undefined) expect(existsSync(daemonDirectory), `daemon directory remains after cleanup: ${daemonDirectory}`).toBe(false);
    expect(daemonRuntimeDirectory).toBeTypeOf("string");
    if (daemonRuntimeDirectory !== undefined) expect(existsSync(daemonRuntimeDirectory), `runtime directory remains after cleanup: ${daemonRuntimeDirectory}`).toBe(false);
    expect(daemonStateDirectory).toBeTypeOf("string");
    if (daemonStateDirectory !== undefined) expect(existsSync(daemonStateDirectory), `state directory remains after cleanup: ${daemonStateDirectory}`).toBe(false);
    expect(daemonSocketPath).toBeTypeOf("string");
    if (daemonSocketPath !== undefined) expect(existsSync(daemonSocketPath), `socket remains after cleanup: ${daemonSocketPath}`).toBe(false);
  });
});
