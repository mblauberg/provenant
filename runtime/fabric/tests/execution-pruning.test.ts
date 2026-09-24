import { execFileSync, spawn } from "node:child_process";
import {
  chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync,
  rmSync, utimesSync, writeFileSync,
} from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";

import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { cancelActiveExecutions, dispatchConfiguredProvider } from "../src/execution.js";
import {
  DEFAULT_RETENTION_HOURS,
  fabricStatus,
  OWNER_RECORD_NAME,
  processStartedAt,
  pruneDispatchRuns,
  retentionHours,
  reapOrphanedRuns,
} from "../src/run-registry.js";
import type { Identity } from "../src/identity.js";

const testDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(testDirectory, "../../..");
const fixture = join(testDirectory, "lifecycle-owner-fixture.mjs");
const tsxLoader = createRequire(import.meta.url).resolve("tsx");
const fixturePython = execFileSync("/usr/bin/env", [
  "python3", "-c", "import sys; print(sys.executable)",
], { encoding: "utf8" }).trim();

let temporaryDirectory: string;
let workspace: string;
let product: string;
let identity: Identity;
let ownerEnvironment: NodeJS.ProcessEnv;
const spawnedPids: number[] = [];

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", `'"'"'`)}'`;
}

/** A product root whose dispatch owners are the lifecycle fixture. */
function buildProduct(root: string): string {
  const fake = join(root, "product");
  const owners = join(fake, "skills/orchestrate/scripts");
  const helpers = join(fake, "scripts/lib");
  mkdirSync(owners, { recursive: true });
  mkdirSync(helpers, { recursive: true });
  copyFileSync(
    join(repositoryRoot, "scripts/lib/harness-python.sh"),
    join(helpers, "harness-python.sh"),
  );
  for (const name of ["run_dir_init.sh", "dispatch_run.py", "batch_run.py", "run_controls.py"]) {
    const owner = join(owners, name);
    writeFileSync(owner, name === "run_dir_init.sh"
      ? `#!/bin/sh\nPROVENANT_FIXTURE_OWNER=${shellQuote(name)} exec ${shellQuote(process.execPath)} ${shellQuote(fixture)} "$@"\n`
      : `#!/usr/bin/env python3\nimport os, sys\nos.environ["PROVENANT_FIXTURE_OWNER"] = ${JSON.stringify(name)}\nos.execv(${JSON.stringify(process.execPath)}, [${JSON.stringify(process.execPath)}, ${JSON.stringify(fixture)}, *sys.argv[1:]])\n`);
    chmodSync(owner, 0o755);
  }
  return fake;
}

function alive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
}

async function waitFor(condition: () => boolean, label: string, timeoutMs = 10_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (!condition() && Date.now() < deadline) await delay(20);
  expect(condition(), label).toBe(true);
}

async function waitForFile(path: string, label = `file ${path}`): Promise<string> {
  await waitFor(() => existsSync(path), label);
  return readFileSync(path, "utf8");
}

function runDirectories(): string[] {
  const root = join(workspace, ".agent-run", "runs");
  return existsSync(root) ? readdirSync(root).filter((name) => /^\d{8}-\d{4}-(dispatch|batch)-/u.test(name)).sort() : [];
}

async function startSleepingRun(prompt: string): Promise<Record<string, unknown>> {
  const started = await dispatchConfiguredProvider(
    { adapter: "codex", prompt, task_id: "sleeping-task", wait_seconds: 0 },
    identity,
    new AbortController().signal,
    ownerEnvironment,
  );
  expect(started.status, JSON.stringify(started)).toBe("running");
  spawnedPids.push(Number(started.pid));
  return started;
}

beforeEach(() => {
  temporaryDirectory = mkdtempSync(join(tmpdir(), "fabric-lifecycle-"));
  workspace = join(temporaryDirectory, "workspace");
  mkdirSync(workspace, { recursive: true });
  product = buildProduct(temporaryDirectory);
  identity = { project: workspace, cwd: workspace, agentId: "lifecycle", provider: "codex" };
  ownerEnvironment = {
    ...process.env,
    AGENT_FABRIC_PRODUCT_ROOT: product,
    HARNESS_PYTHON: fixturePython,
  };
});

afterEach(async () => {
  cancelActiveExecutions();
  for (const pid of spawnedPids.splice(0)) {
    try {
      process.kill(-pid, "SIGKILL");
    } catch { /* group already gone */ }
    try {
      process.kill(pid, "SIGKILL");
    } catch { /* already gone */ }
  }
  await delay(50);
  rmSync(temporaryDirectory, { recursive: true, force: true });
});

describe("run directory pruning", () => {
  it("does not report interrupted while a finalising owner is alive", async () => {
    const runDir = join(workspace, ".agent-run", "runs", "20260801-1200-dispatch-finalising-abcdef");
    const attemptDir = join(runDir, "tasks", "task-1", "attempt-001");
    mkdirSync(attemptDir, { recursive: true });
    const row = JSON.parse(readFileSync(join(testDirectory, "fixtures/attempt.json"), "utf8"));
    row.run_id = "mcp-abcdef";
    row.state = "running";
    row.status = null;
    writeFileSync(join(attemptDir, "attempt.json"), JSON.stringify(row));
    writeFileSync(join(runDir, "RUN_RECEIPT.json"), JSON.stringify({ status: "active" }));
    writeFileSync(join(runDir, "dispatch-status.json"), JSON.stringify({
      status: "interrupted", finished_at: new Date().toISOString(), task_id: "task-1",
    }));
    writeFileSync(join(runDir, OWNER_RECORD_NAME), JSON.stringify({
      schema_version: 1, kind: "dispatch", owner_pid: process.pid, owner_pgid: process.pid,
      owner_started_at: null, host_pid: process.pid, host_started_at: null,
      run_token: "finalising", workspace, task_id: "task-1", started_at: new Date().toISOString(),
      owner_stdout: "", owner_stderr: "",
    }));
    expect(await fabricStatus(workspace, "mcp-abcdef")).toMatchObject({ state: "running", status: null });
    rmSync(join(runDir, OWNER_RECORD_NAME));
    expect(await fabricStatus(workspace, "mcp-abcdef")).toMatchObject({ state: "terminal", status: "interrupted" });
  });
  it("retains a resumable input required run past failed retention", () => {
    const runDir = join(workspace, ".agent-run", "runs", "20260801-1200-dispatch-question-a1b2c3");
    mkdirSync(runDir, { recursive: true });
    const receipt = join(runDir, "RUN_RECEIPT.json");
    writeFileSync(receipt, JSON.stringify({ status: "input_required", resumable: true }));
    const past = new Date(Date.now() - 30 * 86400000);
    utimesSync(receipt, past, past);
    utimesSync(runDir, past, past);
    pruneDispatchRuns(workspace, ownerEnvironment);
    expect(existsSync(runDir)).toBe(true);
  });
  function ageRun(name: string, hoursOld: number): { runDir: string; stdout: string; stderr: string } {
    const runRoot = join(workspace, ".agent-run");
    mkdirSync(runRoot, { recursive: true });
    const runDir = join(runRoot, name);
    mkdirSync(runDir, { recursive: true });
    writeFileSync(join(runDir, "MANIFEST.md"), "# aged fixture\n");
    const stdout = `${runDir}-owner.stdout.jsonl`;
    const stderr = `${runDir}-owner.stderr.log`;
    writeFileSync(stdout, "{}\n");
    writeFileSync(stderr, "");
    const when = new Date(Date.now() - hoursOld * 3_600_000);
    for (const path of [join(runDir, "MANIFEST.md"), runDir, stdout, stderr]) utimesSync(path, when, when);
    return { runDir, stdout, stderr };
  }

  async function ordinaryDispatch(environment: NodeJS.ProcessEnv = ownerEnvironment): Promise<void> {
    const done = await dispatchConfiguredProvider(
      { adapter: "codex", prompt: "ordinary run", wait_seconds: 5 },
      identity,
      new AbortController().signal,
      environment,
    );
    expect(done.status).toBe("ok");
  }

  it("ages out mcp-* run directories and their sibling owner logs", async () => {
    const aged = ageRun("mcp-aged01", 24 * 30);
    await ordinaryDispatch();
    expect(existsSync(aged.runDir), "an aged run directory survived the dispatch path").toBe(false);
    expect(existsSync(aged.stdout), "an aged sibling owner stdout log survived").toBe(false);
    expect(existsSync(aged.stderr), "an aged sibling owner stderr log survived").toBe(false);
  });

  it("keeps a run inside the retention window", async () => {
    const fresh = ageRun("mcp-fresh01", 1);
    pruneDispatchRuns(workspace, ownerEnvironment);
    expect(existsSync(fresh.runDir)).toBe(true);
    expect(existsSync(fresh.stdout)).toBe(true);
  });

  it("takes the retention from configuration", async () => {
    const aged = ageRun("mcp-aged02", 3);
    pruneDispatchRuns(workspace, { ...ownerEnvironment, AGENT_FABRIC_RUN_RETENTION_HOURS: "2" });
    expect(existsSync(aged.runDir), "a configured two-hour retention did not apply").toBe(false);
  });

  it("leaves everything that is not an mcp- run directory alone", async () => {
    const runRoot = join(workspace, ".agent-run");
    mkdirSync(runRoot, { recursive: true });
    const foreign = join(runRoot, "20200101T000000Z");
    mkdirSync(foreign, { recursive: true });
    const when = new Date(Date.now() - 24 * 3_600_000 * 400);
    utimesSync(foreign, when, when);
    pruneDispatchRuns(workspace, ownerEnvironment);
    expect(existsSync(foreign), "pruning reached beyond the MCP dispatch path").toBe(true);
  });

  it("never prunes a run still in flight", async () => {
    const started = await startSleepingRun("sleep with provider");
    const runDir = String((started.paths as Record<string, string>).run_dir);
    await waitForFile(join(runDir, OWNER_RECORD_NAME));
    // The owner writes its pid last, so waiting for it means nothing else will
    // touch the directory and quietly refresh the mtime under the backdate.
    await waitForFile(join(runDir, "sleeping.pid"));
    spawnedPids.push(Number(started.pid));
    // Backdate the run and every sibling it owns, so nothing but liveness can
    // be what keeps it: age must not be the reason it survives.
    const when = new Date(Date.now() - 24 * 3_600_000 * 365);
    const runRoot = join(workspace, ".agent-run");
    const name = runDir.split("/").pop()!;
    utimesSync(runDir, when, when);
    for (const entry of readdirSync(runRoot)) {
      if (entry.startsWith(`${name}-`)) utimesSync(join(runRoot, entry), when, when);
    }

    await ordinaryDispatch();
    expect(existsSync(runDir), "an in-flight run was pruned").toBe(true);
    expect(existsSync(join(runDir, OWNER_RECORD_NAME))).toBe(true);
    expect(runDirectories()).toContain(runDir.split("/").pop());
  });

  it("keeps an aged run while its recorded provider session is live", () => {
    const aged = ageRun("mcp-live-provider", 24 * 30);
    const provider = spawn(process.execPath, ["-e", "setInterval(() => undefined, 1000)"], {
      detached: true,
      stdio: "ignore",
    });
    provider.unref();
    const providerPid = provider.pid;
    expect(providerPid).toBeDefined();
    spawnedPids.push(providerPid!);
    const providerStartedAt = processStartedAt(providerPid!);
    expect(providerStartedAt).not.toBeNull();
    const runToken = "live-provider-token";
    writeFileSync(join(aged.runDir, OWNER_RECORD_NAME), JSON.stringify({
      schema_version: 1,
      kind: "dispatch",
      run_dir: aged.runDir,
      workspace,
      run_token: runToken,
      owner_pid: 999_991,
      owner_pgid: 999_991,
      owner_started_at: null,
      host_pid: 999_992,
      host_started_at: null,
      started_at: new Date().toISOString(),
      owner_stdout: aged.stdout,
      owner_stderr: aged.stderr,
      task_id: "live-provider-task",
    }, null, 2) + "\n");
    writeFileSync(join(aged.runDir, "dispatch-provider.json"), JSON.stringify({
      run_token: runToken,
      provider_pid: providerPid,
      provider_pgid: providerPid,
      provider_started_at: providerStartedAt,
    }, null, 2) + "\n");
    const when = new Date(Date.now() - 24 * 3_600_000 * 30);
    utimesSync(aged.runDir, when, when);
    utimesSync(aged.stdout, when, when);
    utimesSync(aged.stderr, when, when);

    expect(pruneDispatchRuns(workspace, {})).toStrictEqual([]);
    expect(existsSync(aged.runDir)).toBe(true);
    expect(alive(providerPid!)).toBe(true);
  });
});

describe("retention configuration", () => {
  it("defaults when nothing is configured and when the value is unusable", () => {
    expect(retentionHours({})).toBe(DEFAULT_RETENTION_HOURS);
    expect(retentionHours({ AGENT_FABRIC_RUN_RETENTION_HOURS: "not-a-number" }))
      .toBe(DEFAULT_RETENTION_HOURS);
    expect(retentionHours({ AGENT_FABRIC_RUN_RETENTION_HOURS: "-1" })).toBe(DEFAULT_RETENTION_HOURS);
    expect(retentionHours({ AGENT_FABRIC_RUN_RETENTION_HOURS: "2" })).toBe(2);
    expect(retentionHours({ AGENT_FABRIC_RUN_RETENTION_HOURS: "0" })).toBe(0);
  });
});

it('closes an abandoned active receipt as interrupted and keeps protected runs', async () => {
 const root = join(workspace,'.agent-run/runs');
 const run=join(root,'20260920-1010-dispatch-fixture-abcdef');
 mkdirSync(run,{recursive:true});
 const receipt=join(run,'RUN_RECEIPT.json');
 writeFileSync(receipt,JSON.stringify({status:'active'}));
 const old=new Date(Date.now()-72*3600000);utimesSync(receipt,old,old);
 writeFileSync(join(run,'KEEP'),'');
 await reapOrphanedRuns(workspace);
 expect(JSON.parse(readFileSync(receipt,'utf8')).status).toBe('interrupted');
 expect(pruneDispatchRuns(workspace,{AGENT_FABRIC_RUN_RETENTION_HOURS:'0'})).not.toContain(run);
});

it('retains failed v2 runs for fourteen days and unknown receipts for triage', () => {
 const root=join(workspace,'.agent-run/runs');const when=new Date(Date.now()-8*86400000);
 for(const status of ['failed','mystery']) {
  const dir=join(root,`20260915-1010-dispatch-${status}-abcdef`);mkdirSync(dir,{recursive:true});
  const receipt=join(dir,'RUN_RECEIPT.json');writeFileSync(receipt,JSON.stringify({status}));utimesSync(receipt,when,when);utimesSync(dir,when,when);
 }
 expect(pruneDispatchRuns(workspace,{})).toEqual([]);
});
