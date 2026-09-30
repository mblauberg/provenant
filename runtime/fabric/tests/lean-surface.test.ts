import { Client } from "@modelcontextprotocol/sdk/client/index.js";
import { StdioClientTransport } from "@modelcontextprotocol/sdk/client/stdio.js";
import { appendFileSync, existsSync, chmodSync, copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { resolve, join } from "node:path";
import { createRequire } from "node:module";
import { execFileSync } from "node:child_process";
import { afterAll, describe, expect, it } from "vitest";

import { cancelConfiguredRun } from "../src/execution.js";
import { fullView, lanesDigest, cancelDigest } from "../src/surface.js";
import { readRuns } from "../src/run-reader.js";

const roots: string[] = [];
afterAll(() => { for (const root of roots) rmSync(root, { recursive: true, force: true }); });

function installFixtureOwners(product: string): void {
  const owners = join(product, "skills/orchestrate/scripts");
  mkdirSync(owners, { recursive: true });
  mkdirSync(join(product, "config"), { recursive: true });
  const routing = JSON.parse(readFileSync(resolve(import.meta.dirname, "../../../config/model-routing.json"), "utf8"));
  routing.adapters.codex.models.push({ id: "fixture", names: ["fixture"] });
  writeFileSync(join(product, "config/model-routing.json"), JSON.stringify(routing));
  copyFileSync(resolve(import.meta.dirname, "../../../config/adapter-compatibility.yaml"), join(product, "config/adapter-compatibility.yaml"));
  mkdirSync(join(product, "scripts/lib"), { recursive: true });
  copyFileSync(resolve(import.meta.dirname, "../../../scripts/lib/harness-python.sh"), join(product, "scripts/lib/harness-python.sh"));
  for (const name of ["run_dir_init.sh", "dispatch_run.py", "batch_run.py", "run_controls.py"]) {
    const path = join(owners, name), fixture = join(import.meta.dirname, "v2-owner-fixture.mjs");
    writeFileSync(path, name.endsWith(".sh")
      ? `#!/bin/sh\nPROVENANT_FIXTURE_OWNER=${name} exec '${process.execPath}' '${fixture}' "$@"\n`
      : `#!/usr/bin/env python3\nimport os,sys\nos.environ['PROVENANT_FIXTURE_OWNER']=${JSON.stringify(name)}\nos.execv(${JSON.stringify(process.execPath)},[${JSON.stringify(process.execPath)},${JSON.stringify(fixture)},*sys.argv[1:]])\n`);
    chmodSync(path, 0o755);
  }
}

async function connect() {
  const root = mkdtempSync(join(tmpdir(), "fabric-lean-"));
  roots.push(root);
  const workspace = join(root, "workspace"), product = join(root, "product"), state = join(root, "state");
  mkdirSync(workspace);
  installFixtureOwners(product);
  const client = new Client({ name: "lean", version: "1" });
  await client.connect(new StdioClientTransport({
    command: resolve(import.meta.dirname, "../bin/fabric-mcp"),
    cwd: workspace,
    env: {
      ...(process.env as Record<string, string>),
      FABRIC_NODE: process.execPath,
      AGENT_FABRIC_TSX_LOADER: createRequire(import.meta.url).resolve("tsx"),
      AGENT_FABRIC_STATE_DIRECTORY: state,
      AGENT_FABRIC_LABEL: "lean-seat",
      AGENT_FABRIC_PRODUCT_ROOT: product,
      HARNESS_PYTHON: execFileSync("python3", ["-c", "import sys;print(sys.executable)"], { encoding: "utf8" }).trim(),
    },
    stderr: "pipe",
  }));
  const call = async (name: string, args: Record<string, unknown>) => {
    const result = await client.callTool({ name, arguments: args });
    return { result, text: (result.content as any[])[0].text as string, size: JSON.stringify(result).length };
  };
  return { client, call, workspace };
}
const idOf = (text: string) => text.match(/\bmcp-[A-Za-z0-9_-]+/u)?.[0]!;

describe("lean surface over the fixture owners", () => {
  it("measures the reply sizes the lean surface targets", async () => {
    const { client, call } = await connect();
    try {
      const ids: string[] = [];
      for (let index = 0; index < 8; index++)
        ids.push(idOf((await call("fabric_dispatch", { adapter: "codex", model: "fixture", prompt: "quick", wait_seconds: 20 })).text));
      const status = await call("fabric_status", { id: ids[0], wait_seconds: 0 });
      const full = await call("fabric_status", { ids: ids.slice(0, 3), detail: "full" });
      const runs = await call("fabric_runs", {});
      const slow = idOf((await call("fabric_dispatch", { adapter: "codex", model: "fixture", prompt: "slow", wait_seconds: 3 })).text);
      const cancel = await call("fabric_cancel", { id: slow });
      const line = `${JSON.stringify(cancel.text.slice(0, 160))} status_terminal=${status.size} runs_8=${runs.size} cancel=${cancel.size} status_full_3=${full.size}\n`;
      if (process.env.LEAN_MEASURE_OUT) appendFileSync(process.env.LEAN_MEASURE_OUT, line);
      expect(status.text).toContain(ids[0]);
    } finally {
      await client.close();
    }
  }, 120_000);

  it("accepts run_id, numeric strings and a numeric until, each with a warning", async () => {
    const { client, call } = await connect();
    try {
      const id = idOf((await call("fabric_dispatch", { adapter: "codex", model: "fixture", prompt: "quick", wait_seconds: "20", id: "named" })).text);
      const byRun = await call("fabric_status", { run_id: id, until: "55", wait_seconds: "0" });
      expect(byRun.text).toContain(id);
      expect(byRun.text).toContain("corrected run_id to id");
      expect(byRun.text).toContain('read wait_seconds "0" as a number');
      const numericUntil = await call("fabric_status", { id, until: "5" });
      expect(numericUntil.text).toContain("read 5 as wait_seconds");
      expect((await call("fabric_status", { task: "named", until: "ANY" })).text).toContain("named");
      expect((await call("fabric_runs", { limit: "1" })).text).toContain("named");
      expect((await call("fabric_output", { run_id: id, max_bytes: "20" })).text).toBeDefined();
      const missing = await call("fabric_cancel", {});
      expect(missing.text).toContain("id:");
    } finally {
      await client.close();
    }
  }, 120_000);

  it("propagates nested task alias warnings with the task index", async () => {
    const { client, call } = await connect();
    try {
      const reply = await call("fabric_dispatch", { tasks: [
        { id: "plain", prompt: "quick", adapter: "codex", model: "fixture" },
        { task_id: "aliased", prompt: "quick", adapter: "codex", model: "fixture", timeout_seconds: "60" },
      ], wait_seconds: 20 });
      expect(reply.text).toContain("tasks[1]: corrected task_id to id");
      expect(reply.text).toContain('tasks[1]: read timeout_seconds "60" as a number');
      expect(reply.text).not.toContain("tasks[0]:");
    } finally {
      await client.close();
    }
  }, 120_000);

  it("filters runs by state and limit and keeps the default reply small", async () => {
    const { client, call } = await connect();
    try {
      const done = [] as string[];
      for (let index = 0; index < 3; index++)
        done.push(idOf((await call("fabric_dispatch", { adapter: "codex", model: "fixture", prompt: "quick", wait_seconds: 20 })).text));
      await call("fabric_dispatch", { adapter: "codex", model: "fixture", prompt: "slow", wait_seconds: 1, task_id: "slowjob" });
      const slow = "slowjob";
      const running = await call("fabric_runs", { state: "running" });
      expect(running.result.structuredContent).toBeUndefined();
      expect(running.text.split("\n")).toHaveLength(1);
      expect(running.text).toContain(slow);
      const terminal = await call("fabric_runs", { state: "terminal", limit: 2 });
      expect(terminal.text.split("\n")).toHaveLength(3);
      expect(terminal.text).toContain("1 more omitted");
      expect((await call("fabric_runs", { state: "active" })).text).toContain(slow);
      const full = await call("fabric_runs", { state: "running", detail: "full" });
      expect((full.result.structuredContent as any).runs).toHaveLength(1);
      await call("fabric_cancel", { id: slow });
    } finally {
      await client.close();
    }
  }, 120_000);

  it("gives terminal status rows a bounded result tail and a running hint", async () => {
    const { client, call } = await connect();
    try {
      const id = idOf((await call("fabric_dispatch", { adapter: "codex", model: "fixture", prompt: "quick", wait_seconds: 20 })).text);
      const tailed = await call("fabric_status", { id });
      expect(tailed.text).toContain("result tail");
      expect(tailed.text).toContain("x".repeat(1200));
      expect(tailed.text).not.toContain("x".repeat(1201));
      expect((await call("fabric_status", { id, tail_chars: 0 })).text).not.toContain("result tail");
      expect((await call("fabric_status", { id, tail_chars: 50 })).text).toContain("x".repeat(50));
      const slow = idOf((await call("fabric_dispatch", { adapter: "codex", model: "fixture", prompt: "slow", wait_seconds: 1 })).text);
      const live = await call("fabric_status", { id: slow });
      expect(live.text).toContain("provenant lanes --wait --all --timeout");
      const cancelled = await call("fabric_cancel", { task_id: slow });
      expect(cancelled.result.structuredContent).toBeUndefined();
      expect(cancelled.text).toContain("cancelled");
      expect(cancelled.size).toBeLessThan(700);
      expect((await call("fabric_cancel", { id: slow, detail: "full" })).result.structuredContent).toBeDefined();
    } finally {
      await client.close();
    }
  }, 120_000);
});

describe("lean views", () => {
  const attempt = (n: number) => ({ attempt: n, state: "terminal", status: "ok", started_at: "t", ended_at: "u",
    paths: { result: `a${n}/result.md` }, provenance: { big: "x".repeat(500) }, evidence: { big: "y".repeat(500) } });
  it("summarises the attempt history in full detail and selects fields on request", () => {
    const row = { id: "one", run_id: "mcp-1", task_id: "one", state: "terminal", status: "ok", digest: "ok one", provenance: { a: 1 }, attempts: [attempt(1), attempt(2)] };
    const full = fullView({ runs: [row] }).runs[0];
    expect(full.attempts).toEqual([
      { attempt: 1, state: "terminal", status: "ok", started_at: "t", ended_at: "u", result: "a1/result.md" },
      { attempt: 2, state: "terminal", status: "ok", started_at: "t", ended_at: "u", result: "a2/result.md" },
    ]);
    expect(full.provenance).toEqual({ a: 1 });
    expect(Object.keys(fullView(row, ["provenance"])).sort()).toEqual(["digest", "id", "provenance", "run_id", "state", "status", "task_id"]);
    expect(fullView(row, ["attempts"]).attempts).toHaveLength(2);
    expect(JSON.stringify(fullView(row)).length).toBeLessThan(JSON.stringify(row).length / 2);
  });
  it("renders lane lines and a compact cancel summary", () => {
    expect(lanesDigest({ runs: [{ id: "a", state: "terminal", status: "ok", route: "codex/x@high", result_path: "r.md" }], omitted: 3 }))
      .toBe("ok  a  codex/x@high  r.md\n3 more omitted; raise limit or pass ids");
    expect(lanesDigest({ runs: [] })).toBe("no runs");
    expect(lanesDigest({ status: "unknown", error: "run_read_failed", runs: [] })).toBe("unknown run_read_failed");
    const rows = Array.from({ length: 6 }, (_, n) => ({ run_id: "mcp-b", task_id: `t${n}`, status: n < 2 ? "cancelled" : "ok", digest: `row ${n}` }));
    expect(cancelDigest({ runs: rows })).toBe("mcp-b 6 tasks: 2 cancelled 4 ok");
    expect(cancelDigest({ runs: rows.slice(0, 2) })).toContain("row 0");
  });
  it("filters readRuns by state", async () => {
    const empty = await readRuns(mkdtempSync(join(tmpdir(), "fabric-lean-empty-")), undefined, 0, undefined, 5, "running");
    expect(empty).toMatchObject({ status: "ok", runs: [] });
  });
});

describe("task-level cancel", () => {
  it("cancels one batch task through run_controls without stopping its siblings", async () => {
    const root = mkdtempSync(join(tmpdir(), "fabric-lean-cancel-"));
    roots.push(root);
    const workspace = join(root, "workspace"), product = join(root, "product");
    mkdirSync(workspace);
    installFixtureOwners(product);
    const log = join(root, "controls.jsonl");
    writeFileSync(join(product, "skills/orchestrate/scripts/run_controls.py"),
      `#!/usr/bin/env python3\nimport json,sys\nopen(${JSON.stringify(log)},'a').write(json.dumps(sys.argv[1:])+'\\n')\n`);
    const runDir = join(workspace, ".agent-run", "mcp-batchx");
    for (const task of ["one", "two"]) {
      const dir = join(runDir, "tasks", task, "attempt-001");
      mkdirSync(dir, { recursive: true });
      writeFileSync(join(dir, "attempt.json"), JSON.stringify({ schema: "fabric.attempt.v1", run_id: "mcp-batchx", task_id: task,
        attempt: 1, state: "running", status: null, started_at: new Date().toISOString(), paths: {}, digest: `running ${task}` }));
    }
    writeFileSync(join(runDir, "dispatch-status.json"), JSON.stringify({ id: "mcp-batchx", batch_id: "batch-001", task_ids: ["one", "two"],
      started_at: new Date().toISOString(), status: "running" }));
    const env = { ...process.env, AGENT_FABRIC_PRODUCT_ROOT: product,
      HARNESS_PYTHON: execFileSync("python3", ["-c", "import sys;print(sys.executable)"], { encoding: "utf8" }).trim() };
    const identity = { project: workspace, cwd: workspace, agentId: "lean", provider: "codex" };
    const result = await cancelConfiguredRun("two", identity, "not needed", env) as any;
    if (result.status === "rejected") throw new Error(JSON.stringify(result));
    const calls = readFileSync(log, "utf8").trim().split("\n").map((line) => JSON.parse(line));
    expect(calls).toHaveLength(1);
    expect(calls[0]).toEqual(expect.arrayContaining(["cancel", "--task-id", "two", "--attempt-id", "attempt-001"]));
    expect(calls[0]).not.toContain("--batch-id");
    expect(result.runs.map((row: any) => row.task_id)).toEqual(["two"]);
    expect(result.reason).toBe("not needed");
  }, 60_000);

  it("rejects a task cancel whose batch scope cannot be established", async () => {
    const root = mkdtempSync(join(tmpdir(), "fabric-lean-scope-"));
    roots.push(root);
    const workspace = join(root, "workspace"), product = join(root, "product");
    mkdirSync(workspace);
    installFixtureOwners(product);
    const log = join(root, "controls.jsonl");
    writeFileSync(join(product, "skills/orchestrate/scripts/run_controls.py"),
      `#!/usr/bin/env python3\nimport json,sys\nopen(${JSON.stringify(log)},'a').write(json.dumps(sys.argv[1:])+'\\n')\n`);
    const runDir = join(workspace, ".agent-run", "mcp-noscope");
    for (const task of ["one", "two"]) {
      const dir = join(runDir, "tasks", task, "attempt-001");
      mkdirSync(dir, { recursive: true });
      writeFileSync(join(dir, "attempt.json"), JSON.stringify({ schema: "fabric.attempt.v1", run_id: "mcp-noscope", task_id: task,
        attempt: 1, state: "running", status: null, started_at: new Date().toISOString(), paths: {}, digest: `running ${task}` }));
    }
    const env = { ...process.env, AGENT_FABRIC_PRODUCT_ROOT: product,
      HARNESS_PYTHON: execFileSync("python3", ["-c", "import sys;print(sys.executable)"], { encoding: "utf8" }).trim() };
    const identity = { project: workspace, cwd: workspace, agentId: "lean", provider: "codex" };
    const result = await cancelConfiguredRun("two", identity, undefined, env) as any;
    expect(result).toMatchObject({ status: "rejected", error: "task_scope_unknown" });
    expect(result.fix).toContain("mcp-noscope");
    expect(existsSync(log)).toBe(false);
  }, 60_000);

  it("never widens a task cancel to the run when the task has not started", async () => {
    const root = mkdtempSync(join(tmpdir(), "fabric-lean-queued-"));
    roots.push(root);
    const workspace = join(root, "workspace"), product = join(root, "product");
    mkdirSync(workspace);
    installFixtureOwners(product);
    const log = join(root, "controls.jsonl");
    writeFileSync(join(product, "skills/orchestrate/scripts/run_controls.py"),
      `#!/usr/bin/env python3\nimport json,sys\nopen(${JSON.stringify(log)},'a').write(json.dumps(sys.argv[1:])+'\\n')\n`);
    const runDir = join(workspace, ".agent-run", "mcp-queuedx");
    mkdirSync(runDir, { recursive: true });
    writeFileSync(join(runDir, "dispatch-status.json"), JSON.stringify({ id: "mcp-queuedx", batch_id: "batch-001", task_ids: ["one", "two"],
      started_at: new Date().toISOString(), status: "running" }));
    const env = { ...process.env, AGENT_FABRIC_PRODUCT_ROOT: product,
      HARNESS_PYTHON: execFileSync("python3", ["-c", "import sys;print(sys.executable)"], { encoding: "utf8" }).trim() };
    const identity = { project: workspace, cwd: workspace, agentId: "lean", provider: "codex" };
    const result = await cancelConfiguredRun("two", identity, undefined, env) as any;
    expect(result).toMatchObject({ status: "rejected", error: "task_not_started" });
    expect(result.fix).toContain("mcp-queuedx");
    expect(existsSync(log)).toBe(false);
    const unknown = await cancelConfiguredRun("three", identity, undefined, env) as any;
    expect(unknown.status).toBe("rejected");
  }, 60_000);
});

