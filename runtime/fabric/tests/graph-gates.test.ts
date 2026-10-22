import { spawn } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, it } from "vitest";

import { launchGate, readGateExit } from "../src/graph/gates.js";
import { processStartedAt, signalRecordedGroup } from "../src/run-registry.js";
import { scratchRoot } from "./helpers/temp-root.js";

const tsxLoader = createRequire(import.meta.url).resolve("tsx");
const env = { ...process.env, AGENT_FABRIC_TSX_LOADER: tsxLoader, NODE_NO_WARNINGS: "1" };
const cleanup: number[] = [];

const alive = (pid: number) => { try { process.kill(pid, 0); return true; } catch (error) { return (error as NodeJS.ErrnoException).code !== "ESRCH"; } };
async function until(check: () => boolean, timeoutMs = 15_000): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (!check()) {
    if (Date.now() > deadline) return false;
    await new Promise((resolveWait) => setTimeout(resolveWait, 50));
  }
  return true;
}

afterEach(() => {
  for (const pid of cleanup.splice(0)) try { process.kill(-pid, "SIGKILL"); } catch { /* gone */ }
});

/** Run the stand-in owner until it kills itself; return the pids it reported. */
function owner(mode: "no-go" | "buffered-go", dir: string): Promise<{ wrapper: number; bystander: number }> {
  const worker = fileURLToPath(new URL("./graph-gate-worker.ts", import.meta.url));
  return new Promise((done, fail) => {
    const child = spawn(process.execPath, ["--import", tsxLoader, worker, mode, "tok-1", dir], { cwd: dir, env, stdio: ["ignore", "pipe", "inherit"] });
    let stdout = "";
    child.stdout.on("data", (chunk: Buffer) => { stdout += chunk.toString(); });
    child.once("exit", (_code, signal) => {
      if (signal !== "SIGKILL") return fail(new Error(`owner exited without dying: ${signal}`));
      const pids = JSON.parse(stdout.trim()) as { wrapper: number; bystander: number };
      cleanup.push(pids.bystander, pids.wrapper);
      done(pids);
    });
  });
}

describe("gate launch handshake", () => {
  it("runs nothing when the owner dies between the gate intent and go, even with another child holding inherited fds", async () => {
    const dir = scratchRoot("graph-gate-nogo-");
    const { wrapper, bystander } = await owner("no-go", dir);
    expect(await until(() => !alive(wrapper))).toBe(true);
    expect(alive(bystander)).toBe(true);
    expect(existsSync(join(dir, "marker"))).toBe(false);
    expect(existsSync(join(dir, "exit.json"))).toBe(false);
  }, 30_000);

  it("still runs and records a gate whose go was written just before the owner died", async () => {
    const dir = scratchRoot("graph-gate-buffered-");
    await owner("buffered-go", dir);
    expect(await until(() => readGateExit(join(dir, "exit.json")) !== undefined)).toBe(true);
    expect(readGateExit(join(dir, "exit.json"))).toMatchObject({ exit_code: 0, signal: null });
    expect(readFileSync(join(dir, "marker"), "utf8")).toBe("ran\n");
  }, 30_000);

  it("reaps the gate command and records its exit status", async () => {
    const dir = scratchRoot("graph-gate-reap-");
    const launch = launchGate("tok-2", dir, env);
    cleanup.push(launch.pid);
    launch.go({ token: "tok-2", argv: ["sh", "-c", "echo out; exit 7"], cwd: dir, out: join(dir, "o"), err: join(dir, "e"), exit: join(dir, "x.json") });
    await launch.exited;
    expect(readGateExit(join(dir, "x.json"))).toMatchObject({ exit_code: 7 });
    expect(readFileSync(join(dir, "o"), "utf8")).toBe("out\n");
    expect(await until(() => !alive(-launch.pid))).toBe(true);
  }, 30_000);

  it("refuses a go line carrying another token", async () => {
    const dir = scratchRoot("graph-gate-token-");
    const launch = launchGate("tok-3", dir, env);
    cleanup.push(launch.pid);
    launch.go({ token: "other", argv: ["sh", "-c", "echo ran > marker"], cwd: dir, out: join(dir, "o"), err: join(dir, "e"), exit: join(dir, "x.json") });
    await launch.exited;
    expect(existsSync(join(dir, "marker"))).toBe(false);
  }, 30_000);
});

describe("signalRecordedGroup", () => {
  it("terminates a gate group whose forked children ignore SIGTERM, and verifies none remain", async () => {
    const dir = scratchRoot("graph-gate-group-");
    const launch = launchGate("tok-4", dir, env);
    cleanup.push(launch.pid);
    const started = processStartedAt(launch.pid);
    launch.go({ token: "tok-4", argv: ["sh", "-c", "trap '' TERM; (trap '' TERM; sleep 60; :) & echo $! > child; sleep 60; :"], cwd: dir,
      out: join(dir, "o"), err: join(dir, "e"), exit: join(dir, "x.json") });
    expect(await until(() => existsSync(join(dir, "child")) && readFileSync(join(dir, "child"), "utf8").trim() !== "")).toBe(true);
    const child = Number(readFileSync(join(dir, "child"), "utf8"));
    expect(alive(child)).toBe(true);
    await expect(signalRecordedGroup({ pid: launch.pid, pgid: launch.pid, started_at: started }, 300)).resolves.toBe("exited");
    expect(alive(-launch.pid)).toBe(false);
    expect(alive(child)).toBe(false);
  }, 30_000);

  it("treats a group whose leader pid now belongs to another process as exited, and an unrecorded start as fenced", async () => {
    await expect(signalRecordedGroup({ pid: process.pid, pgid: process.pid, started_at: "x" })).resolves.toBe("fenced");
    const sleeper = spawn("sleep", ["30"], { detached: true, stdio: "ignore" });
    cleanup.push(sleeper.pid!);
    await expect(signalRecordedGroup({ pid: sleeper.pid!, pgid: sleeper.pid!, started_at: null })).resolves.toBe("fenced");
    await expect(signalRecordedGroup({ pid: sleeper.pid!, pgid: sleeper.pid!, started_at: "Thu Jan  1 00:00:00 1970" })).resolves.toBe("exited");
    expect(alive(sleeper.pid!)).toBe(true);
  }, 30_000);
});
