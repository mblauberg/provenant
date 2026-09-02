import { execFileSync } from "node:child_process";
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
import { DEFAULT_RETENTION_HOURS, OWNER_RECORD_NAME, retentionHours } from "../src/run-registry.js";
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
  const root = join(workspace, ".agent-run");
  return existsSync(root) ? readdirSync(root).filter((name) => name.startsWith("mcp-")).sort() : [];
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

describe("run directory pruning", () => {
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
    expect(done.status).toBe("succeeded");
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
    await ordinaryDispatch();
    expect(existsSync(fresh.runDir)).toBe(true);
    expect(existsSync(fresh.stdout)).toBe(true);
  });

  it("takes the retention from configuration", async () => {
    const aged = ageRun("mcp-aged02", 3);
    await ordinaryDispatch({ ...ownerEnvironment, AGENT_FABRIC_RUN_RETENTION_HOURS: "2" });
    expect(existsSync(aged.runDir), "a configured two-hour retention did not apply").toBe(false);
  });

  it("leaves everything that is not an mcp- run directory alone", async () => {
    const runRoot = join(workspace, ".agent-run");
    mkdirSync(runRoot, { recursive: true });
    const foreign = join(runRoot, "20200101T000000Z");
    mkdirSync(foreign, { recursive: true });
    const when = new Date(Date.now() - 24 * 3_600_000 * 400);
    utimesSync(foreign, when, when);
    await ordinaryDispatch();
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
