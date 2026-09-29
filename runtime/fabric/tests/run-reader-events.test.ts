import { mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { afterEach, expect, it } from "vitest";

import { readRuns, readEvents } from "../src/run-reader.js";

const roots: string[] = [];
afterEach(() => { for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true }); });

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "fabric-reader-"));
  roots.push(root);
  return root;
}

function attempt(workspace: string, tree: string, taskId: string, state: string, status: string | null, runName?: string) {
  const run = runName ?? `20260924-1012-dispatch-fixture-${taskId.slice(-6).padStart(6, "a")}`;
  const dir = join(workspace, ".agent-run", "runs", run, tree, taskId, "attempt-001");
  mkdirSync(dir, { recursive: true });
  const row = JSON.parse(readFileSync(new URL("fixtures/attempt.json", import.meta.url), "utf8"));
  Object.assign(row, { run_id: `mcp-${taskId.slice(-6).padStart(6, "a")}`, task_id: taskId, state, status,
    started_at: new Date().toISOString(), ended_at: new Date().toISOString(),
    paths: { result: `${tree}/${taskId}/attempt-001/result.md` }, worktree: "/repo/worktree" });
  writeFileSync(join(dir, "attempt.json"), JSON.stringify(row));
  writeFileSync(join(dir, "result.md"), "result");
  return { row, run };
}

const product = resolve(import.meta.dirname, "../../..");
const tsxLoader = createRequire(import.meta.url).resolve("tsx");

function laneWaitForSeat(cwd: string, state: string, seat: string, ...ids: string[]) {
  return spawnSync("python3", [join(product, "scripts/provenant"), "lanes", "--wait", ...ids], {
    cwd, encoding: "utf8", timeout: 15_000,
    env: { ...process.env, AGENT_FABRIC_PRODUCT_ROOT: product, AGENT_FABRIC_TSX_LOADER: tsxLoader,
      AGENT_FABRIC_STATE_DIRECTORY: state, AGENT_FABRIC_SEAT: "codex", AGENT_FABRIC_LABEL: seat },
  });
}

function laneWait(cwd: string, state: string, ...ids: string[]) {
  return laneWaitForSeat(cwd, state, "wait-seat", ...ids);
}

function laneWaitEnv(state: string, seat = "wait-seat") {
  return { ...process.env, AGENT_FABRIC_PRODUCT_ROOT: product, AGENT_FABRIC_TSX_LOADER: tsxLoader,
    AGENT_FABRIC_STATE_DIRECTORY: state, AGENT_FABRIC_SEAT: "codex", AGENT_FABRIC_LABEL: seat };
}

function startWait(cwd: string, state: string, ...ids: string[]) {
  const child = spawn("python3", [join(product, "scripts/provenant"), "lanes", "--wait", ...ids], {
    cwd, env: laneWaitEnv(state), stdio: ["ignore", "pipe", "pipe"],
  });
  const output = { stdout: "", stderr: "", code: undefined as number | null | undefined };
  child.stdout.setEncoding("utf8").on("data", (chunk: string) => { output.stdout += chunk; });
  child.stderr.setEncoding("utf8").on("data", (chunk: string) => { output.stderr += chunk; });
  const exited = new Promise<number | null>((resolveExit, reject) => {
    child.once("error", reject);
    child.once("exit", (code) => { output.code = code; resolveExit(code); });
  });
  return { output, exited };
}

const pause = (ms: number) => new Promise((resolvePause) => setTimeout(resolvePause, ms));

it("reads both receipt layouts through one versioned root-relative run interface", async () => {
  const workspace = fixture();
  attempt(workspace, "tasks", "modern", "terminal", "ok");
  attempt(workspace, "dispatch/tasks", "legacy", "terminal", "failed");
  const result = await readRuns(workspace);
  expect(result.schema).toBe("fabric.runs.v1");
  expect(result.status).toBe("ok");
  expect(result.runs.map((row) => row.task_id).sort()).toEqual(["legacy", "modern"]);
  for (const row of result.runs) {
    expect(row.receipt_path).toMatch(/^runs\//u);
    expect(row.result_path).toMatch(/^runs\//u);
    expect(row.route).toContain("codex/");
    expect(row.writer).toBe(false);
    expect(row.worktree).toBe("/repo/worktree");
  }
});

it("emits terminal and input_required changes with a cursor", async () => {
  const workspace = fixture();
  attempt(workspace, "tasks", "done", "terminal", "ok");
  attempt(workspace, "tasks", "needs-input", "input_required", "input_required");
  const first = await readEvents(workspace);
  expect(first.events.filter((event) => event.type === "task_state").map((event) => "state" in event ? event.state : null).sort()).toEqual(["input_required", "terminal"]);
  expect((await readEvents(workspace, first.cursor)).events).toEqual([]);
});

it("reports every retained terminal attempt and an inbox message once", async () => {
  const workspace = fixture();
  const { row, run } = attempt(workspace, "tasks", "retry", "terminal", "failed");
  const next = join(workspace, ".agent-run", "runs", run, "tasks", "retry", "attempt-002");
  mkdirSync(next);
  writeFileSync(join(next, "attempt.json"), JSON.stringify({ ...row, attempt: 2, status: "ok" }));
  const message = { messageId: "message-1", from: "worker", taskId: "retry", body: "A long\nupdate", kind: "note",
    conversationId: "conversation-1", replyTo: null, at: new Date().toISOString(), claimId: null, claimExpiresAt: null };
  const first = await readEvents(workspace, undefined, [message]);
  expect(first.events.filter((event) => event.type === "task_state").map((event) => event.status))
    .toEqual(expect.arrayContaining(["failed", "ok"]));
  expect(first.events.find((event) => event.type === "inbox_message")).toMatchObject({ id: "message-1", preview: "A long update" });
  expect((await readEvents(workspace, first.cursor, [message])).events).toEqual([]);
});

it("keeps the event cursor bounded across hundreds of retained attempts", async () => {
  const workspace = fixture();
  const { row, run } = attempt(workspace, "tasks", "many-attempts", "terminal", "failed");
  const taskDir = join(workspace, ".agent-run", "runs", run, "tasks", "many-attempts");
  for (let number = 2; number <= 40; number += 1) {
    const dir = join(taskDir, `attempt-${String(number).padStart(3, "0")}`);
    mkdirSync(dir);
    writeFileSync(join(dir, "attempt.json"), JSON.stringify({ ...row, attempt: number }));
  }
  const first = await readEvents(workspace);
  expect(first.events.filter((event) => event.type === "task_state")).toHaveLength(readdirSync(taskDir).length);
  expect(first.cursor.length).toBeLessThan(8192);
  expect((await readEvents(workspace, first.cursor)).events).toEqual([]);
  const next = join(taskDir, "attempt-501");
  mkdirSync(next);
  writeFileSync(join(next, "attempt.json"), JSON.stringify({ ...row, attempt: 501, status: "ok" }));
  expect((await readEvents(workspace, first.cursor)).events).toMatchObject([{ type: "task_state", status: "ok" }]);
});

it("does not publish a result path outside its run", async () => {
  const workspace = fixture();
  const { row, run } = attempt(workspace, "tasks", "escaped", "terminal", "ok");
  row.paths.result = "../../outside.md";
  writeFileSync(join(workspace, ".agent-run", "runs", run, "tasks", "escaped", "attempt-001", "attempt.json"), JSON.stringify(row));
  expect((await readRuns(workspace)).runs[0]?.result_path).toBeNull();
});

it("rejects a missing result below a symlink that escapes its run", async () => {
  const workspace = fixture();
  const { row, run } = attempt(workspace, "tasks", "linked", "terminal", "ok");
  const runDir = join(workspace, ".agent-run", "runs", run);
  symlinkSync(workspace, join(runDir, "escape"));
  row.paths.result = "escape/missing/result.md";
  writeFileSync(join(runDir, "tasks", "linked", "attempt-001", "attempt.json"), JSON.stringify(row));
  expect((await readRuns(workspace)).runs[0]?.result_path).toBeNull();
});

it("normalises an alias for a missing path within the run", async () => {
  const workspace = fixture();
  const { row, run } = attempt(workspace, "tasks", "alias", "terminal", "ok");
  const runDir = join(workspace, ".agent-run", "runs", run);
  const alias = join(workspace, "run-alias");
  symlinkSync(runDir, alias);
  row.paths.result = join(alias, "tasks", "alias", "attempt-001", "pending.md");
  writeFileSync(join(runDir, "tasks", "alias", "attempt-001", "attempt.json"), JSON.stringify(row));
  expect((await readRuns(workspace)).runs[0]?.result_path).toBe(`runs/${run}/tasks/alias/attempt-001/pending.md`);
});

it.skipIf(process.platform !== "darwin" || !realpathSync(tmpdir()).startsWith("/private/var/"))("normalises a missing /var path against the /private/var run root", async () => {
  const workspace = fixture();
  const canonical = realpathSync(workspace);
  expect(canonical).toMatch(/^\/private\/var\//u);
  const { row, run } = attempt(workspace, "tasks", "var-alias", "terminal", "ok");
  const runDir = join(workspace, ".agent-run", "runs", run);
  row.paths.result = join(canonical.replace(/^\/private\/var\//u, "/var/"),
    ".agent-run", "runs", run, "tasks", "var-alias", "attempt-001", "pending.md");
  writeFileSync(join(runDir, "tasks", "var-alias", "attempt-001", "attempt.json"), JSON.stringify(row));
  expect((await readRuns(workspace)).runs[0]?.result_path).toBe(`runs/${run}/tasks/var-alias/attempt-001/pending.md`);
});

it("finds a receipt from the known layout when an attempt has no result", async () => {
  const workspace = fixture();
  const { row, run } = attempt(workspace, "tasks", "early-failure", "terminal", "failed");
  row.paths = {};
  const dir = join(workspace, ".agent-run", "runs", run, "tasks", "early-failure", "attempt-001");
  rmSync(join(dir, "result.md"));
  writeFileSync(join(dir, "attempt.json"), JSON.stringify(row));
  expect((await readRuns(workspace)).runs[0]?.receipt_path).toBe(`runs/${run}/tasks/early-failure/attempt-001/attempt.json`);
});

it("exits an event follower after its run becomes idle", () => {
  const workspace = fixture();
  attempt(workspace, "tasks", "finished", "terminal", "ok");
  const product = resolve(import.meta.dirname, "../../..");
  const result = spawnSync("python3", [join(product, "scripts/provenant"), "events", "--follow", "--until-idle"], {
    cwd: workspace, encoding: "utf8", timeout: 5000,
    env: { ...process.env, AGENT_FABRIC_PRODUCT_ROOT: product,
      AGENT_FABRIC_STATE_DIRECTORY: join(workspace, "state"),
      AGENT_FABRIC_TSX_LOADER: createRequire(import.meta.url).resolve("tsx") },
  });
  expect(result.status, result.stderr).toBe(0);
  expect(result.stdout).toContain('"type":"task_state"');
});

it("exits an event follower after an input_required attempt", () => {
  const workspace = fixture();
  attempt(workspace, "tasks", "needs-input", "input_required", "input_required");
  const product = resolve(import.meta.dirname, "../../..");
  const result = spawnSync("python3", [join(product, "scripts/provenant"), "events", "--follow", "--until-idle"], {
    cwd: workspace, encoding: "utf8", timeout: 2000,
    env: { ...process.env, AGENT_FABRIC_PRODUCT_ROOT: product,
      AGENT_FABRIC_STATE_DIRECTORY: join(workspace, "state"),
      AGENT_FABRIC_TSX_LOADER: createRequire(import.meta.url).resolve("tsx") },
  });
  expect(result.status, result.stderr).toBe(0);
  expect(result.stdout).toContain('"state":"input_required"');
});

it("keeps an event follower open by default", async () => {
  const workspace = fixture();
  attempt(workspace, "tasks", "finished", "terminal", "ok");
  const product = resolve(import.meta.dirname, "../../..");
  const child = spawn("python3", [join(product, "scripts/provenant"), "events", "--follow"], {
    cwd: workspace,
    env: { ...process.env, AGENT_FABRIC_PRODUCT_ROOT: product,
      AGENT_FABRIC_STATE_DIRECTORY: join(workspace, "state"),
      AGENT_FABRIC_TSX_LOADER: createRequire(import.meta.url).resolve("tsx") },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  child.stdout.setEncoding("utf8").on("data", (chunk: string) => { stdout += chunk; });
  const exited = new Promise<number | null>((resolveExit) => child.once("exit", resolveExit));
  const deadline = Date.now() + 2000;
  while (!stdout.includes('"type":"task_state"') && Date.now() < deadline)
    await new Promise((resolve) => setTimeout(resolve, 20));
  child.kill("SIGTERM");
  expect(await exited).toBeNull();
  expect(stdout).toContain('"type":"task_state"');
}, 3000);

it("exposes the same run schema through the provenant lanes command", () => {
  const workspace = fixture();
  attempt(workspace, "tasks", "cli-task", "terminal", "ok");
  const product = resolve(import.meta.dirname, "../../..");
  const result = spawnSync("python3", [join(product, "scripts/provenant"), "lanes", "--json"], {
    cwd: workspace, encoding: "utf8", env: { ...process.env, AGENT_FABRIC_PRODUCT_ROOT: product,
      AGENT_FABRIC_TSX_LOADER: createRequire(import.meta.url).resolve("tsx") },
  });
  expect(result.status, result.stderr).toBe(0);
  expect(JSON.parse(result.stdout)).toMatchObject({ schema: "fabric.runs.v1", status: "ok", runs: [{ task_id: "cli-task" }] });
});

it("reports omitted lanes in text and JSON and lets an id select a lane past the cap", () => {
  const workspace = fixture();
  const { row: older, run: olderRun } = attempt(workspace, "tasks", "must-show", "running", "running");
  older.started_at = "2026-09-20T10:00:00.000Z";
  writeFileSync(join(workspace, ".agent-run", "runs", olderRun,
    "tasks", "must-show", "attempt-001", "attempt.json"), JSON.stringify(older));
  for (let index = 0; index < 22; index += 1)
    attempt(workspace, "tasks", `cap-${String(index).padStart(3, "0")}`, "terminal", "ok");
  const product = resolve(import.meta.dirname, "../../..");
  const env = { ...process.env, AGENT_FABRIC_PRODUCT_ROOT: product,
    AGENT_FABRIC_TSX_LOADER: createRequire(import.meta.url).resolve("tsx") };
  const text = spawnSync("python3", [join(product, "scripts/provenant"), "lanes"], {
    cwd: workspace, encoding: "utf8", env,
  });
  expect(text.status, text.stderr).toBe(0);
  expect(text.stdout.split("\n", 1)[0]).toContain("must-show");
  const json = spawnSync("python3", [join(product, "scripts/provenant"), "lanes", "--json"], {
    cwd: workspace, encoding: "utf8", env,
  });
  expect(json.status, json.stderr).toBe(0);
  const listed = JSON.parse(json.stdout);
  expect(listed.omitted).toBeGreaterThan(0);
  expect(listed.omitted_hint).toBeTruthy();
  expect(listed.runs[0]).toMatchObject({ task_id: "must-show", state: "running" });

  const selected = spawnSync("python3", [join(product, "scripts/provenant"), "lanes", "--json", "cap-000"], {
    cwd: workspace, encoding: "utf8", env,
  });
  expect(selected.status, selected.stderr).toBe(0);
  expect(JSON.parse(selected.stdout).runs.map((row: { task_id: string }) => row.task_id)).toContain("cap-000");
}, 20_000);

it("waits for a running lane to become terminal and prints one compact row", async () => {
  const workspace = fixture();
  attempt(workspace, "tasks", "pre-existing-done", "terminal", "ok");
  const { row: older, run: olderRun } = attempt(
    workspace, "tasks", "wait-for-it", "terminal", "ok", "20260920-1000-dispatch-older-run",
  );
  older.started_at = "2026-09-20T10:00:00.000Z";
  older.run_id = "mcp-older-run";
  writeFileSync(join(workspace, ".agent-run", "runs", olderRun, "tasks", "wait-for-it", "attempt-001", "attempt.json"), JSON.stringify(older));
  const { row, run } = attempt(workspace, "tasks", "wait-for-it", "running", "running");
  const resultFile = join(workspace, ".agent-run", "runs", run, "tasks", "wait-for-it", "attempt-001", "attempt.json");
  const state = join(workspace, "state");
  const child = spawn("python3", [join(product, "scripts/provenant"), "lanes", "--wait", "wait-for-it"], {
    cwd: workspace,
    env: laneWaitEnv(state),
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8").on("data", (chunk: string) => { stdout += chunk; });
  child.stderr.setEncoding("utf8").on("data", (chunk: string) => { stderr += chunk; });
  const exited = new Promise<number | null>((resolveExit, reject) => {
    child.once("error", reject);
    child.once("exit", (code) => resolveExit(code));
  });
  setTimeout(() => {
    writeFileSync(resultFile, JSON.stringify({ ...row, state: "terminal", status: "ok" }));
    attempt(workspace, "tasks", "newly-finished", "terminal", "ok");
  }, 3000);
  expect(await exited).toBe(0);
  expect(stderr).toBe("");
  expect(stdout).toContain("wait-for-it");
  expect(stdout).not.toContain("newly-finished");
  expect(stdout).not.toContain("pre-existing-done");
  expect(stdout).toContain("result.md");
}, 12_000);

it("reports lanes completed between waits once, then keeps the no-running message", async () => {
  const workspace = fixture();
  const state = join(workspace, "state");
  const { row, run } = attempt(workspace, "tasks", "first-done", "running", "running");
  const firstReceipt = join(workspace, ".agent-run", "runs", run,
    "tasks", "first-done", "attempt-001", "attempt.json");
  const child = spawn("python3", [join(product, "scripts/provenant"), "lanes", "--wait"], {
    cwd: workspace, env: laneWaitEnv(state), stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8").on("data", (chunk: string) => { stdout += chunk; });
  child.stderr.setEncoding("utf8").on("data", (chunk: string) => { stderr += chunk; });
  const exited = new Promise<number | null>((resolveExit, reject) => {
    child.once("error", reject);
    child.once("exit", (code) => resolveExit(code));
  });
  await new Promise((resolve) => setTimeout(resolve, 100));
  writeFileSync(firstReceipt, JSON.stringify({ ...row, state: "terminal", status: "ok" }));
  expect(await exited).toBe(0);
  expect(stderr).toBe("");
  expect(stdout).toContain("first-done");

  attempt(workspace, "tasks", "between-waits", "terminal", "ok");
  const next = laneWait(workspace, state);
  expect(next.status, next.stderr).toBe(0);
  expect(next.stdout).toContain("between-waits");
  expect(next.stdout).not.toContain("first-done");
}, 12_000);

it("reports a pre-existing terminal lane once to the same seat", () => {
  const workspace = fixture();
  const state = join(workspace, "state");
  attempt(workspace, "tasks", "already-done", "terminal", "ok");
  // Retention bounds the scan: a first wait does not replay lanes older than a day.
  const { row, run } = attempt(workspace, "tasks", "long-retired", "terminal", "ok");
  row.started_at = "2020-01-01T00:00:00.000Z";
  row.ended_at = "2020-01-01T00:00:01.000Z";
  writeFileSync(join(workspace, ".agent-run", "runs", run,
    "tasks", "long-retired", "attempt-001", "attempt.json"), JSON.stringify(row));
  const first = laneWait(workspace, state);
  expect(first.status, first.stderr).toBe(0);
  expect(first.stdout).toContain("already-done");
  expect(first.stdout).not.toContain("long-retired");

  const second = laneWait(workspace, state);
  expect(second.status, second.stderr).toBe(0);
  expect(second.stdout).toBe("no lanes are running\n");

  const otherSeat = laneWaitForSeat(workspace, state, "other-seat");
  expect(otherSeat.status, otherSeat.stderr).toBe(0);
  expect(otherSeat.stdout).toContain("already-done");
}, 30_000);

it("waits for project lanes when invoked from a registered worktree", async () => {
  const workspace = fixture();
  const project = join(workspace, "project");
  const linked = join(project, ".worktrees", "linked");
  mkdirSync(project, { recursive: true });
  execFileSync("git", ["init", "--quiet"], { cwd: project });
  execFileSync("git", ["config", "user.email", "fabric@example.invalid"], { cwd: project });
  execFileSync("git", ["config", "user.name", "Fabric test"], { cwd: project });
  writeFileSync(join(project, "README"), "fixture\n");
  execFileSync("git", ["add", "README"], { cwd: project });
  execFileSync("git", ["commit", "--quiet", "-m", "fixture"], { cwd: project });
  mkdirSync(join(project, ".worktrees"), { recursive: true });
  execFileSync("git", ["worktree", "add", "--quiet", "--detach", linked, "HEAD"], { cwd: project });

  const { row, run } = attempt(project, "tasks", "linked-wait", "running", "running");
  const receipt = join(project, ".agent-run", "runs", run,
    "tasks", "linked-wait", "attempt-001", "attempt.json");
  const state = join(workspace, "state");
  const child = spawn("python3", [join(product, "scripts/provenant"), "lanes", "--wait"], {
    cwd: linked, env: laneWaitEnv(state), stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8").on("data", (chunk: string) => { stdout += chunk; });
  child.stderr.setEncoding("utf8").on("data", (chunk: string) => { stderr += chunk; });
  const exited = new Promise<number | null>((resolveExit, reject) => {
    child.once("error", reject);
    child.once("exit", (code) => resolveExit(code));
  });
  await new Promise((resolve) => setTimeout(resolve, 100));
  writeFileSync(receipt, JSON.stringify({ ...row, state: "input_required", status: "input_required" }));
  expect(await exited).toBe(0);
  expect(stderr).toBe("");
  expect(stdout).toContain("linked-wait");
}, 12_000);

it("reports every unseen completion past the 20-row display cap", () => {
  const workspace = fixture();
  const state = join(workspace, "state");
  for (let index = 0; index < 23; index += 1) attempt(workspace, "tasks", `bulk-${String(index).padStart(3, "0")}`, "terminal", "ok");
  const result = laneWait(workspace, state);
  expect(result.status, result.stderr).toBe(0);
  expect(result.stdout.trim().split("\n")).toHaveLength(23);
}, 20_000);

it("wakes only for the named lane, not for unrelated lanes that finish meanwhile", async () => {
  const workspace = fixture();
  const state = join(workspace, "state");
  attempt(workspace, "tasks", "unrelated-old", "terminal", "ok");
  const { row, run } = attempt(workspace, "tasks", "named-lane", "running", "running");
  const waiter = startWait(workspace, state, row.run_id);
  // Let the waiter take its first snapshot so the unrelated lane appears mid-wait.
  await pause(3000);
  attempt(workspace, "tasks", "smoke-848-1", "terminal", "ok");
  await pause(2600);
  expect(waiter.output.code, waiter.output.stdout + waiter.output.stderr).toBeUndefined();
  writeFileSync(join(workspace, ".agent-run", "runs", run, "tasks", "named-lane", "attempt-001", "attempt.json"),
    JSON.stringify({ ...row, state: "terminal", status: "ok" }));
  expect(await waiter.exited).toBe(0);
  expect(waiter.output.stdout).toContain("named-lane");
  expect(waiter.output.stdout).not.toContain("smoke-848-1");
  expect(waiter.output.stdout).not.toContain("unrelated-old");
}, 20_000);

it("a batch id wakes for its own children only", () => {
  const workspace = fixture();
  const state = join(workspace, "state");
  const batchRun = "20260924-1012-dispatch-batch-fixture";
  attempt(workspace, "tasks", "child-one", "terminal", "ok", batchRun);
  attempt(workspace, "tasks", "child-two", "running", "running", batchRun);
  writeFileSync(join(workspace, ".agent-run", "runs", batchRun, "dispatch-status.json"),
    JSON.stringify({ batch_id: "batch-fixture", started_at: new Date().toISOString() }));
  attempt(workspace, "tasks", "outsider", "terminal", "ok");
  const result = laneWait(workspace, state, "batch-fixture");
  expect(result.status, result.stderr).toBe(0);
  expect(result.stdout).toContain("child-one");
  expect(result.stdout).not.toContain("outsider");
}, 30_000);

it("reports input_required, then the resumed attempt's terminal state", () => {
  const workspace = fixture();
  const state = join(workspace, "state");
  const { row, run } = attempt(workspace, "tasks", "asks-first", "terminal", "input_required");
  const first = laneWait(workspace, state);
  expect(first.status, first.stderr).toBe(0);
  expect(first.stdout).toMatch(/^input_required\s+asks-first/u);
  const second = join(workspace, ".agent-run", "runs", run, "tasks", "asks-first", "attempt-002");
  mkdirSync(second, { recursive: true });
  writeFileSync(join(second, "attempt.json"), JSON.stringify({ ...row, attempt: 2, status: "ok",
    paths: { result: "tasks/asks-first/attempt-002/result.md" } }));
  const resumed = laneWait(workspace, state);
  expect(resumed.status, resumed.stderr).toBe(0);
  expect(resumed.stdout).toMatch(/^ok\s+asks-first/u);
  expect(laneWait(workspace, state).stdout).toBe("no lanes are running\n");
}, 30_000);

it("lists the project's lanes from a registered worktree and a subdirectory", () => {
  const workspace = fixture();
  const project = join(workspace, "project");
  const linked = join(project, ".worktrees", "linked");
  mkdirSync(join(project, "sub", "dir"), { recursive: true });
  execFileSync("git", ["init", "--quiet"], { cwd: project });
  execFileSync("git", ["-c", "user.email=fabric@example.invalid", "-c", "user.name=Fabric test",
    "commit", "--quiet", "--allow-empty", "-m", "fixture"], { cwd: project });
  execFileSync("git", ["worktree", "add", "--quiet", "--detach", linked, "HEAD"], { cwd: project });
  attempt(project, "tasks", "project-lane", "running", "running");
  const env = laneWaitEnv(join(workspace, "state"));
  for (const cwd of [linked, join(project, "sub", "dir")]) {
    const run = (...args: string[]) => {
      const result = spawnSync("python3", [join(product, "scripts/provenant"), ...args], { cwd, encoding: "utf8", env });
      expect(result.status, `${cwd} ${args.join(" ")}: ${result.stderr}`).toBe(0);
      return result.stdout;
    };
    expect(run("lanes", "--json")).toContain("project-lane");
    expect(run("fabric", "status", "--runs")).toContain("project-lane");
    expect(JSON.parse(run("fabric", "dispatch", "list", "--json")).workspace).toBe(realpathSync(project));
  }
}, 30_000);
