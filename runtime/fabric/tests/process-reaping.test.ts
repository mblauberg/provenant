import { spawn, type ChildProcess } from "node:child_process";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { once } from "node:events";
import * as ps from "../src/ps.mjs";
import { afterEach, expect, it, vi } from "vitest";
import { scratchRoot } from "./helpers/temp-root.js";
import { listRecordedRuns, processStartedAt, reapAttemptRuns, reapOrphanedRuns, signalRecordedProcess, statusRows,
  signalRecordedRun, terminateRecordedRun, writeOwnerRecord } from "../src/run-registry.js";

const directories: string[] = [];
const children: ChildProcess[] = [];

it.each([
  ["closure", "interrupted"],
  ["closure", "cancelled"],
  ["request", "interrupted"],
  ["request", "cancelled"],
  ["stale-request", "lost"],
  ["marker", "cancelled"],
  ["other-marker", "lost"],
  ["batch-marker", "cancelled"],
  ["other-batch-marker", "lost"],
  ["unbound-batch-marker", "lost"],
])("preserves %s evidence when reaping a dead attempt as %s", async (evidence, status) => {
  const workspace = scratchRoot("fabric-attempt-reaping-");
  directories.push(workspace);
  const runDir = join(workspace, ".agent-run/runs/20261021-1200-dispatch-stopped-abcdef");
  const attemptDir = join(runDir, "tasks/stopped/attempt-001");
  mkdirSync(attemptDir, { recursive: true });
  writeFileSync(join(runDir, "MANIFEST.md"), "# Manifest\n");
  writeFileSync(join(runDir, "RUN_RECEIPT.json"), JSON.stringify({ status: "active" }));
  const row = JSON.parse(readFileSync(new URL("fixtures/attempt.json", import.meta.url), "utf8"));
  Object.assign(row, { state: "running", status: null, task_id: "stopped", cwd: workspace,
    pgid: 99_999_999, pgid_started_at: "gone", ended_at: null });
  if (evidence === "batch-marker" || evidence === "other-batch-marker") row.batch_id = "batch-002";
  const path = join(attemptDir, "attempt.json");
  writeFileSync(path, JSON.stringify(row));
  writeFileSync(join(runDir, "dispatch-status.json"), JSON.stringify({ run_token: "current",
    owner_pid: 99_999_999, status: evidence === "closure" ? status : "running",
    ...(evidence === "closure" ? { finished_at: new Date().toISOString() } : {}) }));
  if (evidence === "request" || evidence === "stale-request") {
    writeFileSync(join(runDir, "termination-request.json"), JSON.stringify({
      run_token: evidence === "stale-request" ? "previous" : "current", signal: "SIGKILL",
      source: status === "cancelled" ? "cancel_requested" : "orphan_reaper",
      sender_pid: process.pid, requested_at: new Date().toISOString(), provider_pid: row.pgid,
    }));
  }
  if (evidence === "marker" || evidence === "other-marker") {
    const target = join(runDir, "dispatch/tasks", evidence === "marker" ? "stopped" : "other", "attempt-001");
    mkdirSync(target, { recursive: true });
    writeFileSync(join(target, "cancel.request"), "");
  }
  if (evidence.endsWith("batch-marker")) {
    const target = join(runDir, "dispatch/batches", evidence === "batch-marker" ? "batch-002" : "batch-001");
    mkdirSync(target, { recursive: true });
    writeFileSync(join(target, "cancel.request"), "");
  }
  // Exercise startup reaping before the status reader can synthesise a closure.
  await reapAttemptRuns(workspace);
  const result = await statusRows(workspace, ["stopped"], 0, "all", undefined, "full", false);
  expect(result.runs).toEqual([expect.objectContaining({ state: "terminal", status })]);
  expect(JSON.parse(readFileSync(path, "utf8"))).toMatchObject({ state: "terminal", status });
});

it.each(["status", "startup"])("%s persists a SIGKILL'd attempt as lost", async (hook) => {
  const workspace = scratchRoot("fabric-attempt-reaping-");
  directories.push(workspace);
  const runDir = join(workspace, ".agent-run/runs/20261021-1200-dispatch-killed-abcdef");
  const attemptDir = join(runDir, "tasks/killed/attempt-001");
  mkdirSync(attemptDir, { recursive: true });
  writeFileSync(join(runDir, "MANIFEST.md"), "# Manifest\n");
  writeFileSync(join(runDir, "RUN_RECEIPT.json"), JSON.stringify({ status: "active" }));
  const owner = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { detached: true, stdio: "ignore" });
  children.push(owner);
  await once(owner, "spawn");
  const row = JSON.parse(readFileSync(new URL("fixtures/attempt.json", import.meta.url), "utf8"));
  Object.assign(row, { state: "running", status: null, task_id: "killed", cwd: workspace,
    pgid: owner.pid!, pgid_started_at: processStartedAt(owner.pid!), ended_at: null });
  const path = join(attemptDir, "attempt.json");
  writeFileSync(path, JSON.stringify(row));
  const unrelated = join(workspace, ".agent-run/runs/20261021-1200-dispatch-other-fedcba");
  mkdirSync(join(unrelated, "tasks/other/attempt-001"), { recursive: true });
  writeFileSync(join(unrelated, "MANIFEST.md"), "# Manifest\n");
  writeFileSync(join(unrelated, "RUN_RECEIPT.json"), JSON.stringify({ status: "active" }));
  const otherPath = join(unrelated, "tasks/other/attempt-001/attempt.json");
  writeFileSync(otherPath, JSON.stringify({ ...row, task_id: "other", run_id: "mcp-fedcba" }));
  const exited = once(owner, "exit");
  process.kill(-owner.pid!, "SIGKILL");
  await exited;
  if (hook === "startup") await reapAttemptRuns(workspace);
  const result = await statusRows(workspace, ["killed"], 0, "all", undefined, "full", false);
  expect(result.runs).toEqual([expect.objectContaining({ state: "terminal", status: "lost", error_class: "provider_error" })]);
  expect(JSON.parse(readFileSync(path, "utf8"))).toMatchObject({ state: "terminal", status: "lost", evidence: { signature: "pgid_gone" } });
  expect(JSON.parse(readFileSync(join(runDir, "RUN_RECEIPT.json"), "utf8"))).toMatchObject({ status: "failed", attempts: [expect.objectContaining({ status: "lost" })] });
  expect(JSON.parse(readFileSync(otherPath, "utf8"))).toMatchObject({ state: hook === "startup" ? "terminal" : "running" });
  const before = readFileSync(path, "utf8");
  await statusRows(workspace, ["killed"], 0, "all", undefined, "full", false);
  expect(readFileSync(path, "utf8")).toBe(before);
});

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
    const workspace = scratchRoot("fabric-reaping-");
    directories.push(workspace);
    const runDir = join(workspace, ".agent-run", "runs", "20260801-1200-dispatch-live-lane-abcdef");
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
    const workspace = scratchRoot("fabric-reaping-");
    directories.push(workspace);
    const runDir = join(workspace, ".agent-run", "runs", "20260801-1200-dispatch-controller-abcdef");
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
  const workspace = scratchRoot("fabric-reaping-");
  directories.push(workspace);
  const runDir = join(workspace, ".agent-run", "runs", "20260801-1200-dispatch-owned-abcdef");
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
