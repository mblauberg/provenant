import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import Database from "better-sqlite3";
import { execFileSync, spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { afterEach, expect, it } from "vitest";

const pidLogs = new Set<string>();
const roots = new Set<string>();
afterEach(async () => {
  const active = new Set<number>();
  for (const log of pidLogs) {
    try {
      for (const line of readFileSync(log, "utf8").split("\n").filter(Boolean)) {
        const event = JSON.parse(line) as { pid: number; event: "start" | "exit" };
        if (event.event === "start") active.add(event.pid);
        else active.delete(event.pid);
      }
    } catch {}
  }
  for (const pid of active) {
    try { process.kill(pid, "SIGKILL"); } catch {}
  }
  for (const root of roots) rmSync(root, { recursive: true, force: true });
  pidLogs.clear();
  roots.clear();
});

/** Two agents of one project: a chair in a linked worktree and a worker in the primary checkout. */
function fixtureProject() {
  const root = mkdtempSync(join(tmpdir(), "fabric-sessions-"));
  roots.add(root);
  const pidLog = join(root, "fixture-pids.jsonl");
  pidLogs.add(pidLog);
  const primary = join(root, "primary"), linked = join(root, "linked"), product = join(root, "product");
  mkdirSync(primary);
  const git = (...args: string[]) => execFileSync("git", args, { cwd: primary, stdio: "pipe" });
  git("init", "-q");
  git("-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid", "commit", "--allow-empty", "-qm", "fixture");
  git("worktree", "add", "-q", "--detach", linked);
  mkdirSync(join(product, "config"), { recursive: true });
  const routing = JSON.parse(readFileSync(resolve(import.meta.dirname, "../../../config/model-routing.json"), "utf8"));
  routing.adapters.codex.models.push({ id: "fixture", names: ["fixture"] });
  writeFileSync(join(product, "config/model-routing.json"), JSON.stringify(routing));
  copyFileSync(resolve(import.meta.dirname, "../../../config/adapter-compatibility.yaml"), join(product, "config/adapter-compatibility.yaml"));
  const owners = join(product, "skills/orchestrate/scripts");
  mkdirSync(owners, { recursive: true });
  mkdirSync(join(product, "scripts/lib"), { recursive: true });
  copyFileSync(resolve(import.meta.dirname, "../../../scripts/lib/harness-python.sh"), join(product, "scripts/lib/harness-python.sh"));
  const fixture = join(import.meta.dirname, "v2-owner-fixture.mjs");
  for (const name of ["run_dir_init.sh", "dispatch_run.py", "batch_run.py", "run_controls.py"]) {
    writeFileSync(join(owners, name), name.endsWith(".sh")
      ? `#!/bin/sh\nPROVENANT_FIXTURE_OWNER=${name} exec '${process.execPath}' '${fixture}' "$@"\n`
      : `#!/usr/bin/env python3\nimport os,sys\nos.environ['PROVENANT_FIXTURE_OWNER']=${JSON.stringify(name)}\nos.execv(${JSON.stringify(process.execPath)},[${JSON.stringify(process.execPath)},${JSON.stringify(fixture)},*sys.argv[1:]])\n`);
    chmodSync(join(owners, name), 0o755);
  }
  const python = execFileSync("python3", ["-c", "import sys;print(sys.executable)"], { encoding: "utf8" }).trim();
  const env = (cwd: string, label: string, seat: string) => ({
    ...(process.env as Record<string, string>),
    FABRIC_NODE: process.execPath,
    AGENT_FABRIC_TSX_LOADER: createRequire(import.meta.url).resolve("tsx"),
    AGENT_FABRIC_STATE_DIRECTORY: join(root, "state"),
    AGENT_FABRIC_LABEL: label,
    AGENT_FABRIC_SEAT: seat,
    PROVENANT_CHAIR: "chair-seat",
    AGENT_FABRIC_PRODUCT_ROOT: product,
    PROVENANT_FIXTURE_PID_LOG: pidLog,
    HARNESS_PYTHON: python,
    GIT_WORK_TREE: cwd,
  });
  const connect = async (cwd: string, label: string, seat: string) => {
    const client = new Client({ name: label, version: "1" });
    await client.connect(new StdioClientTransport({
      command: resolve(import.meta.dirname, "../bin/fabric-mcp"), args: [], cwd, env: env(cwd, label, seat), stderr: "pipe",
    }));
    const call = async (name: string, args: Record<string, unknown> = {}) => {
      const result = await client.callTool({ name: `fabric_${name}`, arguments: name === "dispatch" ? { detail: "full", ...args } : args });
      expect(result.isError, JSON.stringify(result)).not.toBe(true);
      return { ...(result.structuredContent as Record<string, any>), text: (result.content as any[])[0]?.text as string } as Record<string, any>;
    };
    return { client, call };
  };
  return { root, primary: realpathSync(primary), linked: realpathSync(linked), connect, env };
}

it("starts, inspects, resumes across agents, serialises and forgets a named session", async () => {
  const project = fixtureProject();
  const chair = await project.connect(project.linked, "chair-seat", "codex");
  const worker = await project.connect(project.primary, "worker-seat", "claude");
  try {
    const started = await chair.call("dispatch", { session: "Review", prompt: "first", wait_seconds: 5 });
    expect(started).toMatchObject({ status: "ok", attempt: 1, session: "Review", session_turn: "start" });
    expect(started.text).toContain("session Review start");
    const run = started.run_id as string, task = started.task_id as string;
    const inspected = await chair.call("session", { action: "inspect", name: "Review" });
    expect(inspected).toMatchObject({
      name: "Review", adapter: "codex", provider_session_id: `fixture-${run}-${task}`, run_id: run, task_id: task,
      attempt: 1, active_run_id: null, updated_by: "chair-seat",
    });
    expect(inspected.result_path).toMatch(/attempt-001\/result\.md$/u);
    // One case-sensitive name per project.
    expect(await chair.call("session", { action: "inspect", name: "review" })).toMatchObject({ error: "session_unknown" });

    // Another agent of the same project continues the provider session by name.
    const resumed = await worker.call("dispatch", { session: "Review", prompt: "second", wait_seconds: 5 });
    expect(resumed).toMatchObject({ status: "ok", run_id: run, attempt: 2, session_turn: "resume" });
    const args = JSON.parse(readFileSync(join(started.run_dir, "_owner", `${task}-args-2.json`), "utf8"));
    expect(args).toEqual(expect.arrayContaining(["--resume", run, "--task-id", task]));
    expect(await worker.call("session", { action: "inspect", name: "Review" })).toMatchObject({
      provider_session_id: `fixture-${run}-${task}`, attempt: 2, updated_by: "worker-seat",
    });
    // Resume keeps the route; a change is the existing typed rejection.
    expect(await chair.call("dispatch", { session: "Review", prompt: "x", adapter: "claude" }))
      .toMatchObject({ status: "rejected", error: "resume_route_change" });

    // Only a clean turn moves the pointer.
    expect(await chair.call("dispatch", { session: "Review", prompt: "fail", wait_seconds: 5 }))
      .toMatchObject({ status: "failed", attempt: 3 });
    expect(await chair.call("session", { action: "inspect", name: "Review" })).toMatchObject({
      attempt: 2, last_turn: { status: "failed", attempt: 3 },
    });
    expect(await chair.call("dispatch", { session: "Review", prompt: "again", wait_seconds: 5 }))
      .toMatchObject({ status: "ok", attempt: 4, session_turn: "resume" });

    // One active turn per name: a second caller and forget are refused with the active run.
    const slow = await chair.call("dispatch", { session: "Review", prompt: "slow", wait_seconds: 0 });
    expect(slow.text).toContain("session Review resume active");
    const busy = await worker.call("dispatch", { session: "Review", prompt: "x", wait_seconds: 0 });
    expect(busy).toMatchObject({ status: "rejected", error: "session_busy", active_run_id: run });
    expect(busy.text).toContain(`Wait for run ${run}`);
    expect(await worker.call("session", { action: "forget", name: "Review" }))
      .toMatchObject({ status: "rejected", error: "session_busy", active_run_id: run });
    expect((await worker.call("session", { action: "list" })).text).toContain("busy resume");
    // The active pointer is reconciled from run state: cancelling ends the turn.
    await chair.call("cancel", { id: run });
    expect(await worker.call("session", { action: "inspect", name: "Review" })).toMatchObject({
      attempt: 4, active_run_id: null, last_turn: { status: "cancelled", attempt: 5 },
    });
    const kept = (await chair.call("session", { action: "inspect", name: "Review" })).result_path as string;
    expect(await worker.call("session", { action: "forget", name: "Review" })).toMatchObject({ status: "ok", forgotten: "Review" });
    expect(await chair.call("session", { action: "inspect", name: "Review" })).toMatchObject({ error: "session_unknown" });
    expect(existsSync(kept)).toBe(true);
    // A forgotten name starts afresh.
    expect(await chair.call("dispatch", { session: "Review", prompt: "new", wait_seconds: 5 }))
      .toMatchObject({ status: "ok", attempt: 1, session_turn: "start" });
  } finally {
    await chair.client.close();
    await worker.client.close();
  }
});

it("reports continuation_unsupported and starts fresh only when asked", async () => {
  const project = fixtureProject();
  const chair = await project.connect(project.linked, "chair-seat", "codex");
  try {
    const first = await chair.call("dispatch", { session: "pilot", adapter: "copilot", prompt: "first", wait_seconds: 5 });
    expect(first).toMatchObject({ status: "ok", session_turn: "start" });
    expect(await chair.call("session", { action: "inspect", name: "pilot" })).toMatchObject({ adapter: "copilot" });
    const refused = await chair.call("dispatch", { session: "pilot", prompt: "next", wait_seconds: 5 });
    expect(refused).toMatchObject({ status: "rejected", error: "continuation_unsupported" });
    expect(refused.text).toContain("fresh: true");
    const fresh = await chair.call("dispatch", { session: "pilot", prompt: "next", fresh: true, wait_seconds: 5 });
    expect(fresh).toMatchObject({ status: "ok", attempt: 1, session_turn: "fresh" });
    expect(fresh.run_id).not.toBe(first.run_id);
    expect(readFileSync(join(fresh.run_dir, "_owner", `${fresh.task_id}-prompt-1.md`), "utf8"))
      .toMatch(new RegExp(`^Fresh session handed off from Fabric run ${first.run_id}`, "u"));
    expect(await chair.call("session", { action: "inspect", name: "pilot" })).toMatchObject({ run_id: fresh.run_id, adapter: "copilot" });

    // A native adapter whose latest attempt lost its provider session is not silently relaunched.
    await chair.call("dispatch", { session: "lost", prompt: "first", wait_seconds: 5 });
    expect(await chair.call("dispatch", { session: "lost", prompt: "lose-session", wait_seconds: 5 }))
      .toMatchObject({ status: "failed", attempt: 2 });
    expect(await chair.call("dispatch", { session: "lost", prompt: "x", wait_seconds: 5 }))
      .toMatchObject({ status: "rejected", error: "continuation_unsupported" });
    expect(await chair.call("dispatch", { prompt: "x", fresh: true }))
      .toMatchObject({ status: "rejected", error: "dispatch_conflict" });
    expect(await chair.call("dispatch", { session: "x", resume: first.run_id, prompt: "x" }))
      .toMatchObject({ status: "rejected", error: "dispatch_conflict" });

    // A launch whose caller died before recording a run is reconciled by pid, with no timer.
    const database = (await chair.call("whoami")).database as string;
    const dead = spawnSync(process.execPath, ["-e", "0"]).pid!;
    const db = new Database(database);
    const launching = db.prepare(`UPDATE sessions SET turn_status = NULL, turn_run_id = NULL, turn_attempt = 1,
      turn_kind = 'fresh', turn_pid = ? WHERE name = 'pilot'`);
    launching.run(process.pid);
    expect((await chair.call("session", { action: "inspect", name: "pilot" })).text).toContain("busy fresh launching");
    expect(await chair.call("dispatch", { session: "pilot", prompt: "x", fresh: true }))
      .toMatchObject({ status: "rejected", error: "session_busy", active_run_id: null });
    launching.run(dead);
    db.close();
    expect(await chair.call("session", { action: "inspect", name: "pilot" })).toMatchObject({
      run_id: fresh.run_id, active_run_id: null, last_turn: { status: "interrupted" },
    });
  } finally {
    await chair.client.close();
  }
});

it("drives a named session from the CLI", async () => {
  const project = fixtureProject();
  const cli = (...args: string[]) => spawnSync(resolve(import.meta.dirname, "../bin/fabric"), args, {
    cwd: project.linked, env: project.env(project.linked, "cli-seat", "codex"), encoding: "utf8",
  });
  writeFileSync(join(project.linked, "prompt.md"), "first");
  const started = cli("dispatch", "--session", "cli", "--prompt-file", "prompt.md", "--wait");
  expect(started.status, started.stderr).toBe(0);
  expect(started.stdout).toMatch(/status: ok\nsession cli codex fixture-/u);
  const resumed = cli("dispatch", "--session", "cli", "--prompt-file", "prompt.md", "--wait");
  expect(resumed.stdout).toMatch(/#2 · result/u);
  expect(JSON.parse(cli("session", "inspect", "cli").stdout)).toMatchObject({ name: "cli", attempt: 2 });
  expect(JSON.parse(cli("session", "list").stdout).sessions).toHaveLength(1);
  expect(JSON.parse(cli("session", "forget", "cli").stdout)).toMatchObject({ forgotten: "cli" });
  expect(cli("session", "inspect", "cli").stderr).toContain("no session cli");
});
