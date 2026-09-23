import { execFileSync, spawn } from "node:child_process";
import {
  chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync,
  rmSync, utimesSync, writeFileSync,
} from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { cancelActiveExecutions, dispatchConfiguredBatch, dispatchConfiguredProvider } from "../src/execution.js";
import {
  listRecordedRuns,
  fabricStatus,
  OWNER_RECORD_NAME,
  processStartedAt,
  reapOrphanedRuns,
} from "../src/run-registry.js";
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
  const root = join(workspace, ".agent-run", "runs");
  return existsSync(root) ? readdirSync(root).filter((name) => /^\d{8}-\d{4}-(dispatch|batch)-/u.test(name)).sort() : [];
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

function fabricCliOutput(args: string[]): { status: 0 | 1; stdout: string } {
  try {
    return { status: 0, stdout: fabricCli(args) };
  } catch (error) {
    const result = error && typeof error === "object"
      ? error as { signal?: unknown; status?: unknown; stdout?: unknown }
      : undefined;
    if (result?.status === 1 && result.signal === null && typeof result.stdout === "string") {
      return { status: 1, stdout: result.stdout };
    }
    throw error;
  }
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

describe("owner records", () => {
  it("launches no owner if the initial status file cannot be written", async () => {
    const result = await dispatchConfiguredProvider({ adapter: "codex", prompt: "sleep without provider", wait_seconds: 0 },
      identity, new AbortController().signal, { ...ownerEnvironment, FIXTURE_STATUS_DIRECTORY: "1" });
    expect(result.status).toBe("rejected");
    const root = join(workspace, ".agent-run");
    const recordPaths = readdirSync(root).map((name) => join(root, name, OWNER_RECORD_NAME)).filter(existsSync);
    const pids = recordPaths.map((path) => Number(JSON.parse(readFileSync(path, "utf8")).owner_pid));
    spawnedPids.push(...pids);
    await delay(200);
    expect(pids.filter(alive), "a failed status write left an owner running").toEqual([]);
    expect(recordPaths).toEqual([]);
  });

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

  it("rejects an empty provider result with task and route context", async () => {
    const done = await dispatchConfiguredProvider(
      { adapter: "codex", prompt: "emit empty provider result", task_id: "empty-result", wait_seconds: 5 },
      identity,
      new AbortController().signal,
      ownerEnvironment,
    );
    expect(done).toMatchObject({
      status: "failed",
      outcome: "empty_output",
      task_id: "empty-result",
      route: {
        adapter: "codex",
        provider_family: "codex",
        resolved_model: "workhorse",
        execution_intent: "ordinary",
      },
    });
  }, 40_000);

  it("keeps custody until a provider outliving its owner has stopped", async () => {
    const done = await dispatchConfiguredProvider(
      { adapter: "codex", prompt: "exit with provider", task_id: "exit-with-provider", wait_seconds: 5 },
      identity,
      new AbortController().signal,
      ownerEnvironment,
    );
    const runDir = String((done.paths as Record<string, string>).run_dir);
    const providerPid = await waitForPid(join(runDir, "provider.pid"));

    expect(alive(providerPid)).toBe(false);
    expect(existsSync(join(runDir, OWNER_RECORD_NAME))).toBe(false);
  }, 40_000);

  it("holds owner cleanup through host shutdown until a resistant provider stops", async () => {
    const started = await dispatchConfiguredProvider(
      { adapter: "codex", prompt: "exit with resistant provider", task_id: "shutdown-provider", wait_seconds: 0 },
      identity,
      new AbortController().signal,
      ownerEnvironment,
    );
    const runDir = String((started.paths as Record<string, string>).run_dir);
    const providerPid = await waitForPid(join(runDir, "provider.pid"));
    const ownerRecord = JSON.parse(await waitForFile(join(runDir, OWNER_RECORD_NAME))) as {
      run_token: string;
    };
    const providerRecord = JSON.parse(await waitForFile(join(runDir, "dispatch-provider.json"))) as {
      run_token: string;
      provider_started_at: string;
    };
    expect(providerRecord.run_token).toBe(ownerRecord.run_token);
    expect(processStartedAt(providerPid)).toBe(providerRecord.provider_started_at);
    expect(Number(execFileSync("/bin/ps", ["-o", "pgid=", "-p", String(providerPid)], {
      encoding: "utf8",
    }).trim())).toBe(providerPid);
    writeFileSync(join(runDir, "exit-owner.release"), "exit\n");
    await waitFor(() => !alive(Number(started.pid)), "the owner never exited");
    await delay(200);
    expect(alive(providerPid)).toBe(true);

    await cancelActiveExecutions();

    expect(alive(providerPid)).toBe(false);
    expect(existsSync(join(runDir, OWNER_RECORD_NAME))).toBe(false);
  }, 40_000);
});

describe("cancellation", () => {
  it("prints an unconfirmed stop reason from dispatch kill", async () => {
    const runDir = join(workspace, ".agent-run", "mcp-cli-reason");
    mkdirSync(runDir, { recursive: true });
    const provider = spawn(process.execPath, ["-e", "setInterval(() => undefined, 1000)"], {
      detached: true,
      stdio: "ignore",
    });
    provider.unref();
    const providerPid = provider.pid!;
    spawnedPids.push(providerPid);
    const wrapperPath = join(temporaryDirectory, "cli-wrapper.mjs");
    const wrapperPidPath = join(temporaryDirectory, "cli-wrapper.pid");
    const releasePath = join(temporaryDirectory, "cli-wrapper.release");
    const cliPath = join(packageRoot, "src", "cli.ts");
    writeFileSync(wrapperPath, `
      import { existsSync, writeFileSync } from "node:fs";
      writeFileSync(${JSON.stringify(wrapperPidPath)}, String(process.pid));
      await new Promise((resolveWait) => {
        const timer = setInterval(() => {
          if (!existsSync(${JSON.stringify(releasePath)})) return;
          clearInterval(timer);
          resolveWait();
        }, 10);
      });
      process.argv = [process.execPath, ${JSON.stringify(cliPath)}, "dispatch", "kill", ${JSON.stringify(runDir)}];
      await import(${JSON.stringify(cliPath)});
    `);
    const cli = spawn(process.execPath, ["--import", tsxLoader, wrapperPath], {
      cwd: workspace,
      env: ownerEnvironment,
      stdio: ["ignore", "pipe", "inherit"],
    });
    spawnedPids.push(cli.pid!);
    let stdout = "";
    cli.stdout.setEncoding("utf8");
    cli.stdout.on("data", (chunk: string) => { stdout += chunk; });
    const finished = new Promise<{ code: number | null; signal: NodeJS.Signals | null }>((resolveFinished) => {
      cli.once("close", (code, signal) => resolveFinished({ code, signal }));
    });
    const cliPid = Number((await waitForFile(wrapperPidPath)).trim());
    const providerStartedAt = processStartedAt(providerPid);
    expect(providerStartedAt).not.toBeNull();
    const runToken = "cli-unconfirmed-token";
    writeFileSync(join(runDir, OWNER_RECORD_NAME), JSON.stringify({
      schema_version: 1,
      kind: "dispatch",
      run_dir: runDir,
      workspace,
      run_token: runToken,
      owner_pid: 999_971,
      owner_pgid: 999_971,
      owner_started_at: null,
      host_pid: 999_972,
      host_started_at: null,
      started_at: new Date().toISOString(),
      owner_stdout: `${runDir}-owner.stdout.jsonl`,
      owner_stderr: `${runDir}-owner.stderr.log`,
      task_id: "cli-unconfirmed-task",
    }) + "\n");
    // The CLI refuses to signal its own process group, leaving this provider
    // live and making the recorded stop outcome explicitly unconfirmed.
    writeFileSync(join(runDir, "dispatch-provider.json"), JSON.stringify({
      run_token: runToken,
      provider_pid: providerPid,
      provider_pgid: cliPid,
      provider_started_at: providerStartedAt,
    }) + "\n");
    writeFileSync(releasePath, "go\n");

    expect(await finished).toEqual({ code: 1, signal: null });
    expect(stdout).toContain("still running");
    expect(alive(providerPid)).toBe(true);
  }, 40_000);

  it("fails dispatch kill after signalling only the owner of a live provider", async () => {
    const runDir = join(workspace, ".agent-run", "mcp-cli-signalled-owner");
    mkdirSync(runDir, { recursive: true });
    const owner = spawn(process.execPath, ["-e", "setInterval(() => undefined, 1000)"], {
      detached: true,
      stdio: "ignore",
    });
    owner.unref();
    const ownerPid = owner.pid!;
    spawnedPids.push(ownerPid);
    const provider = spawn(process.execPath, ["-e", "setInterval(() => undefined, 1000)"], {
      detached: true,
      stdio: "ignore",
    });
    provider.unref();
    const providerPid = provider.pid!;
    spawnedPids.push(providerPid);
    const wrapperPath = join(temporaryDirectory, "cli-signalled-wrapper.mjs");
    const wrapperPidPath = join(temporaryDirectory, "cli-signalled-wrapper.pid");
    const releasePath = join(temporaryDirectory, "cli-signalled-wrapper.release");
    const cliPath = join(packageRoot, "src", "cli.ts");
    writeFileSync(wrapperPath, `
      import { existsSync, writeFileSync } from "node:fs";
      writeFileSync(${JSON.stringify(wrapperPidPath)}, String(process.pid));
      await new Promise((resolveWait) => {
        const timer = setInterval(() => {
          if (!existsSync(${JSON.stringify(releasePath)})) return;
          clearInterval(timer);
          resolveWait();
        }, 10);
      });
      process.argv = [process.execPath, ${JSON.stringify(cliPath)}, "dispatch", "kill", ${JSON.stringify(runDir)}];
      await import(${JSON.stringify(cliPath)});
    `);
    const cli = spawn(process.execPath, ["--import", tsxLoader, wrapperPath], {
      cwd: workspace,
      env: ownerEnvironment,
      stdio: ["ignore", "pipe", "inherit"],
    });
    spawnedPids.push(cli.pid!);
    let stdout = "";
    cli.stdout.setEncoding("utf8");
    cli.stdout.on("data", (chunk: string) => { stdout += chunk; });
    const finished = new Promise<{ code: number | null; signal: NodeJS.Signals | null }>((resolveFinished) => {
      cli.once("close", (code, signal) => resolveFinished({ code, signal }));
    });
    const cliPid = Number((await waitForFile(wrapperPidPath)).trim());
    const ownerStartedAt = processStartedAt(ownerPid);
    const providerStartedAt = processStartedAt(providerPid);
    expect(ownerStartedAt).not.toBeNull();
    expect(providerStartedAt).not.toBeNull();
    const runToken = "cli-signalled-owner-token";
    writeFileSync(join(runDir, OWNER_RECORD_NAME), JSON.stringify({
      schema_version: 1,
      kind: "dispatch",
      run_dir: runDir,
      workspace,
      run_token: runToken,
      owner_pid: ownerPid,
      owner_pgid: ownerPid,
      owner_started_at: ownerStartedAt,
      host_pid: 999_973,
      host_started_at: null,
      started_at: new Date().toISOString(),
      owner_stdout: `${runDir}-owner.stdout.jsonl`,
      owner_stderr: `${runDir}-owner.stderr.log`,
      task_id: "cli-signalled-owner-task",
    }) + "\n");
    writeFileSync(join(runDir, "dispatch-provider.json"), JSON.stringify({
      run_token: runToken,
      provider_pid: providerPid,
      provider_pgid: cliPid,
      provider_started_at: providerStartedAt,
    }) + "\n");
    writeFileSync(releasePath, "go\n");

    expect(await finished).toEqual({ code: 1, signal: null });
    expect(stdout).toContain("still running");
    expect(alive(ownerPid)).toBe(false);
    expect(alive(providerPid)).toBe(true);
  }, 40_000);

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
    const result = fabricCliOutput(["dispatch", "kill", runDir, "--json"]);
    const killed = JSON.parse(result.stdout) as Record<string, unknown>;
    expect(killed.signalled).toBe(true);
    if (killed.reason === "still running") {
      expect(result.status).toBe(1);
    } else {
      expect(result.status).toBe(0);
      expect(killed.reason).toBeUndefined();
    }
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
    spawnedPids.push(host.pid!);
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

    const reaped = await reapOrphanedRuns(workspace);
    expect(reaped.length).toBeGreaterThan(0);
    await waitFor(() => !alive(providerPid), "the orphaned provider outlived its dead host");
  }, 40_000);

  it("keeps the owner record until an orphan that ignores SIGTERM is killed", async () => {
    const host = spawn(process.execPath, ["--import", tsxLoader, hostWorker, workspace, "ignore SIGTERM"], {
      env: ownerEnvironment,
      stdio: ["ignore", "pipe", "inherit"],
    });
    spawnedPids.push(host.pid!);
    let stdout = "";
    host.stdout.setEncoding("utf8");
    host.stdout.on("data", (chunk: string) => { stdout += chunk; });
    await waitFor(() => stdout.includes("\n"), "the host never reported its run");
    const started = JSON.parse(stdout.split("\n")[0]!) as Record<string, unknown>;
    const runDir = String((started.paths as Record<string, string>).run_dir);
    const ownerPid = Number(started.pid);
    spawnedPids.push(ownerPid);
    await waitForFile(join(runDir, "sleeping.pid"));

    host.kill("SIGKILL");
    await waitFor(() => host.exitCode !== null || host.signalCode !== null, "the host never died");

    const attempt = join(runDir, "tasks/task-1/attempt-001");
    mkdirSync(attempt, { recursive: true });
    const row = JSON.parse(readFileSync(join(testDirectory, "fixtures/attempt.json"), "utf8"));
    row.run_id = started.id; row.state = "running"; row.status = null;
    writeFileSync(join(attempt, "attempt.json"), JSON.stringify(row));
    const reaping = reapOrphanedRuns(workspace);
    await waitForFile(join(runDir, "term-ignored.marker"));
    expect(existsSync(join(runDir, OWNER_RECORD_NAME))).toBe(true);

    const [reaped] = await reaping;
    expect(reaped?.escalated).toBe(true);
    expect(alive(ownerPid)).toBe(false);
    expect(existsSync(join(runDir, OWNER_RECORD_NAME))).toBe(false);
    expect(await fabricStatus(workspace, String(started.id))).toMatchObject({state:"terminal",status:"interrupted"});
  }, 40_000);

  it("does not mask a still-running orphan with the host-gone reason", async () => {
    const runDir = join(workspace, ".agent-run", "mcp-still-running");
    mkdirSync(runDir, { recursive: true });
    const provider = spawn(process.execPath, ["-e", "setInterval(() => undefined, 1000)"], {
      detached: true,
      stdio: "ignore",
    });
    provider.unref();
    const providerPid = provider.pid!;
    spawnedPids.push(providerPid);
    const providerStartedAt = processStartedAt(providerPid);
    expect(providerStartedAt).not.toBeNull();
    const runToken = "still-running-token";
    writeFileSync(join(runDir, OWNER_RECORD_NAME), JSON.stringify({
      schema_version: 1,
      kind: "dispatch",
      run_dir: runDir,
      workspace,
      run_token: runToken,
      owner_pid: 999_981,
      owner_pgid: 999_981,
      owner_started_at: null,
      host_pid: 999_982,
      host_started_at: null,
      started_at: new Date().toISOString(),
      owner_stdout: `${runDir}-owner.stdout.jsonl`,
      owner_stderr: `${runDir}-owner.stderr.log`,
      task_id: "still-running-task",
    }) + "\n");
    // This protected group makes the reaper refuse to signal the test host.
    writeFileSync(join(runDir, "dispatch-provider.json"), JSON.stringify({
      run_token: runToken,
      provider_pid: providerPid,
      provider_pgid: process.pid,
      provider_started_at: providerStartedAt,
    }) + "\n");

    const [outcome] = await reapOrphanedRuns(workspace);
    expect(outcome?.reason).toBe("still running");
    expect(existsSync(join(runDir, OWNER_RECORD_NAME))).toBe(true);
    expect(alive(providerPid)).toBe(true);
  });

  it("reaps orphans on the dispatch path without a daemon", async () => {
    const host = spawn(process.execPath, ["--import", tsxLoader, hostWorker, workspace, "sleep with provider"], {
      env: ownerEnvironment,
      stdio: ["ignore", "pipe", "inherit"],
    });
    spawnedPids.push(host.pid!);
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

    await expect(reapOrphanedRuns(workspace)).resolves.toStrictEqual([]);
    await delay(100);
    expect(alive(providerPid)).toBe(true);
    expect(listRecordedRuns(workspace).some((run) => run.run_dir === runDir)).toBe(true);
  }, 40_000);
});

describe("compact status", () => {
  it("selects the newest directory when repeated ids have equal start timestamps", async () => {
    const started = new Date().toISOString();
    for (const name of ["mcp-older", "mcp-younger"]) {
      const dir = join(workspace, ".agent-run", name);
      mkdirSync(dir, { recursive: true });
      writeFileSync(join(dir, "dispatch-status.json"), JSON.stringify({ id: name, task_id: "same-task",
        status: "succeeded", started_at: started }));
      await delay(20);
    }
    expect(await fabricStatus(workspace, "same-task")).toMatchObject({ id: "mcp-younger",
      note: expect.stringMatching(/newest/u) });
  });

  it("prefers a unique run id over a newer run's matching task id", async () => {
    for (const [name, task, age] of [["mcp-original", "original", 1000], ["mcp-newer", "mcp-original", 0]] as const) {
      const dir = join(workspace, ".agent-run", name);
      mkdirSync(dir, { recursive: true });
      writeFileSync(join(dir, "dispatch-status.json"), JSON.stringify({ id: name, task_id: task,
        status: "succeeded", started_at: new Date(Date.now() - age).toISOString() }));
    }
    expect(await fabricStatus(workspace, "mcp-original")).toMatchObject({
      id: "mcp-original", run_dir: realpathSync(join(workspace, ".agent-run", "mcp-original")),
    });
  });

  it("returns unique ids for running and terminal batches and resolves repeated task ids newest", async () => {
    const first = await dispatchConfiguredBatch({ tasks: [{ adapter: "codex", prompt: "empty batch" }], wait_seconds: 0 },
      identity, new AbortController().signal, ownerEnvironment);
    spawnedPids.push(Number(first.pid));
    const second = await dispatchConfiguredBatch({ tasks: [{ adapter: "codex", prompt: "empty batch" }], wait_seconds: 5 },
      identity, new AbortController().signal, ownerEnvironment);
    expect(first.id).toMatch(/^mcp-/u);
    expect(second.id).toMatch(/^mcp-/u);
    expect(first.id).not.toBe(second.id);
    for (const result of [first, second]) {
      expect(await fabricStatus(workspace, String(result.id))).toMatchObject({
        run_dir: (result.paths as Record<string, string>).run_dir,
      });
    }
    for (const id of ["task-1", "batch-001"]) {
      expect(await fabricStatus(workspace, id)).toMatchObject({
        run_dir: (second.paths as Record<string, string>).run_dir,
        note: expect.stringMatching(/newest/u),
      });
    }
  });

  it("does not report an interrupted partial batch as completed", async () => {
    const dir = join(workspace, ".agent-run", "mcp-partial");
    const attemptDir = join(dir, "dispatch", "tasks", "done", "attempt-001");
    mkdirSync(attemptDir, { recursive: true });
    writeFileSync(join(dir, "dispatch-status.json"), JSON.stringify({ id: "partial", kind: "batch",
      status: "running", task_ids: ["done", "missing"], started_at: new Date().toISOString() }));
    writeFileSync(join(attemptDir, "attempt.json"), JSON.stringify({ task_id: "done", status: "succeeded" }));
    expect(await fabricStatus(workspace, "partial")).toMatchObject({ status: "interrupted" });
    expect(await fabricStatus(workspace, "done")).toMatchObject({ id: "done", status: "succeeded" });
    const summaryDir = join(dir, "dispatch", "batches", "partial");
    mkdirSync(summaryDir, { recursive: true });
    writeFileSync(join(summaryDir, "summary.json"), JSON.stringify({ status: "cancelled", batch_id: "partial",
      tasks: [{ task_id: "done", status: "succeeded" }, { task_id: "missing", status: "cancelled" }] }));
    expect(await fabricStatus(workspace, "partial")).toMatchObject({ status: "cancelled",
      result_path: realpathSync(join(summaryDir, "summary.json")) });
  });

  it("retains terminal route and result after the owner exits", async () => {
    const result = await dispatchConfiguredProvider(
      { adapter: "codex", model: "gpt-6-luna", effort: "medium", prompt: "emit empty provider result", task_id: "status-task", wait_seconds: 5 },
      identity, new AbortController().signal, ownerEnvironment,
    );
    const status = await fabricStatus(workspace, "status-task");
    expect(status).toMatchObject({ id: "status-task", status: "failed", stalled: false });
    expect(status.result_path).toEqual((result.paths as Record<string, unknown>).result);
    expect((await fabricStatus(workspace)).runs).toHaveLength(1);
  });
});

describe("front door model selection", () => {
  it("leaves shorthand aliases for the routing owner to resolve", async () => {
    mkdirSync(join(product, "config"));
    copyFileSync(join(repositoryRoot, "config", "model-routing.json"), join(product, "config", "model-routing.json"));
    for (const name of ["luna", "sol", "astra"]) {
      const done = await dispatchConfiguredProvider({ adapter: "codex", alias: name, prompt: "ordinary run", wait_seconds: 5 },
        identity, new AbortController().signal, { ...ownerEnvironment, AGENT_FABRIC_INSTANCE_ROOT: product });
      expect(done).toMatchObject({ status: "succeeded", route: { resolved_model: name } });
    }
  });
  it("treats an empty model as omitted and keeps the alias", async () => {
    const done = await dispatchConfiguredProvider({adapter:"codex",alias:"scout",model:"",prompt:"ordinary run",wait_seconds:5}, identity, new AbortController().signal, ownerEnvironment);
    expect(done).toMatchObject({status:"succeeded",route:{resolved_model:"scout"}});
  });
  it("passes an explicit model and effort without an alias", async () => {
    const done = await dispatchConfiguredProvider({ adapter: "codex", model: "gpt-6-luna", effort: "medium", prompt: "ordinary run", wait_seconds: 5 },
      identity, new AbortController().signal, ownerEnvironment);
    expect(done).toMatchObject({ status: "succeeded", route: { resolved_model: "gpt-6-luna" } });
  });
  it("rejects empty retained batch results per task", async () => {
    const done = await dispatchConfiguredBatch({ tasks: [{ adapter: "codex", prompt: "empty batch" }], wait_seconds: 5 },
      identity, new AbortController().signal, ownerEnvironment);
    expect(done).toMatchObject({ status: "completed", counts: { failed: 1 }, tasks: [{ status: "failed", outcome: "empty_output" }] });
  });
});

describe("status list bounds", () => {
  it("lists only twenty recent runs and reads an older run by id without modifying it", async () => {
    for (let i = 0; i < 25; i++) {
      const dir = join(workspace, ".agent-run", `mcp-${i}`);
      mkdirSync(dir, { recursive: true });
      writeFileSync(join(dir, "dispatch-status.json"), JSON.stringify({ id: `task-${i}`, status: "failed",
        started_at: new Date(Date.now() - (i === 24 ? 90_000_000 : i * 1000)).toISOString() }));
    }
    const path = join(workspace, ".agent-run", "mcp-24", "dispatch-status.json");
    const before = readFileSync(path, "utf8");
    expect((await fabricStatus(workspace)).runs).toHaveLength(20);
    expect(await fabricStatus(workspace, "task-24", 1)).toMatchObject({ id: "task-24", status: "failed" });
    expect(readFileSync(path, "utf8")).toBe(before);
    expect(await fabricStatus(workspace, "../../outside")).toMatchObject({ status: "rejected", error: "run_not_found" });
  });
});

describe("status liveness", () => {
  it("reads final files when the owner finishes during the liveness probe", async () => {
    const dir = join(workspace, ".agent-run", "mcp-race");
    const attemptDir = join(dir, "dispatch", "tasks", "race", "attempt-001");
    mkdirSync(attemptDir, { recursive: true });
    writeFileSync(join(dir, OWNER_RECORD_NAME), JSON.stringify({ schema_version: 1, kind: "dispatch",
      owner_pid: process.pid, owner_pgid: process.pid, owner_started_at: null, task_id: "race" }));
    const statusPath = join(dir, "dispatch-status.json");
    writeFileSync(statusPath, JSON.stringify({ id: "race", status: "running" }));
    const probe = vi.spyOn(process, "kill").mockImplementationOnce(() => {
      writeFileSync(join(attemptDir, "result.md"), "done");
      writeFileSync(join(attemptDir, "attempt.json"), JSON.stringify({ task_id: "race", status: "succeeded",
        result: { path: "dispatch/tasks/race/attempt-001/result.md" } }));
      writeFileSync(statusPath, JSON.stringify({ id: "race", status: "succeeded" }));
      throw new Error("ESRCH: owner exited");
    });
    try {
      expect(await fabricStatus(workspace, "race", 1)).toMatchObject({ status: "succeeded",
        result_path: realpathSync(join(attemptDir, "result.md")) });
    } finally { probe.mockRestore(); }
  });

  it("marks silence only beyond the mode timeout threshold", async () => {
    const dir = join(workspace, ".agent-run", "mcp-silent");
    mkdirSync(dir, { recursive: true });
    const started = new Date(Date.now() - 1_000_000).toISOString();
    writeFileSync(join(dir, OWNER_RECORD_NAME), JSON.stringify({ schema_version: 1, kind: "dispatch",
      run_dir: dir, workspace, run_token: "test", owner_pid: process.pid, owner_pgid: process.pid,
      owner_started_at: processStartedAt(process.pid), started_at: started, task_id: "silent" }));
    const statusPath = join(dir, "dispatch-status.json");
    const record = { id: "silent", status: "running", started_at: started, timeout_seconds: 3600 };
    writeFileSync(statusPath, JSON.stringify(record));
    expect(await fabricStatus(workspace, "silent")).toMatchObject({ status: "running", stalled: true });
    writeFileSync(statusPath, JSON.stringify({ ...record, timeout_seconds: 10800 }));
    expect(await fabricStatus(workspace, "silent")).toMatchObject({ status: "running", stalled: false });
    writeFileSync(statusPath, JSON.stringify(record));
    const scratch = join(temporaryDirectory, "fabric-provider-fixture");
    const rawDir = join(scratch, "cf-dispatch-run.fixture");
    mkdirSync(rawDir, { recursive: true });
    const attemptDir = join(dir, "dispatch", "tasks", "silent", "attempt-001");
    mkdirSync(attemptDir, { recursive: true });
    writeFileSync(join(attemptDir, "provider-output.json"), JSON.stringify({ directory: scratch }));
    writeFileSync(join(rawDir, "raw"), "streaming provider output");
    expect(await fabricStatus(workspace, "silent")).toMatchObject({ status: "running", stalled: false, output_age_seconds: 0 });
    rmSync(scratch, { recursive: true });
    writeFileSync(`${dir}-owner.stdout.jsonl`, "new provider output");
    expect(await fabricStatus(workspace, "silent")).toMatchObject({ status: "running", stalled: false, output_age_seconds: 0 });
  });

  it("waits for terminal output and reads a completed batch task separately", async () => {
    const dir = join(workspace, ".agent-run", "mcp-wait");
    const taskDir = join(dir, "dispatch", "tasks", "child", "attempt-001");
    mkdirSync(taskDir, { recursive: true });
    const worker = spawn(process.execPath, ["-e", "setTimeout(() => {}, 400)"], { stdio: "ignore" });
    const started = new Date().toISOString();
    writeFileSync(join(dir, OWNER_RECORD_NAME), JSON.stringify({ schema_version: 1, kind: "batch",
      run_dir: dir, workspace, run_token: "test", owner_pid: worker.pid, owner_pgid: worker.pid,
      owner_started_at: null, started_at: started, batch_id: "batch-001" }));
    const statusPath = join(dir, "dispatch-status.json");
    writeFileSync(statusPath, JSON.stringify({ id: "batch-001", status: "running", started_at: started }));
    worker.once("exit", () => {
      writeFileSync(join(taskDir, "result.md"), "answer");
      writeFileSync(join(taskDir, "attempt.json"), JSON.stringify({ task_id: "child", status: "succeeded",
        route: { adapter: "claude", resolved_model: "opus" }, result: { path: "dispatch/tasks/child/attempt-001/result.md" } }));
      writeFileSync(statusPath, JSON.stringify({ id: "batch-001", status: "completed", started_at: started }));
    });
    expect(await fabricStatus(workspace, "batch-001", 2)).toMatchObject({ status: "completed" });
    expect(await fabricStatus(workspace, "child")).toMatchObject({ id: "child", status: "succeeded", model: "opus" });
  });
});


describe("preflight cancellation", () => {
  it.each(["dispatch", "batch"].flatMap((kind) => ["PREFLIGHT", "SETUP"].map((phase) => [kind, phase])))
  ("aborts %s %s without launching an owner", async (kind, phase) => {
    const controller = new AbortController();
    const pidPath = join(temporaryDirectory, "preflight.pid");
    const releasePath = join(temporaryDirectory, "preflight.release");
    const env = { ...ownerEnvironment, [`FIXTURE_${phase}_PID`]: pidPath, [`FIXTURE_${phase}_RELEASE`]: releasePath };
    const pending = (kind === "dispatch"
      ? dispatchConfiguredProvider({ adapter: "codex", prompt: "ordinary run", wait_seconds: 0 }, identity, controller.signal, env)
      : dispatchConfiguredBatch({ tasks: [{ adapter: "codex", prompt: "empty batch" }], wait_seconds: 0 }, identity, controller.signal, env))
      .then(() => "returned", () => "aborted");
    const pid = await waitForPid(pidPath);
    controller.abort();
    try {
      expect(await Promise.race([pending, delay(1000).then(() => "still pending")])).toBe("aborted");
      await waitFor(() => !alive(pid), "preflight child survived abort");
      expect(runDirectories()).toEqual([]);
    } finally {
      writeFileSync(releasePath, "release");
      await pending;
    }
  });
});


describe("infrastructure failures", () => {
  it.each(["dispatch", "batch"].flatMap((kind) => ["exit", "json", "setup", "missing owner"].map((failure) => [kind, failure])))
  ("classifies %s %s failures as preflight unavailable", async (kind, failure) => {
    const env = { ...ownerEnvironment, FIXTURE_PREFLIGHT_FAILURE: failure,
      ...(failure === "setup" ? { FIXTURE_SETUP_FAILURE: "1" } : {}) };
    if (failure === "missing owner") rmSync(join(product, "skills/orchestrate/scripts/dispatch_run.py"));
    const result = kind === "dispatch"
      ? await dispatchConfiguredProvider({ adapter: "codex", prompt: "ordinary run" }, identity, new AbortController().signal, env)
      : await dispatchConfiguredBatch({ tasks: [{ adapter: "codex", prompt: "empty batch" }] }, identity, new AbortController().signal, env);
    expect(result).toMatchObject({ status: "rejected", error: "preflight_unavailable", fix: expect.stringMatching(/Check/u) });
  });
});


describe("status CLI flags", () => {
  it.each([
    ["--wait-seconds", "10", "cli-task"],
    ["--json", "cli-task"],
    ["--json", "--wait-seconds", "10", "cli-task"],
    ["cli-task", "--json", "--wait-seconds", "10"],
  ])("finds the run with arguments %j", (...args) => {
    const dir = join(workspace, ".agent-run", "mcp-cli");
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, "dispatch-status.json"), JSON.stringify({ id: "cli-task", status: "failed" }));
    expect(JSON.parse(fabricCli(["status", ...args]))).toMatchObject({ id: "cli-task", status: "failed" });
    expect(existsSync(join(temporaryDirectory, "state"))).toBe(false);
  });
});

it('reads v1 attempts verbatim from the shared run root with ledger fields', async () => {
 const run = join(workspace,'.agent-run/runs/20260923-1012-dispatch-fixture-a81f3c');
 const attempt = join(run,'tasks/task-1/attempt-001');
 mkdirSync(attempt,{recursive:true});
 const row = JSON.parse(readFileSync(join(testDirectory,'fixtures/attempt.json'),'utf8'));
 row.cwd=workspace; row.worktree=workspace;
 writeFileSync(join(attempt,'attempt.json'),JSON.stringify(row));
 const {statusRows}=await import('../src/run-registry.js');
 const status = (await statusRows(workspace,['mcp-a81f3c'],0,'all',undefined,'full')).runs![0]!;
 expect(status).toMatchObject({schema:'fabric.status.v1',run_id:row.run_id,status:'ok',digest:row.digest,attempts:[row],worktree:workspace,dirty:null,ahead:null});
 expect(status.provenance).toEqual(row.provenance);
});

it('places logs and staging files inside a named run directory', async () => {
 const result = await dispatchConfiguredProvider({prompt:'fixture',wait_seconds:5},identity,new AbortController().signal,ownerEnvironment);
 const paths=result.paths as Record<string,string>;
 expect(paths.run_dir).toMatch(/\/\.agent-run\/runs\/\d{8}-\d{4}-dispatch-.*-[a-zA-Z0-9]{6}$/u);
 expect(paths.owner_stdout).toBe(join(paths.run_dir!,'_owner/stdout.jsonl'));
 expect(result.id).toMatch(/^mcp-.{6}$/u);
});

it.each([[[]], [['--interval','2']]])('watch prints a terminal state once and exits with %j', (options) => {
 const dir=join(workspace,'.agent-run/mcp-watch');mkdirSync(dir,{recursive:true});
 writeFileSync(join(dir,'dispatch-status.json'),JSON.stringify({id:'mcp-watch',status:'failed'}));
 expect(fabricCli(['watch','mcp-watch',...options])).toMatch(/^failed mcp-watch/mu);
});

it('keeps unpublished batch tasks visible and honours receipt interruption', async () => {
 const dir=join(workspace,'.agent-run/runs/20260923-1012-batch-fixture-a81f3c');
 const path=join(dir,'tasks/task-1/attempt-001');mkdirSync(path,{recursive:true});
 const row=JSON.parse(readFileSync(join(testDirectory,'fixtures/attempt.json'),'utf8'));
 writeFileSync(join(path,'attempt.json'),JSON.stringify(row));
 writeFileSync(join(dir,'dispatch-status.json'),JSON.stringify({id:row.run_id,batch_id:'batch-001',status:'running',task_ids:['task-1','task-2'],started_at:new Date().toISOString()}));
 const status=await fabricStatus(workspace,row.run_id);
 expect(status.runs).toHaveLength(2);expect(status.runs[1]).toMatchObject({task_id:'task-2',state:'queued'});
 row.state='running';row.status=null;writeFileSync(join(path,'attempt.json'),JSON.stringify(row));
 writeFileSync(join(dir,'RUN_RECEIPT.json'),JSON.stringify({status:'interrupted'}));
 const interrupted=await fabricStatus(workspace,row.run_id);
 expect(interrupted.runs.every((r:any)=>r.status === 'interrupted')).toBe(true);
});

it('bounds output slices, rejects escaped output, and waits for all requested tasks', async () => {
 const {statusRows,fabricOutput}=await import('../src/run-registry.js');
 const dir=join(workspace,'.agent-run/runs/20260923-1012-dispatch-fixture-b81f3c');
 const attempt=join(dir,'tasks/task-1/attempt-001');mkdirSync(attempt,{recursive:true});
 const row=JSON.parse(readFileSync(join(testDirectory,'fixtures/attempt.json'),'utf8'));
 row.run_id='mcp-b81f3c';row.paths.result='tasks/task-1/attempt-001/result.md';
 writeFileSync(join(attempt,'attempt.json'),JSON.stringify(row));writeFileSync(join(attempt,'result.md'),'abcdef');
 expect(await fabricOutput(workspace,{id:row.run_id,offset:2,max_bytes:2})).toMatchObject({digest:'cd',next_offset:4,eof:false});
 row.paths.result='../../outside';writeFileSync(join(attempt,'attempt.json'),JSON.stringify(row));
 expect(await fabricOutput(workspace,{id:row.run_id})).toMatchObject({status:'rejected',error:'output_unavailable'});
 row.state='running';row.status=null;writeFileSync(join(attempt,'attempt.json'),JSON.stringify(row));
 const timer=setTimeout(()=>{row.state='terminal';row.status='ok';writeFileSync(join(attempt,'attempt.json'),JSON.stringify(row));},200);
 try {const result=await statusRows(workspace,[row.run_id],1,'all');expect(result.runs?.[0]?.status).toBe('ok');}finally{clearTimeout(timer);}
});

it('selects the catalogue owner for a model-only request from another seat', async () => {
 mkdirSync(join(product,'config'));copyFileSync(join(repositoryRoot,'config/model-routing.json'),join(product,'config/model-routing.json'));
 const result=await dispatchConfiguredProvider({model:'gpt-6-luna',prompt:'fixture',wait_seconds:5},{...identity,provider:'claude'},new AbortController().signal,{...ownerEnvironment,AGENT_FABRIC_INSTANCE_ROOT:product});
 expect(result).toMatchObject({status:'succeeded',route:{adapter:'codex',resolved_model:'gpt-6-luna'}});
});

it('keeps non-Git cwd dispatches in the caller run root', async () => {
 const nested=join(workspace,'nested');mkdirSync(nested);
 const result=await dispatchConfiguredProvider({cwd:nested,prompt:'fixture',wait_seconds:5},identity,new AbortController().signal,ownerEnvironment);
 const path=(result.paths as Record<string,string>).run_dir!;
 expect(path).toContain(join(workspace,'.agent-run/runs').replace('/var/folders/','/private/var/folders/'));
 expect(await fabricStatus(workspace,String(result.id))).toMatchObject({status:'succeeded'});
});

it('terminalises a stopped v1 run before removing its owner record', async () => {
 const {terminateRecordedRun}=await import('../src/run-registry.js');
 const dir=join(workspace,'.agent-run/mcp-stopped');
 const path=join(dir,'tasks/task-1/attempt-001');mkdirSync(path,{recursive:true});
 const row=JSON.parse(readFileSync(join(testDirectory,'fixtures/attempt.json'),'utf8'));
 row.run_id='mcp-stopped';row.state='running';row.status=null;
 writeFileSync(join(path,'attempt.json'),JSON.stringify(row));
 await terminateRecordedRun({schema_version:1,kind:'dispatch',run_dir:dir,workspace,run_token:'gone',owner_pid:999991,owner_pgid:999991,owner_started_at:null,host_pid:999992,host_started_at:null,started_at:new Date().toISOString(),owner_stdout:'',owner_stderr:'',run_id:row.run_id,running:false,orphaned:false,provider:null});
 expect(await fabricStatus(workspace,row.run_id)).toMatchObject({state:'terminal',status:'interrupted'});
});

it('preserves a rejected resume and its fix without advising a terminal poll', async () => {
 const dir=join(workspace,'.agent-run/mcp-resume-rejected');
 const path=join(dir,'tasks/task-1/attempt-001');mkdirSync(path,{recursive:true});
 const row=JSON.parse(readFileSync(join(testDirectory,'fixtures/attempt.json'),'utf8'));
 row.run_id='mcp-resume-rejected';
 writeFileSync(join(path,'attempt.json'),JSON.stringify(row));
 writeFileSync(join(dir,'dispatch-status.json'),JSON.stringify({task_id:'task-1',next_attempt:2,status:'rejected',message:'dispatch a new run',fix:'dispatch a new run',finished_at:new Date().toISOString()}));
 const result=await fabricStatus(workspace,row.run_id);
 expect(result).toMatchObject({state:'terminal',status:'rejected',message:'dispatch a new run',fix:'dispatch a new run'});
 expect(result.digest).not.toContain('fabric_status');
});

it('paginates UTF-8 without replacement characters and reads bounded live tails', async () => {
 const {fabricOutput}=await import('../src/run-registry.js');
 const dir=join(workspace,'.agent-run/mcp-utf8');const attempt=join(dir,'tasks/task-1/attempt-001');mkdirSync(attempt,{recursive:true});
 const row=JSON.parse(readFileSync(join(testDirectory,'fixtures/attempt.json'),'utf8'));
 row.run_id='mcp-utf8';row.paths.result='tasks/task-1/attempt-001/result.md';row.state='running';row.status=null;
 writeFileSync(join(attempt,'attempt.json'),JSON.stringify(row));
 const original='a😀é中z';writeFileSync(join(attempt,'result.md'),original);
 let text='',offset=0;
 for(let i=0;i<10;i++) {
  const page=await fabricOutput(workspace,{id:row.run_id,offset,max_bytes:3});
  if(page.error) { expect(page.error).toBe('output_bounds'); break; }
  text+=page.digest;offset=Number(page.next_offset);if(page.eof) break;
 }
 // A one-codepoint page must fit; normal pages preserve every byte across boundaries.
 text='';offset=0;
 for(let i=0;i<10;i++) {
  const page=await fabricOutput(workspace,{id:row.run_id,offset,max_bytes:4});
  text+=page.digest;offset=Number(page.next_offset);if(page.eof) break;
 }
 expect(text).toBe(original);
 const tail=await fabricOutput(workspace,{id:row.run_id,tail:true,max_bytes:5});
 expect(tail).toMatchObject({digest:'中z',eof:true,next_offset:Buffer.byteLength(original)});
});

it('retains custody when a live owner has no verifiable start identity', async () => {
 const {terminateRecordedRun}=await import('../src/run-registry.js');
 const child=spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{detached:true,stdio:'ignore'});
 spawnedPids.push(child.pid!);
 const dir=join(workspace,'.agent-run/mcp-unverified');mkdirSync(dir,{recursive:true});
 const record={schema_version:1 as const,kind:'dispatch' as const,run_dir:dir,workspace,run_token:'unknown',owner_pid:child.pid!,owner_pgid:child.pid!,owner_started_at:null,host_pid:process.pid,host_started_at:null,started_at:new Date().toISOString(),owner_stdout:'',owner_stderr:''};
 writeFileSync(join(dir,OWNER_RECORD_NAME),JSON.stringify(record));
 const result=await terminateRecordedRun({...record,run_id:'mcp-unverified',running:true,orphaned:false,provider:null},0);
 expect(result).toMatchObject({signalled:false,reason:'still running'});
 expect(existsSync(join(dir,OWNER_RECORD_NAME))).toBe(true);
});

it('binds termination writes to the discovered run directory', async () => {
 const {findRecordedRun,terminateRecordedRun}=await import('../src/run-registry.js');
 const dir=join(workspace,'.agent-run/mcp-forged'),outside=join(workspace,'outside');
 mkdirSync(dir,{recursive:true});mkdirSync(outside);
 const target=join(outside,'dispatch-status.json');writeFileSync(target,'untouched');
 writeFileSync(join(dir,OWNER_RECORD_NAME),JSON.stringify({schema_version:1,kind:'dispatch',run_dir:outside,workspace,run_token:'gone',owner_pid:999991,owner_pgid:999991,owner_started_at:null,host_pid:999992,host_started_at:null,started_at:new Date().toISOString(),owner_stdout:'',owner_stderr:''}));
 const record=findRecordedRun(workspace,'mcp-forged')!;
 await terminateRecordedRun(record);
 expect(readFileSync(target,'utf8')).toBe('untouched');
 expect(JSON.parse(readFileSync(join(dir,'dispatch-status.json'),'utf8')).status).toBe('interrupted');
});
