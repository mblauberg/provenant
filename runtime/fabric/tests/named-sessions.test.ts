import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import Database from "better-sqlite3";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { afterEach, expect, it } from "vitest";

import { Store } from "../src/store.js";
import { digest } from "../src/surface.js";

async function until<T>(probe: () => Promise<T> | T, done: (value: T) => boolean): Promise<T> {
  for (let tries = 0; ; tries++) {
    const value = await probe();
    if (done(value) || tries > 250) return value;
    await new Promise((settle) => setTimeout(settle, 40));
  }
}

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
    // A name holds one provider session, never a council.
    expect(await chair.call("dispatch", { session: "Board", prompt: "x", council: 2 }))
      .toMatchObject({ status: "rejected", error: "session_invalid" });
    // Resume keeps the route; a change is the existing typed rejection.
    expect(await chair.call("dispatch", { session: "Review", prompt: "x", adapter: "claude" }))
      .toMatchObject({ status: "rejected", error: "resume_route_change" });

    // Only a clean turn moves the pointer, and the next turn continues from it.
    expect(await chair.call("dispatch", { session: "Review", prompt: "fail-new-session", wait_seconds: 5 }))
      .toMatchObject({ status: "failed", attempt: 3, session_id: "other-3" });
    expect(await chair.call("session", { action: "inspect", name: "Review" })).toMatchObject({
      attempt: 2, provider_session_id: `fixture-${run}-${task}`, last_turn: { status: "failed", attempt: 3 },
    });
    expect(await chair.call("dispatch", { session: "Review", prompt: "again", wait_seconds: 5 }))
      .toMatchObject({ status: "ok", attempt: 4, session_turn: "resume", session_id: `fixture-${run}-${task}` });
    expect(JSON.parse(readFileSync(join(started.run_dir, "_owner", `${task}-args-4.json`), "utf8")))
      .toEqual(expect.arrayContaining(["--resume-attempt", "2", "--require-session", `fixture-${run}-${task}`]));

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
}, 60_000);

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

    // A failed turn that lost its session does not strand the name: the next
    // turn continues the last clean attempt's provider session.
    const lost = await chair.call("dispatch", { session: "lost", prompt: "first", wait_seconds: 5 });
    expect(await chair.call("dispatch", { session: "lost", prompt: "lose-session", wait_seconds: 5 }))
      .toMatchObject({ status: "failed", attempt: 2 });
    expect(await chair.call("dispatch", { session: "lost", prompt: "x", wait_seconds: 5 }))
      .toMatchObject({ status: "ok", attempt: 3, session_id: `fixture-${lost.run_id}-${lost.task_id}` });
    // A provider that no longer has the session is reported, never silently relaunched.
    const gone = await chair.call("dispatch", { session: "lost", prompt: "no-conversation", wait_seconds: 5 });
    expect(gone).toMatchObject({ status: "rejected", error: "continuation_unsupported", attempt: 4 });
    expect(gone.text).toContain("fresh: true");
    expect(await chair.call("session", { action: "inspect", name: "lost" })).toMatchObject({
      attempt: 3, last_turn: { status: "continuation_unsupported", attempt: 4 },
    });

    // fresh hands off the last clean attempt's result, not a later failed one.
    const marks = await chair.call("dispatch", { session: "marks", prompt: "mark", wait_seconds: 5 });
    expect(await chair.call("dispatch", { session: "marks", prompt: "mark-fail", wait_seconds: 5 }))
      .toMatchObject({ status: "failed", attempt: 2 });
    const handed = await chair.call("dispatch", { session: "marks", prompt: "next", fresh: true, wait_seconds: 5 });
    const brief = readFileSync(join(handed.run_dir, "_owner", `${handed.task_id}-prompt-1.md`), "utf8");
    expect(brief).toContain(`result of ${marks.run_id}/${marks.task_id}#1`);
    expect(brief).not.toContain(`${marks.task_id}#2`);
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
}, 60_000);

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
}, 60_000);

it("keeps a turn busy through the owner's fallback and advances to the attempt that succeeded", async () => {
  const project = fixtureProject();
  const chair = await project.connect(project.linked, "chair-seat", "codex");
  const worker = await project.connect(project.primary, "worker-seat", "claude");
  try {
    const launched = await chair.call("dispatch", { session: "fb", prompt: "fallback-slow", wait_seconds: 0 });
    const run = launched.id as string;
    const status = await until(() => chair.call("status", { ids: [run], detail: "full" }),
      (value) => value.runs?.[0]?.attempt === 2);
    const row = status.runs[0];
    expect(row.attempts[0]).toMatchObject({ attempt: 1, state: "terminal", status: "failed" });
    // Attempt 1 failed, but the turn is the whole invocation: the fallback still runs.
    expect(await worker.call("session", { action: "inspect", name: "fb" })).toMatchObject({ active_run_id: run });
    expect(await worker.call("session", { action: "forget", name: "fb" }))
      .toMatchObject({ status: "rejected", error: "session_busy", active_run_id: run });
    expect(await worker.call("dispatch", { session: "fb", prompt: "x" }))
      .toMatchObject({ status: "rejected", error: "session_busy", active_run_id: run });
    writeFileSync(join(row.run_dir, "release"), "");
    await chair.call("status", { ids: [run], wait_seconds: 10 });
    expect(await until(() => worker.call("session", { action: "inspect", name: "fb" }), (value) => value.active_run_id === null))
      .toMatchObject({ run_id: run, attempt: 2, active_run_id: null, last_turn: { status: "ok", attempt: 1 } });
  } finally {
    await chair.client.close();
    await worker.client.close();
  }
}, 60_000);

it("keeps a CLI turn whose waiting caller was killed after launch", async () => {
  const project = fixtureProject();
  const chair = await project.connect(project.linked, "chair-seat", "codex");
  try {
    writeFileSync(join(project.linked, "slow.md"), "slow");
    const waiting = spawn(resolve(import.meta.dirname, "../bin/fabric"),
      ["dispatch", "--session", "kill", "--prompt-file", "slow.md", "--wait"],
      { cwd: project.linked, env: project.env(project.linked, "cli-seat", "codex"), stdio: "ignore" });
    const active = await until(() => chair.call("session", { action: "inspect", name: "kill" }),
      (value) => typeof value.active_run_id === "string");
    const run = active.active_run_id as string;
    expect(run).toMatch(/^mcp-/u);
    // The turn records its run before the owner starts; kill the caller once the owner runs.
    await until(() => chair.call("status", { ids: [run], detail: "full" }),
      (value) => existsSync(join(String(value.runs?.[0]?.run_dir), "dispatch-owner.json")));
    waiting.kill("SIGKILL");
    await new Promise((settle) => waiting.once("exit", settle));
    // The detached owner still runs: the name stays busy on owner evidence.
    expect(await chair.call("session", { action: "inspect", name: "kill" })).toMatchObject({ active_run_id: run });
    expect(await chair.call("dispatch", { session: "kill", prompt: "x" }))
      .toMatchObject({ status: "rejected", error: "session_busy", active_run_id: run });
    await chair.call("cancel", { id: run });
    expect(await until(() => chair.call("session", { action: "inspect", name: "kill" }), (value) => value.active_run_id === null))
      .toMatchObject({ active_run_id: null, last_turn: { status: "cancelled", run_id: run } });
  } finally {
    await chair.client.close();
  }
}, 60_000);

it("refuses a claim planned from a turn another caller has since settled", () => {
  const root = mkdtempSync(join(tmpdir(), "fabric-session-claim-"));
  roots.add(root);
  const store = new Store(join(root, "fabric.db"));
  const who = { project: root, cwd: root, agentId: "a", provider: "codex" };
  try {
    store.announce(who);
    const first = store.claimSessionTurn(who, "n", null, "start", process.pid);
    if (!("claim" in first)) throw new Error("first claim refused");
    store.bindSessionTurn(who, "n", first.claim, { runId: "mcp-one", taskId: "t", attempt: 1 });
    store.settleSessionTurn(who, "n", first.claim, "ok",
      { adapter: "codex", providerSessionId: "s", runId: "mcp-one", taskId: "t", attempt: 1, resultPath: null });
    // Caller A plans attempt 2 from this row...
    const observed = store.session(root, "n")!.turnClaim;
    // ...while caller B takes, launches and settles attempt 2 first.
    const second = store.claimSessionTurn(who, "n", observed, "resume", process.pid);
    if (!("claim" in second)) throw new Error("second claim refused");
    store.bindSessionTurn(who, "n", second.claim, { runId: "mcp-one", taskId: "t", attempt: 2 });
    store.settleSessionTurn(who, "n", second.claim, "ok",
      { adapter: "codex", providerSessionId: "s", runId: "mcp-one", taskId: "t", attempt: 2, resultPath: null });
    // The run id is unchanged, but A's plan is stale.
    expect(store.claimSessionTurn(who, "n", observed, "resume", process.pid)).toMatchObject({ conflict: { attempt: 2 } });
    expect(store.claimSessionTurn(who, "fresh-name", observed, "start", process.pid)).toMatchObject({ conflict: undefined });
  } finally {
    store.close();
  }
});

it("keeps a turn busy between fallback attempts when the owner's start time is unknown", async () => {
  const project = fixtureProject();
  const chair = await project.connect(project.linked, "chair-seat", "codex");
  try {
    const run = (await chair.call("dispatch", { session: "gap", prompt: "fallback-gap", wait_seconds: 0 })).id as string;
    const row = (await until(() => chair.call("status", { ids: [run], detail: "full" }),
      (value) => value.runs?.[0]?.status === "failed")).runs[0];
    // Every attempt so far is terminal and the owner's identity cannot be verified.
    const recordPath = join(row.run_dir, "dispatch-owner.json");
    const record = JSON.parse(readFileSync(recordPath, "utf8"));
    writeFileSync(recordPath, JSON.stringify({ ...record, owner_started_at: null }));
    expect(await chair.call("session", { action: "inspect", name: "gap" })).toMatchObject({ active_run_id: run });
    expect(await chair.call("session", { action: "forget", name: "gap" }))
      .toMatchObject({ status: "rejected", error: "session_busy", active_run_id: run });
    writeFileSync(join(row.run_dir, "release"), "");
    await chair.call("status", { ids: [run], wait_seconds: 10 });
    expect(await until(() => chair.call("session", { action: "inspect", name: "gap" }), (value) => value.active_run_id === null))
      .toMatchObject({ run_id: run, attempt: 2, last_turn: { status: "ok" } });
  } finally {
    await chair.client.close();
  }
}, 60_000);

it("never runs a second turn on a name after a launch it could not record", async () => {
  const project = fixtureProject();
  const chair = await project.connect(project.linked, "chair-seat", "codex");
  const cli = (...args: string[]) => spawnSync(resolve(import.meta.dirname, "../bin/fabric"), args, {
    cwd: project.linked, env: project.env(project.linked, "cli-seat", "codex"), encoding: "utf8",
  });
  const live = () => (JSON.parse(cli("dispatch", "list", "--json").stdout).runs as { running: boolean }[])
    .filter((run) => run.running).length;
  try {
    const db = new Database((await chair.call("whoami")).database as string);
    db.exec(`CREATE TRIGGER refuse_bind BEFORE UPDATE OF turn_run_id ON sessions
      WHEN NEW.turn_run_id IS NOT NULL BEGIN SELECT RAISE(ABORT, 'bind refused'); END`);
    writeFileSync(join(project.linked, "slow.md"), "slow");
    // The CLI launcher cannot record its turn and then exits.
    const first = cli("dispatch", "--session", "unrec", "--prompt-file", "slow.md");
    db.exec("DROP TRIGGER refuse_bind");
    db.close();
    const second = await chair.call("dispatch", { session: "unrec", prompt: "slow", wait_seconds: 0 });
    // One active turn per name: a second turn may start only if the first launched nothing.
    expect(live(), `${first.stdout}${first.stderr}`).toBeLessThanOrEqual(1);
    expect(first.stdout).toContain("session_unrecorded");
    expect(first.stdout).toContain("nothing was launched");
    // A launched run reads running, or null once its owner has written the attempt.
    expect(second).toMatchObject({ session: "unrec", session_turn: "start" });
    expect(second.error).toBeUndefined();
    expect(second.id).toMatch(/^mcp-/u);
    expect(await chair.call("dispatch", { session: "unrec", prompt: "x" }))
      .toMatchObject({ status: "rejected", error: "session_busy", active_run_id: second.id });
    await chair.call("cancel", { id: second.id });
  } finally {
    await chair.client.close();
  }
}, 60_000);

it("keeps a turn busy while its owner lives without an owner record", async () => {
  const project = fixtureProject();
  const chair = await project.connect(project.linked, "chair-seat", "codex");
  try {
    writeFileSync(join(project.linked, "queued.md"), "admission-slow");
    // The CLI launcher cannot publish the owner record, starts the owner anyway and exits.
    const launched = spawnSync(resolve(import.meta.dirname, "../bin/fabric"),
      ["dispatch", "--session", "queue", "--prompt-file", "queued.md"], {
        cwd: project.linked, encoding: "utf8", env: {
          ...project.env(project.linked, "cli-seat", "codex"), PROVENANT_OWNER_RECORD_FAULT: "1",
          NODE_OPTIONS: `${process.env.NODE_OPTIONS ?? ""} --import=${resolve(import.meta.dirname, "spawn-fault-preload.mjs")}`.trim(),
        },
      });
    expect(launched.status, launched.stdout + launched.stderr).toBe(0);
    const run = launched.stdout.split("\n")[0]!;
    expect(run).toMatch(/^mcp-/u);
    // The owner waits for memory admission: its attempt is queued and nothing records the owner.
    const row = (await until(() => chair.call("status", { ids: [run], detail: "full" }),
      (value) => value.runs?.[0]?.state === "queued" && value.runs[0].attempt === 1)).runs[0];
    expect(row).toMatchObject({ state: "queued", attempt: 1 });
    expect(existsSync(join(row.run_dir, "dispatch-owner.json"))).toBe(false);
    expect(await chair.call("session", { action: "inspect", name: "queue" })).toMatchObject({ active_run_id: run });
    expect(await chair.call("session", { action: "forget", name: "queue" }))
      .toMatchObject({ status: "rejected", error: "session_busy", active_run_id: run });
    expect(await chair.call("dispatch", { session: "queue", prompt: "x" }))
      .toMatchObject({ status: "rejected", error: "session_busy", active_run_id: run });
    writeFileSync(join(row.run_dir, "release"), "");
    expect(await until(() => chair.call("session", { action: "inspect", name: "queue" }), (value) => value.active_run_id === null))
      .toMatchObject({ run_id: run, attempt: 1, active_run_id: null, last_turn: { status: "ok" } });
  } finally {
    await chair.client.close();
  }
}, 60_000);

it("keeps a named session's line in a running turn's rebuilt text", () => {
  const text = digest({ state: "running", run_id: "mcp-abc123", digest: "stale",
    session_digest: "\n  session s resume active" });
  expect(text).toBe(`running mcp-abc123 · fabric_status{ids:["mcp-abc123"],wait_seconds:55}\n  session s resume active`);
});

it("recovers a resumed turn whose launcher failed between recording it and starting its owner", async () => {
  const project = fixtureProject();
  const chair = await project.connect(project.linked, "chair-seat", "codex");
  const faulty = (fault: string) => spawnSync(resolve(import.meta.dirname, "../bin/fabric"),
    ["dispatch", "--session", "crash", "--prompt-file", "again.md", "--wait"], {
      cwd: project.linked, encoding: "utf8", env: {
        ...project.env(project.linked, "cli-seat", "codex"), PROVENANT_SPAWN_FAULT: fault,
        NODE_OPTIONS: `${process.env.NODE_OPTIONS ?? ""} --import=${resolve(import.meta.dirname, "spawn-fault-preload.mjs")}`.trim(),
      },
    });
  try {
    const first = await chair.call("dispatch", { session: "crash", prompt: "first", wait_seconds: 5 });
    expect(first).toMatchObject({ status: "ok", attempt: 1 });
    writeFileSync(join(project.linked, "again.md"), "again");

    // A caught failure closes the turn at once; the pointer stays on the last clean attempt.
    const thrown = faulty("throw");
    expect(thrown.stdout + thrown.stderr).toContain("spawn refused");
    expect(await chair.call("session", { action: "inspect", name: "crash" })).toMatchObject({
      run_id: first.run_id, attempt: 1, active_run_id: null, last_turn: { attempt: 2, status: "rejected" },
    });
    expect(await chair.call("dispatch", { session: "crash", prompt: "again", wait_seconds: 5 }))
      .toMatchObject({ status: "ok", attempt: 2, session_turn: "resume" });

    // A launcher that dies at the same point announced an attempt no owner will run:
    // the run lifecycle closes it, so the turn settles and resume works on it.
    expect(faulty("kill").signal).toBe("SIGKILL");
    expect((await chair.call("status", { ids: [first.run_id], detail: "full" })).runs[0])
      .toMatchObject({ attempt: 3, state: "terminal", status: "interrupted" });
    expect(await chair.call("session", { action: "inspect", name: "crash" })).toMatchObject({
      run_id: first.run_id, attempt: 2, active_run_id: null, last_turn: { attempt: 3, status: "interrupted" },
    });
    expect(await chair.call("dispatch", { session: "crash", prompt: "again", wait_seconds: 5 }))
      .toMatchObject({ status: "ok", run_id: first.run_id, attempt: 3, session_turn: "resume" });

    // So does a handoff to a fresh session.
    expect(faulty("kill").signal).toBe("SIGKILL");
    const fresh = await chair.call("dispatch", { session: "crash", prompt: "first", fresh: true, wait_seconds: 5 });
    expect(fresh).toMatchObject({ status: "ok", attempt: 1, session_turn: "fresh" });
    expect(fresh.run_id).not.toBe(first.run_id);
    expect(await chair.call("session", { action: "forget", name: "crash" })).toMatchObject({ forgotten: "crash" });
  } finally {
    await chair.client.close();
  }
}, 60_000);
