import { mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
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

function attempt(workspace: string, tree: string, taskId: string, state: string, status: string | null) {
  const run = `20260924-1012-dispatch-fixture-${taskId.slice(-6).padStart(6, "a")}`;
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

it("reads both receipt layouts through one versioned root-relative run interface", async () => {
  const workspace = fixture();
  attempt(workspace, "tasks", "modern", "terminal", "ok");
  attempt(workspace, "dispatch/tasks", "legacy", "terminal", "failed");
  const result = await readRuns(workspace);
  expect(result.schema).toBe("fabric.runs.v1");
  expect(result.status).toBe("ok");
  expect(result.runs).toHaveLength(2);
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
  expect(first.events.filter((event) => event.type === "task_state")).toHaveLength(2);
  expect(first.events.find((event) => event.type === "inbox_message")).toMatchObject({ id: "message-1", preview: "A long update" });
  expect((await readEvents(workspace, first.cursor, [message])).events).toEqual([]);
});

it("keeps the event cursor bounded across hundreds of retained attempts", async () => {
  const workspace = fixture();
  const { row, run } = attempt(workspace, "tasks", "many-attempts", "terminal", "failed");
  const taskDir = join(workspace, ".agent-run", "runs", run, "tasks", "many-attempts");
  for (let number = 2; number <= 500; number += 1) {
    const dir = join(taskDir, `attempt-${String(number).padStart(3, "0")}`);
    mkdirSync(dir);
    writeFileSync(join(dir, "attempt.json"), JSON.stringify({ ...row, attempt: number }));
  }
  const first = await readEvents(workspace);
  expect(first.events.filter((event) => event.type === "task_state")).toHaveLength(500);
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

it.skipIf(process.platform !== "darwin")("normalises a missing /var path against the /private/var run root", async () => {
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

it("keeps an event follower open by default", () => {
  const workspace = fixture();
  attempt(workspace, "tasks", "finished", "terminal", "ok");
  const product = resolve(import.meta.dirname, "../../..");
  const result = spawnSync("python3", [join(product, "scripts/provenant"), "events", "--follow"], {
    cwd: workspace, encoding: "utf8", timeout: 1500,
    env: { ...process.env, AGENT_FABRIC_PRODUCT_ROOT: product,
      AGENT_FABRIC_STATE_DIRECTORY: join(workspace, "state"),
      AGENT_FABRIC_TSX_LOADER: createRequire(import.meta.url).resolve("tsx") },
  });
  expect((result.error as NodeJS.ErrnoException | undefined)?.code).toBe("ETIMEDOUT");
  expect(result.stdout).toContain('"type":"task_state"');
});

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
