import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
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

it("does not publish a result path outside its run", async () => {
  const workspace = fixture();
  const { row, run } = attempt(workspace, "tasks", "escaped", "terminal", "ok");
  row.paths.result = "../../outside.md";
  writeFileSync(join(workspace, ".agent-run", "runs", run, "tasks", "escaped", "attempt-001", "attempt.json"), JSON.stringify(row));
  expect((await readRuns(workspace)).runs[0]?.result_path).toBeNull();
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
