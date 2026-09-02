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
import { listRecordedRuns, OWNER_RECORD_NAME, reapOrphanedRuns } from "../src/run-registry.js";
import type { Identity } from "../src/identity.js";

const testDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = resolve(testDirectory, "../../..");
const packageRoot = resolve(testDirectory, "..");
const fixture = join(testDirectory, "lifecycle-owner-fixture.mjs");
const hostWorker = join(testDirectory, "dispatch-host-worker.ts");
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

async function waitForPid(path: string): Promise<number> {
  const pid = Number((await waitForFile(path)).trim());
  expect(Number.isInteger(pid) && pid > 0, `no pid in ${path}`).toBe(true);
  spawnedPids.push(pid);
  return pid;
}

function runDirectories(): string[] {
  const root = join(workspace, ".agent-run");
  return existsSync(root) ? readdirSync(root).filter((name) => name.startsWith("mcp-")).sort() : [];
}

function fabricCli(args: string[]): string {
  return execFileSync(join(packageRoot, "bin/fabric"), args, {
    cwd: workspace,
    encoding: "utf8",
    env: {
      ...ownerEnvironment,
      FABRIC_NODE: process.execPath,
      AGENT_FABRIC_TSX_LOADER: tsxLoader,
      AGENT_FABRIC_STATE_DIRECTORY: join(temporaryDirectory, "state"),
    },
  });
}

async function startSleepingRun(prompt: string): Promise<Record<string, unknown>> {
  const started = await dispatchConfiguredProvider(
    { adapter: "codex", prompt, task_id: "sleeping-task", wait_seconds: 0 },
    identity,
    new AbortController().signal,
    ownerEnvironment,
  );
  expect(started.status, JSON.stringify(started)).toBe("running");
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

describe("owner records", () => {
  it("records the owner pid and process group inside the run directory", async () => {
    const started = await startSleepingRun("sleep with provider");
    const runDir = String(started.paths && (started.paths as Record<string, string>).run_dir);
    const recordPath = join(runDir, OWNER_RECORD_NAME);
    const record = JSON.parse(await waitForFile(recordPath)) as Record<string, unknown>;
    expect(record.schema_version).toBe(1);
    expect(record.kind).toBe("dispatch");
    expect(record.owner_pid).toBe(started.pid);
    // A detached owner leads its own group, which is what makes a group signal
    // reach the provider without reaching this process.
    expect(record.owner_pgid).toBe(record.owner_pid);
    expect(record.host_pid).toBe(process.pid);
    expect(record.workspace).toBe(identity.cwd);
    expect(record.task_id).toBe("sleeping-task");
    spawnedPids.push(Number(record.owner_pid));
  }, 40_000);

  it("survives a cold start, so a fresh process can list the run", async () => {
    const started = await startSleepingRun("sleep with provider");
    const runDir = String((started.paths as Record<string, string>).run_dir);
    await waitForFile(join(runDir, OWNER_RECORD_NAME));
    spawnedPids.push(Number(started.pid));

    // A separate process shares nothing but the run directory on disk.
    const listed = JSON.parse(fabricCli(["dispatch", "list", "--json"])) as {
      runs: Array<Record<string, unknown>>;
    };
    const run = listed.runs.find((entry) => entry.run_dir === runDir);
    expect(run, JSON.stringify(listed)).toBeDefined();
    expect(run?.owner_pid).toBe(started.pid);
    expect(run?.running).toBe(true);
  }, 40_000);

  it("removes the record when the owner exits", async () => {
    const done = await dispatchConfiguredProvider(
      { adapter: "codex", prompt: "ordinary run", wait_seconds: 5 },
      identity,
      new AbortController().signal,
      ownerEnvironment,
    );
    expect(done.status).toBe("succeeded");
    const runDir = String((done.paths as Record<string, string>).run_dir);
    expect(existsSync(join(runDir, OWNER_RECORD_NAME))).toBe(false);
  }, 40_000);
});

describe("cancellation", () => {
  it("signals the owner process group, so the provider child dies too", async () => {
    const started = await startSleepingRun("sleep with provider");
    const runDir = String((started.paths as Record<string, string>).run_dir);
    const providerPid = await waitForPid(join(runDir, "provider.pid"));
    spawnedPids.push(Number(started.pid));
    expect(alive(providerPid)).toBe(true);

    cancelActiveExecutions();
    await waitFor(() => !alive(providerPid), "the provider child outlived the cancellation");
  }, 40_000);

  it("cancels a cold-start run before the attempt directory exists", async () => {
    const started = await startSleepingRun("sleep before the attempt directory");
    const runDir = String((started.paths as Record<string, string>).run_dir);
    await waitForFile(join(runDir, "sleeping.pid"));
    const providerPid = await waitForPid(join(runDir, "provider.pid"));
    const ownerPid = Number(started.pid);
    spawnedPids.push(ownerPid);
    const attemptDirectory = join(runDir, "dispatch", "tasks", "sleeping-task", "attempt-001");
    expect(existsSync(attemptDirectory), "the cold-start scenario needs no attempt directory").toBe(false);

    // A fresh process holds no in-memory owner, only the recorded run.
    const killed = JSON.parse(fabricCli(["dispatch", "kill", runDir, "--json"])) as Record<string, unknown>;
    expect(killed.signalled).toBe(true);
    await waitFor(() => !alive(providerPid), "the provider survived a cold-start kill");
    await waitFor(() => !alive(ownerPid) || existsSync(join(runDir, "cancelled.marker")),
      "the owner survived a cold-start kill");
    expect(existsSync(attemptDirectory)).toBe(false);
  }, 40_000);
});

describe("orphan reaping", () => {
  it("leaves no running provider process after its MCP host is killed", async () => {
    const host = spawn(process.execPath, ["--import", tsxLoader, hostWorker, workspace, "sleep with provider"], {
      env: ownerEnvironment,
      stdio: ["ignore", "pipe", "inherit"],
    });
    let stdout = "";
    host.stdout.setEncoding("utf8");
    host.stdout.on("data", (chunk: string) => { stdout += chunk; });
    await waitFor(() => stdout.includes("\n"), "the host never reported its run");
    const started = JSON.parse(stdout.split("\n")[0]!) as Record<string, unknown>;
    const runDir = String((started.paths as Record<string, string>).run_dir);
    const providerPid = await waitForPid(join(runDir, "provider.pid"));
    spawnedPids.push(Number(started.pid));

    host.kill("SIGKILL");
    await waitFor(() => host.exitCode !== null || host.signalCode !== null, "the host never died");
    // Nothing has reaped it yet: the orphan is exactly the defect.
    expect(alive(providerPid)).toBe(true);

    const reaped = reapOrphanedRuns(workspace);
    expect(reaped.length).toBeGreaterThan(0);
    await waitFor(() => !alive(providerPid), "the orphaned provider outlived its dead host");
  }, 40_000);

  it("reaps orphans on the dispatch path without a daemon", async () => {
    const host = spawn(process.execPath, ["--import", tsxLoader, hostWorker, workspace, "sleep with provider"], {
      env: ownerEnvironment,
      stdio: ["ignore", "pipe", "inherit"],
    });
    let stdout = "";
    host.stdout.setEncoding("utf8");
    host.stdout.on("data", (chunk: string) => { stdout += chunk; });
    await waitFor(() => stdout.includes("\n"), "the host never reported its run");
    const started = JSON.parse(stdout.split("\n")[0]!) as Record<string, unknown>;
    const runDir = String((started.paths as Record<string, string>).run_dir);
    const providerPid = await waitForPid(join(runDir, "provider.pid"));
    spawnedPids.push(Number(started.pid));
    host.kill("SIGKILL");
    await waitFor(() => host.exitCode !== null || host.signalCode !== null, "the host never died");

    const next = await dispatchConfiguredProvider(
      { adapter: "codex", prompt: "ordinary run", wait_seconds: 5 },
      identity,
      new AbortController().signal,
      ownerEnvironment,
    );
    expect(next.status).toBe("succeeded");
    await waitFor(() => !alive(providerPid), "an ordinary dispatch did not reap the orphan");
  }, 40_000);

  it("never reaps a run whose host is still alive", async () => {
    const started = await startSleepingRun("sleep with provider");
    const runDir = String((started.paths as Record<string, string>).run_dir);
    const providerPid = await waitForPid(join(runDir, "provider.pid"));
    spawnedPids.push(Number(started.pid));

    expect(reapOrphanedRuns(workspace)).toStrictEqual([]);
    await delay(100);
    expect(alive(providerPid)).toBe(true);
    expect(listRecordedRuns(workspace).some((run) => run.run_dir === runDir)).toBe(true);
  }, 40_000);
});
