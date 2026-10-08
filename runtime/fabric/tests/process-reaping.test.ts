import { spawn, type ChildProcess } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { once } from "node:events";
import * as ps from "../src/ps.mjs";
import { afterEach, expect, it, vi } from "vitest";
import { listRecordedRuns, processStartedAt, reapOrphanedRuns, signalRecordedProcess,
  signalRecordedRun, terminateRecordedRun, writeOwnerRecord } from "../src/run-registry.js";

const directories: string[] = [];
const children: ChildProcess[] = [];

afterEach(async () => {
  vi.restoreAllMocks();
  for (const child of children.splice(0)) {
    if (child.exitCode === null && child.signalCode === null) {
      const exited = once(child, "exit");
      child.kill("SIGKILL");
      await exited;
    }
  }
  for (const dir of directories.splice(0)) rmSync(dir, { recursive: true, force: true });
});

it.each(["EPERM", "EACCES", "unknown_start"])(
  "does not reap a live lane when its host probe is %s", async (restriction) => {
    const workspace = mkdtempSync(join(tmpdir(), "fabric-reaping-"));
    directories.push(workspace);
    const runDir = join(workspace, ".agent-run", "mcp-live-lane");
    mkdirSync(runDir, { recursive: true });
    const owner = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"],
      { detached: true, stdio: "ignore" });
    children.push(owner);
    await once(owner, "spawn");
    const ownerPid = owner.pid!;
    writeOwnerRecord({ schema_version: 1, kind: "dispatch", run_dir: runDir, workspace,
      run_token: "live", owner_pid: ownerPid, owner_pgid: ownerPid,
      owner_started_at: processStartedAt(ownerPid), host_pid: process.pid,
      host_started_at: restriction === "unknown_start" ? null : processStartedAt(process.pid),
      started_at: new Date().toISOString(), owner_stdout: "", owner_stderr: "" });
    writeFileSync(join(runDir, "dispatch-status.json"), JSON.stringify({ status: "running" }));
    const realKill = process.kill.bind(process);
    const signals: [number, string | number | undefined][] = [];
    vi.spyOn(process, "kill").mockImplementation((pid, sig) => {
      if (pid === process.pid && sig === 0 && restriction !== "unknown_start")
        throw Object.assign(new Error("probe denied"), { code: restriction });
      if (sig !== 0) signals.push([pid, sig]);
      return realKill(pid, sig);
    });
    expect(listRecordedRuns(workspace)[0]).toMatchObject({ running: true, orphaned: false });
    expect(await reapOrphanedRuns(workspace)).toEqual([]);
    expect(signals).toEqual([]);
    expect(owner.signalCode).toBeNull();
  },
);

it("refuses a recorded group that does not match the verified process", () => {
  const signals: number[] = [];
  const realKill = process.kill.bind(process);
  vi.spyOn(process, "kill").mockImplementation((pid, sig) => {
    if (sig === 0) return realKill(pid, sig);
    signals.push(pid);
    return true;
  });
  expect(signalRecordedProcess(process.pid, process.pid + 100, processStartedAt(process.pid), "SIGTERM"))
    .toBe(false);
  expect(signals).toEqual([]);
});

it.each(["shared-host", "unavailable-census", "partial-census", "host-omitted"])(
  "signals only its verified provider PID with a %s", async (census) => {
    const provider = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"],
      { detached: true, stdio: "ignore" });
    children.push(provider);
    await once(provider, "spawn");
    const pid = provider.pid!;
    const started = processStartedAt(pid);
    const originalPs = ps.psOutput;
    vi.spyOn(ps, "psOutput").mockImplementation((args, env) => {
      if (args.includes("-e")) {
        if (census === "unavailable-census") throw new Error("census unavailable");
        if (census === "partial-census") return "999999 999999 node\n";
        if (census === "host-omitted") return `${pid} ${pid} node\n`;
        return `${pid} ${pid} node\n999999 ${pid} codex-code-mode-host\n`;
      }
      return originalPs(args, env);
    });
    const signals: number[] = [];
    const realKill = process.kill.bind(process);
    vi.spyOn(process, "kill").mockImplementation((target, sig) => {
      if (sig === 0) return realKill(target, sig);
      signals.push(target);
      return true;
    });
    expect(signalRecordedProcess(pid, pid, started, "SIGTERM")).toBe(true);
    expect(signals).toEqual([pid]);
  },
);

it.each(["cancel_requested", "orphan_reaper"])(
  "%s signals the verified provider individually despite its own process group", async (source) => {
    const workspace = mkdtempSync(join(tmpdir(), "fabric-reaping-"));
    directories.push(workspace);
    const runDir = join(workspace, ".agent-run", "mcp-controller");
    mkdirSync(runDir, { recursive: true });
    const provider = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"],
      { detached: true, stdio: "ignore" });
    children.push(provider);
    await once(provider, "spawn");
    const pid = provider.pid!;
    const started = processStartedAt(pid);
    const signals: number[] = [];
    const realKill = process.kill.bind(process);
    vi.spyOn(process, "kill").mockImplementation((target, sig) => {
      if (sig === 0) return realKill(target, sig);
      signals.push(target);
      return true;
    });
    expect(signalRecordedRun({ schema_version: 1, kind: "dispatch", run_dir: runDir, workspace,
      run_id: "mcp-controller", run_token: "controller", owner_pid: process.pid, owner_pgid: process.pid,
      owner_started_at: processStartedAt(process.pid), host_pid: process.pid,
      host_started_at: processStartedAt(process.pid), started_at: new Date().toISOString(),
      owner_stdout: "", owner_stderr: "", running: true, orphaned: false,
      provider: { run_token: "controller", provider_pid: pid, provider_pgid: pid, provider_started_at: started },
    }, "SIGTERM", source)).toBe(true);
    expect(signals).toEqual([pid]);
    expect(JSON.parse(readFileSync(join(runDir, "termination-request.json"), "utf8")))
      .toMatchObject({ source, provider_pid: pid, sender_pid: process.pid });
  },
);

it("records the orphan reaper source before terminating an owned process", async () => {
  const workspace = mkdtempSync(join(tmpdir(), "fabric-reaping-"));
  directories.push(workspace);
  const runDir = join(workspace, ".agent-run", "mcp-owned");
  mkdirSync(runDir, { recursive: true });
  const owner = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"],
    { detached: true, stdio: "ignore" });
  children.push(owner);
  await once(owner, "spawn");
  const pid = owner.pid!;
  const run = { schema_version: 1 as const, kind: "dispatch" as const, run_dir: runDir, workspace,
    run_token: "owned", owner_pid: pid, owner_pgid: pid, owner_started_at: processStartedAt(pid),
    host_pid: process.pid, host_started_at: processStartedAt(process.pid),
    started_at: new Date().toISOString(), owner_stdout: "", owner_stderr: "", run_id: "mcp-owned",
    running: true, orphaned: true, provider: null };
  const outcome = await terminateRecordedRun(run, 1000, "interrupted", "orphan_reaper");
  expect(outcome).toMatchObject({ signalled: true, escalated: false });
  expect(JSON.parse(readFileSync(join(runDir, "termination-request.json"), "utf8")))
    .toMatchObject({ source: "orphan_reaper", sender_pid: process.pid, signal: "SIGTERM", run_token: "owned" });
});
