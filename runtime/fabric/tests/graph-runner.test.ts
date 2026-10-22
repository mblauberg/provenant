import Database from "better-sqlite3";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { fileURLToPath } from "node:url";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { Store } from "../src/store.js";
import { catalogueSnapshot } from "../src/catalogue.js";
import { validateGraph, type GraphDocument, type GraphNode } from "../src/graph/document.js";
import { databasePath } from "../src/identity.js";
import { GraphStore, RESERVATION_FILE, type Attempt, type Graph } from "../src/graph/store.js";
import { listRecordedRuns, processStartedAt } from "../src/run-registry.js";
import { scratchRoot } from "./helpers/temp-root.js";

const testDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(testDirectory, "../../..");
const packageRoot = resolve(testDirectory, "..");
const fixture = join(testDirectory, "lifecycle-owner-fixture.mjs");
const tsxLoader = createRequire(import.meta.url).resolve("tsx");
const fixturePython = execFileSync("/usr/bin/env", ["python3", "-c", "import sys; print(sys.executable)"], { encoding: "utf8" }).trim();

let root: string, workspace: string, env: NodeJS.ProcessEnv;
const tracked = new Set<number>();

const shellQuote = (value: string) => `'${value.replaceAll("'", `'"'"'`)}'`;
const git = (cwd: string, ...args: string[]) => execFileSync("git", ["-C", cwd, ...args], { stdio: "ignore" });

/** A product root whose dispatch owners are the lifecycle fixture, with a fast graph tick. */
function buildProduct(caps: Record<string, number> = {}): string {
  const product = join(root, "product");
  const owners = join(product, "skills/orchestrate/scripts");
  mkdirSync(join(product, "config"), { recursive: true });
  mkdirSync(owners, { recursive: true });
  mkdirSync(join(product, "scripts/lib"), { recursive: true });
  for (const name of ["model-routing.json", "adapter-compatibility.yaml"])
    copyFileSync(join(repositoryRoot, "config", name), join(product, "config", name));
  copyFileSync(join(repositoryRoot, "scripts/lib/harness-python.sh"), join(product, "scripts/lib/harness-python.sh"));
  writeFileSync(join(product, "config/graph-defaults.json"), JSON.stringify({
    schema: "provenant.graph-defaults.v1", pool_caps: { opencode: 2, cursor: 1, agy: 2, default: 1, ...caps },
    tick_seconds: 0.1, probe_seconds: 0.3, gate_grace_seconds: 1,
  }));
  for (const name of ["run_dir_init.sh", "dispatch_run.py", "batch_run.py", "run_controls.py"]) {
    const owner = join(owners, name);
    writeFileSync(owner, name === "run_dir_init.sh"
      ? `#!/bin/sh\nPROVENANT_FIXTURE_OWNER=${shellQuote(name)} exec ${shellQuote(process.execPath)} ${shellQuote(fixture)} "$@"\n`
      : `#!/usr/bin/env python3\nimport os, sys\nos.environ["PROVENANT_FIXTURE_OWNER"] = ${JSON.stringify(name)}\nos.execv(${JSON.stringify(process.execPath)}, [${JSON.stringify(process.execPath)}, ${JSON.stringify(fixture)}, *sys.argv[1:]])\n`);
    chmodSync(owner, 0o755);
  }
  return product;
}

function fabric(args: string[], cwd = workspace): { status: number | null; stdout: string; stderr: string } {
  const result = spawnSync(join(packageRoot, "bin/fabric"), args, { cwd, encoding: "utf8", env, timeout: 30_000 });
  return { status: result.status, stdout: result.stdout, stderr: result.stderr };
}

function writeGraph(name: string, document: Record<string, unknown>): string {
  const path = join(root, `${name}.json`);
  writeFileSync(path, JSON.stringify({ schema_version: 1, ...document }));
  return path;
}

function run(document: Record<string, unknown>, cwd = workspace): Record<string, any> {
  const result = fabric(["graph", "run", "--file", writeGraph(`graph-${Date.now()}`, document), "--json"], cwd);
  expect(result.status, result.stdout + result.stderr).toBe(0);
  const view = JSON.parse(result.stdout) as Record<string, any>;
  trackOwner(view.graph_id, cwd);
  return view;
}

const withStore = <T>(work: (store: GraphStore) => T): T => {
  const store = new GraphStore(env);
  try { return work(store); } finally { store.close(); }
};
const graphRow = (id: string, project = workspace): Graph => withStore((store) => store.graph(project, id)!);
const nodes = (id: string, project = workspace): Map<string, Attempt> => withStore((store) => store.current(project, id));
const allAttempts = (id: string, project = workspace): Attempt[] => withStore((store) => store.attempts(project, id));

function trackOwner(id: string, project: string): void {
  void (async () => {
    for (let index = 0; index < 100; index++) {
      const owner = withStore((store) => store.graph(project, id)?.owner_pid);
      if (owner) { tracked.add(owner); return; }
      await delay(50);
    }
  })();
}

const alive = (pid: number) => { try { process.kill(pid, 0); return true; } catch { return false; } };

async function waitForFile(path: string): Promise<string> {
  await waitFor(() => existsSync(path) && readFileSync(path, "utf8").trim() !== "", `file ${path}`);
  return path;
}

async function waitFor(condition: () => boolean, label: string, timeoutMs = 20_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!condition() && Date.now() < deadline) await delay(50);
  expect(condition(), label).toBe(true);
}

const reader = (id: string, route: GraphNode["route"], extra: Record<string, unknown> = {}) =>
  ({ id, route, brief: { prompt: `Survey ${id}.` }, ...extra });

beforeEach(() => {
  root = realpathSync(scratchRoot("fabric-graph-"));
  workspace = join(root, "workspace");
  mkdirSync(workspace);
  git(workspace, "init", "-q", "-b", "main");
  git(workspace, "-c", "user.email=t@example.invalid", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "root");
  env = {
    ...process.env,
    AGENT_FABRIC_PRODUCT_ROOT: buildProduct(),
    AGENT_FABRIC_SEAT: "codex",
    AGENT_FABRIC_STATE_DIRECTORY: join(root, "state"),
    HARNESS_PYTHON: fixturePython,
    FABRIC_NODE: process.execPath,
    AGENT_FABRIC_TSX_LOADER: tsxLoader,
  };
});

afterEach(async () => {
  for (const pid of tracked) { try { process.kill(pid, "SIGKILL"); } catch { /* gone */ } }
  for (const run of listRecordedRuns(workspace)) {
    for (const pid of [run.owner_pid]) if (pid) { try { process.kill(-pid, "SIGKILL"); } catch { /* gone */ } try { process.kill(pid, "SIGKILL"); } catch { /* gone */ } }
  }
  const pids = [...tracked];
  tracked.clear();
  await waitFor(() => pids.every((pid) => !alive(pid)), "graph processes to exit before cleanup");
  if (process.env.GRAPH_TEST_KEEP) { console.log(`kept ${root}`); return; }
  rmSync(root, { recursive: true, force: true });
});

describe("graph runner", { timeout: 60_000 }, () => {
  it("runs a three-tier mixed-provider graph to done with no coordinator, and every receipt joins its dispatch", async () => {
    const started = Date.now();
    const view = run({ nodes: [
      reader("survey", { adapter: "opencode", alias: "glm-free" }, { tier: "junior" }),
      reader("design", { adapter: "cursor", model: "composer-2.5" }, { tier: "senior" }),
      reader("synth", { adapter: "agy", model: "gemini-3.8-flash" }, { tier: "principal", depends_on: ["survey", "design"],
        inputs: [{ from: "survey", part: "result" }, { from: "design", part: "result" }] }),
    ] });
    expect(Date.now() - started).toBeLessThan(15_000);
    expect(view).toMatchObject({ status: "ok", graph_state: "running" });
    await waitFor(() => graphRow(view.graph_id).state !== "running", "graph to finish on its own");
    expect(graphRow(view.graph_id).state).toBe("done");
    const rows = allAttempts(view.graph_id);
    expect(rows.map((row) => [row.node_id, row.state, row.adapter])).toEqual(expect.arrayContaining([
      ["survey", "passed", "opencode"], ["design", "passed", "cursor"], ["synth", "passed", "agy"]]));
    for (const row of rows) {
      const receipt = JSON.parse(readFileSync(join(workspace, ".agent-run/graphs", view.graph_id, "r1", row.node_id, "a1", "receipt.json"), "utf8"));
      expect(receipt.schema).toBe("provenant.graph-node-receipt.v1");
      expect(receipt.identity).toMatchObject({ graph_id: view.graph_id, node_id: row.node_id, attempt: 1, revision: 1 });
      // The join: the receipt names a dispatch whose own status record and attempt receipt agree with it.
      const dispatch = JSON.parse(readFileSync(join(receipt.dispatch.run_dir, "dispatch-status.json"), "utf8"));
      expect(dispatch).toMatchObject({ id: row.run_id, task_id: row.task_id, status: "ok" });
      expect(dispatch.routes[0].adapter).toBe(row.adapter);
      const attempt = JSON.parse(readFileSync(join(receipt.dispatch.run_dir, "dispatch/tasks", row.task_id, "attempt-001/attempt.json"), "utf8"));
      expect(attempt).toMatchObject({ task_id: row.task_id, status: "ok" });
      expect(receipt.outcome.result_path).toBe(join(receipt.dispatch.run_dir, attempt.result.path));
      expect(existsSync(join(receipt.dispatch.run_dir, "KEEP")), "KEEP pins are removed once the graph ends").toBe(false);
    }
    const synth = rows.find((row) => row.node_id === "synth")!;
    expect(synth.input_attempts).toEqual({ survey: 1, design: 1 });
    const brief = readFileSync(join(workspace, ".agent-run/graphs", view.graph_id, "r1/synth/a1/brief.md"), "utf8");
    for (const parent of ["survey", "design"])
      expect(brief).toContain(`- ${parent}: ${String(rows.find((row) => row.node_id === parent)!.outcome!.result_path)}`);
  });

  it("rejects a confidential parent feeding a non-confidential synthesis node before anything runs", () => {
    const result = fabric(["graph", "run", "--file", writeGraph("leak", { nodes: [
      reader("secret", { adapter: "opencode", alias: "glm-free" }, { authority: { confidential: true } }),
      reader("synth", { adapter: "opencode", alias: "glm-free" }, { depends_on: ["secret"], inputs: [{ from: "secret", part: "result" }],
        authority: { confidential: false } }),
    ] })]);
    expect(result.status).toBe(1);
    expect(result.stdout).toContain("node synth: confidentiality_downgrade");
    expect(withStore((store) => store.graphs(workspace))).toEqual([]);
    expect(listRecordedRuns(workspace)).toEqual([]);
  });

  it("survives its owner being killed mid-run: the lane keeps running and resume adopts it", async () => {
    const release = join(root, "release-a");
    const view = run({ nodes: [
      reader("a", { adapter: "opencode", alias: "glm-free" }, { brief: { prompt: `hold ${release}` } }),
      reader("b", { adapter: "opencode", model: "deepseek-free" }, { depends_on: ["a"] }),
    ] });
    const id = view.graph_id as string;
    await waitFor(() => nodes(id).get("a")?.run_id != null, "node a to launch");
    const before = nodes(id).get("a")!;
    const lanePid = Number(readFileSync(await waitForFile(join(before.run_dir!, "sleeping.pid")), "utf8"));
    tracked.add(lanePid);
    expect(existsSync(join(before.run_dir!, "KEEP")), "a launched lane is pinned against pruning").toBe(true);
    const owner = graphRow(id).owner_pid!;
    process.kill(owner, "SIGKILL");
    await waitFor(() => !alive(owner), "owner to die");
    expect(alive(lanePid), "the lane outlives its graph owner").toBe(true);
    const status = fabric(["graph", "status", id]);
    expect(status.stdout).toContain(`owner died: provenant graph resume ${id}`);
    const hint = fabric(["lanes", "--wait", "--all", "--timeout", "0"]);
    expect(hint.stderr).toContain(`graph ${id} owner died: provenant graph resume ${id}`);
    expect(fabric(["graph", "resume", id]).status).toBe(0);
    await waitFor(() => { const pid = graphRow(id).owner_pid; return pid !== null && pid !== owner && alive(pid); }, "a new owner to claim the graph");
    tracked.add(graphRow(id).owner_pid!);
    writeFileSync(release, "");
    await waitFor(() => graphRow(id).state !== "running", "graph to finish after resume");
    expect(graphRow(id).state).toBe("done");
    const a = allAttempts(id).filter((row) => row.node_id === "a");
    expect(a.map((row) => [row.attempt, row.run_id, row.state])).toEqual([[1, before.run_id, "passed"]]);
  });

  it("resumes the frozen route even when catalogue alias order changes", async () => {
    const document = { schema_version: 1, graph_id: "g-012abc", revision: 1,
      nodes: [reader("a", { adapter: "opencode", alias: "junior" })] } as GraphDocument;
    const chair = { project: workspace, cwd: workspace, agentId: "codex-test", provider: "codex" };
    const catalogue = catalogueSnapshot(repositoryRoot);
    const model = "opencode/glm-5.3-flash-free";
    const previous = { ...catalogue, adapters: catalogue.adapters.map((adapter) => adapter.name !== "opencode" ? adapter
      : { ...adapter, aliases: { ...adapter.aliases, junior: [model] } }) };
    const plan = validateGraph(document, chair, previous, env).plan!;
    expect(validateGraph(document, chair, catalogue, env).plan!.nodes[0]!.pool.model).not.toBe(model);
    withStore((store) => store.create(chair, document, plan));
    expect(fabric(["graph", "resume", document.graph_id]).status).toBe(0);
    trackOwner(document.graph_id, workspace);
    await waitFor(() => graphRow(document.graph_id).state !== "running", "frozen graph to finish");
    const row = nodes(document.graph_id).get("a")!;
    expect(row.model).toBe(model);
    const attempt = JSON.parse(readFileSync(join(row.run_dir!, "dispatch/tasks", row.task_id, "attempt-001/attempt.json"), "utf8"));
    expect(attempt.route.resolved_model).toBe(model);
    expect(row.state).toBe("passed");
  });

  it.each(["document", "plan"])("refuses resume when the stored %s spec_hash changes", (part) => {
    const document = { schema_version: 1, graph_id: "g-012abc", revision: 1,
      nodes: [reader("a", { adapter: "opencode", alias: "glm-free" })] } as GraphDocument;
    const chair = { project: workspace, cwd: workspace, agentId: "codex-test", provider: "codex" };
    const plan = validateGraph(document, chair, catalogueSnapshot(repositoryRoot), env).plan!;
    withStore((store) => store.create(chair, document, plan));
    document.nodes[0]!.brief = { prompt: "Changed after validation" };
    const db = new Database(databasePath(env));
    try {
      if (part === "document") db.prepare("UPDATE graph_revisions SET document_json = ?").run(JSON.stringify(document));
      else {
        plan.nodes[0]!.brief.sha256 = "changed";
        db.prepare("UPDATE graph_revisions SET plan_json = ?").run(JSON.stringify(plan));
      }
    } finally { db.close(); }
    const result = fabric(["graph-owner", document.graph_id, workspace]);
    expect(result.status).toBe(1);
    expect(nodes(document.graph_id).get("a")!.run_id).toBeNull();
    const audit = new Database(databasePath(env));
    try { expect(JSON.stringify(audit.prepare("SELECT detail FROM activity").all())).toContain("spec_hash changed; refusing resume"); } finally { audit.close(); }
  });

  it.each(["changed", "unreadable"])("fails a %s prompt at launch once without dispatching or retrying", async (condition) => {
    const brief = join(workspace, "brief.md");
    writeFileSync(brief, "Validated content");
    const document = { schema_version: 1, graph_id: "g-012abc", revision: 1,
      nodes: [reader("a", { adapter: "opencode", alias: "glm-free" }, { brief: { prompt_file: brief } }),
        reader("child", { adapter: "opencode", alias: "glm-free" }, { depends_on: ["a"] })] } as GraphDocument;
    const chair = { project: workspace, cwd: workspace, agentId: "codex-test", provider: "codex" };
    const plan = validateGraph(document, chair, catalogueSnapshot(repositoryRoot), env).plan!;
    withStore((store) => store.create(chair, document, plan));
    if (condition === "changed") writeFileSync(brief, "Unvalidated content");
    else rmSync(brief);
    expect(fabric(["graph", "resume", document.graph_id]).status).toBe(0);
    trackOwner(document.graph_id, workspace);
    await waitFor(() => graphRow(document.graph_id).state !== "running", "prompt error to finish the graph", 5_000);
    const row = nodes(document.graph_id).get("a")!;
    expect(row.state).toBe("failed");
    expect(row.outcome?.reason).toContain(condition === "changed" ? "brief sha256 mismatch" : "brief unavailable");
    expect(row.run_id).toBeNull();
    expect(row.holds_slot).toBe(false);
    expect(nodes(document.graph_id).get("child")!.state).toBe("blocked");
    expect(allAttempts(document.graph_id).filter((attempt) => attempt.node_id === "a")).toHaveLength(1);
    const audit = new Database(databasePath(env));
    try { expect(audit.prepare("SELECT detail FROM activity WHERE detail LIKE '%a a1: running%'").all()).toHaveLength(1); }
    finally { audit.close(); }
  });

  it("quietly exits when a live graph owner holds the lock", () => {
    const document = { schema_version: 1, graph_id: "g-012abc", revision: 1,
      nodes: [reader("a", { adapter: "opencode", alias: "glm-free" })] } as GraphDocument;
    const chair = { project: workspace, cwd: workspace, agentId: "codex-test", provider: "codex" };
    const plan = validateGraph(document, chair, catalogueSnapshot(repositoryRoot), env).plan!;
    withStore((store) => store.create(chair, document, plan));
    withStore((store) => expect(store.claimOwner(workspace, document.graph_id, process.pid, processStartedAt(process.pid), () => false)).toBe(true));
    mkdirSync(join(workspace, ".agent-run/graphs", document.graph_id, "owner.lock"), { recursive: true });
    expect(fabric(["graph-owner", document.graph_id, workspace]).status).toBe(0);
    expect(graphRow(document.graph_id).state).toBe("running");
    const audit = new Database(databasePath(env));
    try { expect(JSON.stringify(audit.prepare("SELECT detail FROM activity").all())).not.toContain("owner lock unavailable"); }
    finally { audit.close(); }
  });

  it("bounds startup fencing and leaves an unverifiable graph nonterminal with a diagnostic", () => {
    const document = { schema_version: 1, graph_id: "g-012abc", revision: 1,
      nodes: [reader("a", { adapter: "opencode", alias: "glm-free" })] } as GraphDocument;
    const chair = { project: workspace, cwd: workspace, agentId: "codex-test", provider: "codex" };
    const plan = validateGraph(document, chair, catalogueSnapshot(repositoryRoot), env).plan!;
    const unknown = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { detached: true, stdio: "ignore" });
    unknown.unref();
    tracked.add(unknown.pid!);
    withStore((store) => {
      store.create(chair, document, plan);
      const row = store.current(workspace, document.graph_id).get("a")!;
      store.reserve(row, { maxWidth: 1, cap: 1, handOccupancy: 0 }, chair.agentId);
      store.setGates(row, [{ index: 0, token: "unknown", argv_hash: "unknown", pid: unknown.pid!, pgid: unknown.pid!, started_at: null }]);
    });
    document.nodes[0]!.brief = { prompt: "Changed" };
    const db = new Database(databasePath(env));
    try { db.prepare("UPDATE graph_revisions SET document_json = ?").run(JSON.stringify(document)); } finally { db.close(); }
    const result = spawnSync(join(packageRoot, "bin/fabric"), ["graph-owner", document.graph_id, workspace],
      { cwd: workspace, encoding: "utf8", env, timeout: 5_000 });
    expect(result.status, result.stderr).toBe(1);
    expect(graphRow(document.graph_id).state).toBe("running");
    expect(nodes(document.graph_id).get("a")!.holds_slot).toBe(true);
    const audit = new Database(databasePath(env));
    try { expect(JSON.stringify(audit.prepare("SELECT detail FROM activity").all())).toContain("startup fencing exhausted"); }
    finally { audit.close(); }
  });

  it("fences live gate groups before stopping a resumed graph whose stored document changed", async () => {
    const linked = join(root, "linked");
    git(workspace, "worktree", "add", "-q", "-b", "work", linked);
    const argv = [process.execPath, "-e", "setInterval(() => {}, 1000)"];
    const document = { schema_version: 1, graph_id: "g-012abc", revision: 1, authority: { mode: "worktree_write" },
      worktrees: { w: { path: linked } }, limits: { gate_allowlist: [argv] },
      nodes: [reader("edit", { adapter: "cursor", model: "composer-2.5" }, { authority: { mode: "worktree_write" }, worktree: "w",
        gate: { fast: [argv] } })] } as GraphDocument;
    const chair = { project: workspace, cwd: workspace, agentId: "codex-test", provider: "codex" };
    const plan = validateGraph(document, chair, catalogueSnapshot(repositoryRoot), env).plan!;
    withStore((store) => store.create(chair, document, plan));
    const id = document.graph_id;
    const gatePidPath = join(root, "gate.pid"), gateChildPidPath = join(root, "gate-child.pid");
    const startGate = "const { spawn } = require('node:child_process'); const { writeFileSync } = require('node:fs'); const childScript = \"const { spawn } = require('node:child_process'); const { writeFileSync } = require('node:fs'); const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore' }); writeFileSync(process.argv[1], String(child.pid)); child.unref();\"; const leader = spawn(process.execPath, ['-e', childScript, process.argv[2]], { detached: true, stdio: 'ignore' }); writeFileSync(process.argv[1], String(leader.pid)); leader.unref();";
    execFileSync(process.execPath, ["-e", startGate, gatePidPath, gateChildPidPath], { stdio: "ignore" });
    const gatePid = Number(readFileSync(await waitForFile(gatePidPath), "utf8"));
    const gateChildPid = Number(readFileSync(await waitForFile(gateChildPidPath), "utf8"));
    tracked.add(gatePid); tracked.add(gateChildPid);
    await waitFor(() => !alive(gatePid) && alive(-gatePid), "gate leader to exit while its group remains live");
    const writer = plan.nodes[0]!.worktree!;
    withStore((store) => {
      const row = store.current(workspace, id).get("edit")!;
      expect(store.reserve(row, { maxWidth: 1, cap: 1, handOccupancy: 0,
        writer: { path: writer.path, gitDir: writer.gitDir, token: "test-reservation", owner: { agentId: "codex-test" } } }, "codex-test")).toBe("reserved");
      expect(store.transition(row, ["running"], "gating")).toBe(true);
      store.setGates(row, [{ index: 0, token: "test-gate", argv_hash: "test-gate", pid: gatePid, pgid: gatePid, started_at: null }]);
    });
    const db = new Database(databasePath(env));
    try {
      const changedDocument = withStore((store) => store.document(workspace, id, 1));
      changedDocument.nodes[0]!.brief = { prompt: "Changed after validation" };
      db.prepare("UPDATE graph_revisions SET document_json = ? WHERE graph_id = ?").run(JSON.stringify(changedDocument), id);
    } finally { db.close(); }
    expect(fabric(["graph-owner", id, workspace]).status).toBe(1);
    expect(graphRow(id).state).toBe("stopped");
    expect(alive(gatePid)).toBe(false);
    expect(alive(gateChildPid)).toBe(false);
    expect(alive(-gatePid)).toBe(false);
  });

  it("rejects a stale zero exit when a recovered gate launch cannot open stdout", async () => {
    const argv = ["/bin/sh", "-c", `echo $$ > ${root}/gate.pid; while :; do sleep 0.1; done`];
    const id = run({ limits: { gate_allowlist: [argv] },
      nodes: [reader("a", { adapter: "opencode", alias: "glm-free" }, { gate: { fast: [argv] } })] }).graph_id as string;
    tracked.add(Number(readFileSync(await waitForFile(join(root, "gate.pid")), "utf8")));
    const gate = nodes(id).get("a")!.gate_procs[0]!;
    tracked.add(gate.pid!);
    const owner = graphRow(id).owner_pid!;
    process.kill(owner, "SIGKILL");
    await waitFor(() => !alive(owner), "old graph owner to die");
    const dir = join(workspace, ".agent-run/graphs", id, "r1/a/a1/gates");
    writeFileSync(join(dir, "0.exit.json"), JSON.stringify({ token: gate.token, exit_code: 0, signal: null, finished_at: new Date().toISOString() }));
    rmSync(join(dir, "0.out"));
    mkdirSync(join(dir, "0.out"));
    expect(fabric(["graph", "resume", id]).status).toBe(0);
    await waitFor(() => {
      const graph = graphRow(id);
      if (graph.owner_pid !== owner && graph.owner_pid !== null) tracked.add(graph.owner_pid);
      return graph.state !== "running";
    }, "recovered graph to finish");
    expect(nodes(id).get("a")!.state).toBe("failed");
    expect(nodes(id).get("a")!.outcome?.reason).toBe("gate 0 recorded no exit");
    expect(nodes(id).get("a")!.gate_procs[0]!.token).not.toBe(gate.token);
  });

  it("cancels during a forking gate and leaves no group member alive before the worktree reservation releases", async () => {
    const linked = join(root, "linked");
    git(workspace, "worktree", "add", "-q", "-b", "work", linked);
    const gitDir = execFileSync("git", ["-C", linked, "rev-parse", "--absolute-git-dir"], { encoding: "utf8" }).trim();
    const script = join(root, "gate.sh");
    writeFileSync(script, `trap '' TERM\n/bin/sh -c 'trap "" TERM; echo $$ > ${root}/child.pid; while :; do sleep 0.1; done' &\necho $$ > ${root}/gate.pid\nwait\n`);
    const argv = ["/bin/sh", script];
    const view = run({ authority: { mode: "worktree_write" }, worktrees: { w: { path: linked } }, limits: { gate_allowlist: [argv] },
      nodes: [reader("edit", { adapter: "cursor", model: "composer-2.5" }, { authority: { mode: "worktree_write" }, worktree: "w",
        gate: { fast: [argv] } })] });
    const id = view.graph_id as string;
    const gatePid = Number(readFileSync(await waitForFile(join(root, "gate.pid")), "utf8"));
    const childPid = Number(readFileSync(await waitForFile(join(root, "child.pid")), "utf8"));
    tracked.add(gatePid); tracked.add(childPid);
    expect(nodes(id).get("edit")!.state).toBe("gating");
    expect(existsSync(join(gitDir, RESERVATION_FILE)), "the graph holds the worktree while its gate runs").toBe(true);
    expect(fabric(["graph", "cancel", id]).status).toBe(0);
    let early = false;
    while (graphRow(id).state === "running") {
      const reserved = existsSync(join(gitDir, RESERVATION_FILE)) &&
        withStore((store) => store.reservation(realpathSync(linked))) !== undefined;
      if (!reserved && (alive(gatePid) || alive(childPid))) early = true;
      await delay(10);
    }
    expect(early, "the reservation released while a gate group member was still alive").toBe(false);
    expect(graphRow(id).state).toBe("cancelled");
    expect(nodes(id).get("edit")!.state).toBe("cancelled");
    expect(alive(gatePid) || alive(childPid)).toBe(false);
    expect(existsSync(join(gitDir, RESERVATION_FILE))).toBe(false);
  });

  it("force resumes an unverifiable owner with an audit trail and recovers its reserved slot", async () => {
    const document = { schema_version: 1, graph_id: "g-012abc", revision: 1,
      nodes: [reader("a", { adapter: "opencode", alias: "glm-free" })] } as GraphDocument;
    const chair = { project: workspace, cwd: workspace, agentId: "codex-test", provider: "codex" };
    const plan = validateGraph(document, chair, catalogueSnapshot(repositoryRoot), env).plan!;
    withStore((store) => {
      store.create(chair, document, plan);
      store.claimOwner(workspace, document.graph_id, process.pid, null, () => false);
      store.reserve(store.current(workspace, document.graph_id).get("a")!, { maxWidth: 1, cap: 1, handOccupancy: 0 }, chair.agentId);
    });
    const result = fabric(["graph", "resume", document.graph_id, "--force"]);
    expect(result.status, result.stdout + result.stderr).toBe(0);
    trackOwner(document.graph_id, workspace);
    await waitFor(() => graphRow(document.graph_id).state !== "running", "forced recovery to finish");
    expect(graphRow(document.graph_id).state).toBe("done");
    expect(nodes(document.graph_id).get("a")!.holds_slot).toBe(false);
    expect(alive(process.pid), "never signal a possibly reused owner PID").toBe(true);
    const audit = new Database(databasePath(env));
    try {
      const forced = audit.prepare("SELECT agent_id, detail FROM activity WHERE detail LIKE '%forced owner takeover%'").all() as Array<{ agent_id: string; detail: string }>;
      expect(forced).toHaveLength(1);
      expect(forced[0]!.agent_id).toMatch(/^codex/u);
      expect(forced[0]!.detail).toContain(String(process.pid));
    } finally { audit.close(); }
  });

  it.each(["takeover", "orphan cleanup"])("keeps a live owner with an unknown start time excluded from %s", async (operation) => {
    const document = { schema_version: 1, graph_id: "g-012abc", revision: 1,
      nodes: [reader("a", { adapter: "opencode", alias: "glm-free" })] } as GraphDocument;
    const chair = { project: workspace, cwd: workspace, agentId: "codex-test", provider: "codex" };
    const plan = validateGraph(document, chair, catalogueSnapshot(repositoryRoot)).plan!;
    withStore((store) => {
      store.create(chair, document, plan);
      expect(store.claimOwner(workspace, document.graph_id, process.pid, null, () => false)).toBe(true);
      expect(store.reserve(store.current(workspace, document.graph_id).get("a")!,
        { maxWidth: 1, cap: 1, handOccupancy: 0 }, chair.agentId)).toBe("reserved");
    });
    if (operation === "takeover") {
      expect(fabric(["graph-owner", document.graph_id, workspace]).status).toBe(0);
      expect(graphRow(document.graph_id).owner_pid).toBe(process.pid);
      const status = JSON.parse(fabric(["graph", "status", document.graph_id, "--json"]).stdout);
      expect(status.owner_alive).toBe(true);
      expect(status.fix).toBeUndefined();
    } else {
      env.AGENT_FABRIC_PRODUCT_ROOT = buildProduct({ opencode: 1 });
      const id = run({ nodes: [reader("b", { adapter: "opencode", alias: "glm-free" })] }).graph_id as string;
      await waitFor(() => graphRow(id).owner_pid !== null, "second graph owner to start");
      await delay(1_000);
      expect(nodes(document.graph_id).get("a")!.holds_slot, "unknown owner identity still holds its reserved slot").toBe(true);
      expect(nodes(id).get("b")!.state).toBe("pending");
    }
  });

  it("never runs more lanes in one pool than its cap across two concurrent graphs", async () => {
    const release = join(root, "release-pool");
    const holding = (prefix: string) => ["1", "2", "3"].map((index) =>
      reader(`${prefix}${index}`, { adapter: "opencode", alias: "glm-free" }, { brief: { prompt: `hold ${release}` } }));
    const first = run({ nodes: holding("x") }).graph_id as string;
    const second = run({ nodes: holding("y") }).graph_id as string;
    let peak = 0;
    const deadline = Date.now() + 4_000;
    while (Date.now() < deadline) {
      const live = [...nodes(first).values(), ...nodes(second).values()].filter((row) => row.holds_slot).length;
      peak = Math.max(peak, live);
      await delay(10);
    }
    expect(peak).toBe(2);
    writeFileSync(release, "");
    for (const id of [first, second]) {
      while (graphRow(id).state === "running") {
        const live = [...nodes(first).values(), ...nodes(second).values()].filter((row) => row.holds_slot).length;
        peak = Math.max(peak, live);
        await delay(10);
      }
      expect(graphRow(id).state).toBe("done");
    }
    expect(peak).toBe(2);
  });

  it("makes a node dispatched into another registered repository visible to lanes --wait there", async () => {
    const other = join(root, "other");
    mkdirSync(other);
    git(other, "init", "-q", "-b", "main");
    const store = new Store(databasePath(env));
    try {
      store.announce({ project: workspace, cwd: workspace, agentId: "codex-graph", provider: "codex" });
      store.announce({ project: other, cwd: other, agentId: "codex-other", provider: "codex" });
    } finally { store.close(); }
    const release = join(root, "release-other");
    const view = run({ nodes: [reader("remote", { adapter: "opencode", alias: "glm-free" }, { cwd: other, brief: { prompt: `hold ${release}` } })] });
    const id = view.graph_id as string;
    await waitFor(() => nodes(id).get("remote")?.run_id != null, "the cross-project node to launch");
    const runId = nodes(id).get("remote")!.run_id!;
    const listed = JSON.parse(fabric(["lanes", "--json"], other).stdout) as { runs: Array<Record<string, unknown>> };
    expect(listed.runs.map((lane) => lane.run_id)).toContain(runId);
    let returned = false;
    const waiting = new Promise<number | null>((resolveWait) => {
      const child = spawn(join(packageRoot, "bin/fabric"), ["lanes", "--wait", "--all", "--timeout", "20"], { cwd: other, env, stdio: "ignore" });
      child.once("exit", (code) => { returned = true; resolveWait(code); });
    });
    await delay(1_500);
    expect(returned, "lanes --wait blocks on the graph's lane").toBe(false);
    writeFileSync(release, "");
    expect(await waiting, "lanes --wait from the target repository returns once the node ends").toBe(0);
    await waitFor(() => graphRow(id).state === "done", "graph to finish");
  });

  it("blocks the descendants of a failed node, lets independent nodes finish and tells the chair", async () => {
    const view = run({ nodes: [
      reader("broken", { adapter: "opencode", alias: "glm-free" }, { brief: { prompt: "fail on purpose" } }),
      reader("after", { adapter: "opencode", alias: "glm-free" }, { depends_on: ["broken"] }),
      reader("aside", { adapter: "opencode", model: "deepseek-free" }),
    ] });
    const id = view.graph_id as string;
    await waitFor(() => graphRow(id).state !== "running", "graph to stop");
    expect(graphRow(id).state).toBe("stopped");
    expect(Object.fromEntries([...nodes(id)].map(([node, row]) => [node, row.state]))).toEqual({ broken: "failed", after: "blocked", aside: "passed" });
    const inbox = new Store(databasePath(env));
    try {
      const bodies = inbox.inbox({ project: workspace, cwd: workspace, agentId: graphRow(id).chair.agentId, provider: "codex" }, { peek: true })
        .map((message: { body: string }) => message.body);
      expect(bodies.some((body: string) => body.includes(`graph ${id} node broken failed`))).toBe(true);
      expect(bodies.some((body: string) => body.startsWith(`graph ${id} stopped: failed broken`))).toBe(true);
    } finally { inbox.close(); }
  });
});
