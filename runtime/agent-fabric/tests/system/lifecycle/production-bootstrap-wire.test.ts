import { statSync, utimesSync } from "node:fs";
import { chmod, lstat, mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

import { afterEach, describe, expect, it, vi } from "vitest";
import Database from "better-sqlite3";
import type { Sha256Digest } from "@local/agent-fabric-protocol";

import {
  connectFabricDaemon,
  deriveTrustedGitExecutionProfileDigest,
} from "../../../src/index.ts";
import {
  preflightFabricDaemonStart,
  startFabricDaemon,
  type FabricDaemonHandle,
} from "../../../src/daemon/client.ts";
import { openFabricDatabase } from "../../../src/persistence/sqlite.ts";
import { digestCanonical } from "../../../src/review/canonical/index.ts";
import {
  settle,
  waitForFile,
  waitForProcessExit,
  waitUntil,
  type WaitOptions,
} from "../../shared/deadline-wait.ts";
import { MCP_ROOT_AUTHORITY } from "../../support/mcp-testkit.ts";
import { createCurrentSessionRun } from "../../support/current-session-testkit.ts";

const handles: FabricDaemonHandle[] = [];
const roots: string[] = [];

type StartOptions = Parameters<typeof startFabricDaemon>[0];

async function provisionLifecycleReceiptAuthority(
  stateDirectory: string,
  authorityId = "production-local-authority",
): Promise<void> {
  await mkdir(stateDirectory, { recursive: true, mode: 0o700 });
  await chmod(stateDirectory, 0o700);
  const databasePath = join(stateDirectory, "lifecycle-receipts.sqlite3");
  const database = new Database(databasePath);
  database.exec(await readFile(
    new URL("../../../schemas/lifecycle-receipt-authority-v1.sql", import.meta.url),
    "utf8",
  ));
  database.prepare("INSERT INTO authority_metadata VALUES(1,1,?)").run(authorityId);
  database.close();
  await chmod(databasePath, 0o600);
  await writeFile(
    join(stateDirectory, "lifecycle-receipts.hmac.key"),
    randomBytes(32),
    { mode: 0o600 },
  );
}

async function launchAndReleaseFromSeparateProcess(
  options: StartOptions,
  release = true,
): Promise<{ pid: number; launcherPid: number }> {
  const clientPath = fileURLToPath(new URL("../../../src/daemon/client.ts", import.meta.url));
  const script = `
    import { startFabricDaemon } from ${JSON.stringify(clientPath)};
    const handle = await startFabricDaemon(JSON.parse(process.argv[1]));
    await new Promise((resolve) => process.stdout.write(JSON.stringify({ pid: handle.pid }) + "\\n", resolve));
    if (${JSON.stringify(release)}) handle.release();
    else process.exit(0);
  `;
  const launcher = spawn(
    process.execPath,
    ["--import", "tsx", "--input-type=module", "-e", script, JSON.stringify(options)],
    {
      cwd: fileURLToPath(new URL("../../..", import.meta.url)),
      detached: true,
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  if (launcher.stdout === null || launcher.stderr === null) {
    throw new Error("released daemon launcher pipes are unavailable");
  }
  if (launcher.pid === undefined) throw new Error("released daemon launcher pid is unavailable");
  const launcherPid = launcher.pid;
  let stderr = "";
  launcher.stderr.setEncoding("utf8");
  launcher.stderr.on("data", (chunk: string) => { stderr += chunk; });
  const line = await new Promise<string>((resolvePromise, reject) => {
    launcher.stdout?.once("data", (chunk: Buffer) => resolvePromise(chunk.toString("utf8")));
    launcher.once("exit", (code) => {
      if (code !== 0) reject(new Error(`released daemon launcher failed ${String(code)}: ${stderr}`));
    });
  });
  await new Promise<void>((resolvePromise, reject) => {
    if (launcher.exitCode !== null) {
      launcher.exitCode === 0
        ? resolvePromise()
        : reject(new Error(`released daemon launcher failed ${String(launcher.exitCode)}: ${stderr}`));
      return;
    }
    launcher.once("exit", (code) => code === 0
      ? resolvePromise()
      : reject(new Error(`released daemon launcher failed ${String(code)}: ${stderr}`)));
  });
  const result: unknown = JSON.parse(line);
  if (
    typeof result !== "object" ||
    result === null ||
    !("pid" in result) ||
    typeof result.pid !== "number"
  ) throw new Error("released daemon launcher result is invalid");
  return { pid: result.pid, launcherPid };
}

const WIRE_WAIT: WaitOptions & Readonly<{ timeoutMs: number }> = {
  timeoutMs: 10_000,
  pollIntervalMs: 20,
};

async function waitForOwnerState(
  runtimeDirectory: string,
  state: "stopped" | "crashed",
): Promise<Record<string, unknown>> {
  const path = join(runtimeDirectory, "fabric-v1.discovery-owner.json");
  return await waitUntil(async () => {
    try {
      const value = JSON.parse(await readFile(path, "utf8")) as Record<string, unknown>;
      return value.state === state ? value : undefined;
    } catch {
      return undefined; // not durable yet
    }
  }, WIRE_WAIT.timeoutMs, `Discovery owner ${state}`, WIRE_WAIT);
}

async function startBusyWalWriter(
  databasePath: string,
  waitForPath?: string,
): Promise<ReturnType<typeof spawn>> {
  const script = `
    import Database from "better-sqlite3";
    import { existsSync } from "node:fs";
    const database = new Database(process.argv[1]);
    const waitForPath = process.argv[2];
    database.pragma("journal_mode = WAL");
    database.pragma("wal_autocheckpoint = 0");
    database.pragma("busy_timeout = 5000");
    const heartbeat = database.prepare(\`
      UPDATE daemon_runtime_epochs
      SET heartbeat_at = heartbeat_at + 1
      WHERE state = 'running'
    \`);
    let stopping = false;
    const stop = () => {
      if (stopping) return;
      stopping = true;
      database.close();
      process.exit(0);
    };
    process.once("SIGTERM", stop);
    process.once("SIGINT", stop);
    process.stdout.write("ready\\n");
    const write = () => {
      if (stopping) return;
      heartbeat.run();
      // Keep the source changing for the bounded inspection retry window
      // without allowing the WAL sidecar to grow until SQLite rejects a
      // partial snapshot before the retry-bound assertion can run.
      setTimeout(write, 1);
    };
    const waitThenWrite = () => {
      if (stopping) return;
      if (waitForPath === undefined || existsSync(waitForPath)) write();
      else setTimeout(waitThenWrite, 1);
    };
    waitThenWrite();
  `;
  const writer = spawn(
    process.execPath,
    [
      "--input-type=module",
      "-e",
      script,
      databasePath,
      ...(waitForPath === undefined ? [] : [waitForPath]),
    ],
    {
      cwd: fileURLToPath(new URL("../../..", import.meta.url)),
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  if (writer.stdout === null || writer.stderr === null) {
    throw new Error("busy WAL writer pipes are unavailable");
  }
  let stderr = "";
  writer.stderr.setEncoding("utf8");
  writer.stderr.on("data", (chunk: string) => { stderr += chunk; });
  const lines = createInterface({ input: writer.stdout, crlfDelay: Infinity });
  await new Promise<void>((resolvePromise, reject) => {
    const timeout = setTimeout(() => reject(new Error("busy WAL writer did not become ready")), 20_000);
    lines.once("line", (line) => {
      clearTimeout(timeout);
      if (line === "ready") resolvePromise();
      else reject(new Error(`busy WAL writer returned an invalid readiness line: ${line}`));
    });
    writer.once("exit", (code) => {
      clearTimeout(timeout);
      reject(new Error(`busy WAL writer exited before readiness: ${String(code)} ${stderr}`));
    });
    writer.once("error", reject);
  });
  lines.close();
  return writer;
}

afterEach(async () => {
  vi.unstubAllEnvs();
  await Promise.allSettled(handles.splice(0).reverse().map(async (handle) => handle.stop()));
  await Promise.allSettled(roots.splice(0).map(async (root) => rm(root, { recursive: true, force: true })));
});

describe("production daemon bootstrap wiring", () => {
  it("accepts the authoritative review catalogue at production startup preflight", async () => {
    await expect(preflightFabricDaemonStart()).resolves.toBeUndefined();
  });

  it("verifies the code-adjacent review catalogue instead of a deployment-home copy", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-review-catalogue-"));
    roots.push(root);
    const agentsHome = join(root, "agents-home");
    const profilePath = join(agentsHome, "config/review-profiles/certifying-review-four-slot-v1.json");
    const schemaPath = join(agentsHome, "runtime/agent-fabric/schemas/review-profile.v1.schema.json");
    await Promise.all([
      mkdir(join(agentsHome, "config/review-profiles"), { recursive: true }),
      mkdir(join(agentsHome, "runtime/agent-fabric/schemas"), { recursive: true }),
    ]);
    const authoritativeProfile = JSON.parse(await readFile(
      new URL("../../../../../config/review-profiles/certifying-review-four-slot-v1.json", import.meta.url),
      "utf8",
    )) as { profileId: string };
    const profile = structuredClone(authoritativeProfile);
    profile.profileId = "silently-rewritten-profile";
    await Promise.all([
      writeFile(profilePath, `${JSON.stringify(profile)}\n`, "utf8"),
      writeFile(
        join(
          agentsHome,
          "config/review-profiles/certifying-review-four-slot-v1.deployment-digest.json",
        ),
        `${JSON.stringify({
          schemaVersion: 1,
          profile: "config/review-profiles/certifying-review-four-slot-v1.json",
          digest: digestCanonical(authoritativeProfile),
        })}\n`,
        "utf8",
      ),
      writeFile(schemaPath, await readFile(
        new URL("../../../schemas/review-profile.v1.schema.json", import.meta.url),
        "utf8",
      ), "utf8"),
    ]);
    const configPath = join(root, "agent-fabric.json");
    await writeFile(configPath, `${JSON.stringify({
      schemaVersion: 1,
      allowedAdapters: [],
      activeAdapters: [],
      allowedProfiles: ["headless"],
      workspaceRoots: [root],
    })}\n`, "utf8");

    await expect(preflightFabricDaemonStart({
      configuration: {
        globalConfigPath: configPath,
        compatibilityPath: fileURLToPath(new URL("../../../../../config/adapter-compatibility.yaml", import.meta.url)),
        compatibilitySchemaPath: fileURLToPath(new URL("../../../schemas/adapter-compatibility.schema.json", import.meta.url)),
        agentsHome,
      },
    })).resolves.toBeUndefined();
  });

  it("opens and owns the configured local lifecycle receipt authority", async () => {
    const root = await mkdtemp(join(tmpdir(), "f-pra-"));
    roots.push(root);
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(root, "runtime");
    await provisionLifecycleReceiptAuthority(stateDirectory);
    const options = {
      databasePath: join(stateDirectory, "fabric.sqlite3"),
      stateDirectory,
      runtimeDirectory,
      socketPath: join(runtimeDirectory, "fabric.sock"),
      workspaceRoots: [root],
      lifecycleReceiptAuthorityId: "production-local-authority",
    };

    const first = await startFabricDaemon(options);
    handles.push(first);
    await first.stop();
    const restarted = await startFabricDaemon(options);
    handles.push(restarted);
    expect(restarted.pid).not.toBe(first.pid);
  });

  it("fails closed when a configured local lifecycle receipt authority is absent or crossed", async () => {
    const absentRoot = await mkdtemp(join(tmpdir(), "f-pra-a-"));
    const crossedRoot = await mkdtemp(join(tmpdir(), "f-pra-x-"));
    roots.push(absentRoot, crossedRoot);

    await expect(startFabricDaemon({
      databasePath: join(absentRoot, "state", "fabric.sqlite3"),
      stateDirectory: join(absentRoot, "state"),
      runtimeDirectory: join(absentRoot, "runtime"),
      socketPath: join(absentRoot, "runtime", "fabric.sock"),
      workspaceRoots: [absentRoot],
      lifecycleReceiptAuthorityId: "production-local-authority",
    })).rejects.toThrow();

    const crossedState = join(crossedRoot, "state");
    await provisionLifecycleReceiptAuthority(crossedState, "ledger-authority");
    await expect(startFabricDaemon({
      databasePath: join(crossedState, "fabric.sqlite3"),
      stateDirectory: crossedState,
      runtimeDirectory: join(crossedRoot, "runtime"),
      socketPath: join(crossedRoot, "runtime", "fabric.sock"),
      workspaceRoots: [crossedRoot],
      lifecycleReceiptAuthorityId: "configured-authority",
    })).rejects.toThrow(/identity mismatch/u);
  });

  it("does not attach a configured caller to an incumbent with a different receipt authority", async () => {
    const root = await mkdtemp(join(tmpdir(), "f-pra-m-"));
    roots.push(root);
    const options = {
      databasePath: join(root, "state", "fabric.sqlite3"),
      stateDirectory: join(root, "state"),
      runtimeDirectory: join(root, "runtime"),
      socketPath: join(root, "runtime", "fabric.sock"),
      workspaceRoots: [root],
    };
    const incumbent = await startFabricDaemon(options);
    handles.push(incumbent);

    await expect(startFabricDaemon({
      ...options,
      lifecycleReceiptAuthorityId: "requested-authority",
    })).rejects.toThrow(/lifecycle receipt authority configuration mismatch/u);
  });

  it("rejects process startup when the authoritative daemon epoch is absent", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-missing-daemon-epoch-"));
    roots.push(root);
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(root, "runtime");
    await Promise.all([
      mkdir(stateDirectory, { mode: 0o700 }),
      mkdir(runtimeDirectory, { mode: 0o700 }),
    ]);
    const processPath = fileURLToPath(new URL("../../../src/daemon/process.ts", import.meta.url));
    const child = spawn(process.execPath, ["--import", "tsx", processPath], {
      cwd: fileURLToPath(new URL("../../..", import.meta.url)),
      env: {
        PATH: process.env.PATH ?? "/usr/bin:/bin",
        HOME: process.env.HOME ?? root,
        AGENT_FABRIC_DATABASE_PATH: join(stateDirectory, "fabric.sqlite3"),
        AGENT_FABRIC_SOCKET_PATH: join(runtimeDirectory, "fabric.sock"),
        AGENT_FABRIC_STATE_DIRECTORY: stateDirectory,
        AGENT_FABRIC_RUNTIME_DIRECTORY: runtimeDirectory,
        AGENT_FABRIC_BOOTSTRAP_CAPABILITY: `afb_${"a".repeat(43)}`,
        AGENT_FABRIC_BOOTSTRAP_MODE: "production-election",
        AGENT_FABRIC_BOOTSTRAP_ACTION_ID: "bootstrap_missing_epoch_01",
        AGENT_FABRIC_BOOTSTRAP_ELECTION_GENERATION: "1",
        AGENT_FABRIC_CAPABILITY_KEY: "b".repeat(43),
        AGENT_FABRIC_EXECUTION_PROFILE: "headless",
        AGENT_FABRIC_MAXIMUM_CONCURRENT_PROVIDER_TURNS: "1",
        AGENT_FABRIC_WORKSPACE_ROOTS_JSON: JSON.stringify([root]),
        AGENT_FABRIC_ADAPTERS_JSON: "{}",
      },
      stdio: ["ignore", "pipe", "pipe"],
    });
    if (child.stdout === null || child.stderr === null) throw new Error("daemon epoch child pipes are unavailable");
    let stderr = "";
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk: string) => { stderr += chunk; });
    const outcome = await new Promise<{ kind: "output"; line: string } | { kind: "closed"; code: number | null }>((resolvePromise, reject) => {
      const timeout = setTimeout(() => reject(new Error("daemon epoch child did not terminate")), 20_000);
      child.stdout?.once("data", (chunk: Buffer) => {
        clearTimeout(timeout);
        resolvePromise({ kind: "output", line: chunk.toString("utf8").trim() });
      });
      child.once("close", (code) => {
        clearTimeout(timeout);
        resolvePromise({ kind: "closed", code });
      });
      child.once("error", reject);
    });
    if (outcome.kind === "output") {
      child.kill("SIGTERM");
      await new Promise<void>((resolvePromise) => child.once("close", () => resolvePromise()));
    }

    expect(outcome).toEqual({ kind: "closed", code: 1 });
    expect(stderr).toContain("agent fabric daemon environment is incomplete");
  });

  it("rejects a pre-cutover database before creating daemon runtime state", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-production-cutover-"));
    roots.push(root);
    const databasePath = join(root, "legacy.sqlite3");
    const legacy = new Database(databasePath);
    legacy.exec(`
      CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);
      INSERT INTO schema_migrations(version, applied_at) VALUES (14, '2026-07-12T00:00:00Z');
      CREATE TABLE legacy_sentinel(value TEXT NOT NULL);
      INSERT INTO legacy_sentinel(value) VALUES ('preserve-me');
    `);
    legacy.close();
    const beforeBytes = await readFile(databasePath);
    const beforeStat = await stat(databasePath);
    const beforeEntries = await readdir(root);

    await expect(startFabricDaemon({
      databasePath,
      stateDirectory: join(root, "state"),
      runtimeDirectory: join(root, "runtime"),
      socketPath: join(root, "runtime", "fabric.sock"),
      workspaceRoots: [root],
    })).rejects.toMatchObject({ code: "SCHEMA_CUTOVER_REQUIRED", preserved: true });

    expect(await readFile(databasePath)).toEqual(beforeBytes);
    expect((await stat(databasePath)).mode).toBe(beforeStat.mode);
    expect((await stat(databasePath)).mtimeMs).toBe(beforeStat.mtimeMs);
    expect(await readdir(root)).toEqual(beforeEntries);
  });

  it("preserves a database published after preflight and rolls back every failed-bootstrap artifact", async () => {
    const root = await mkdtemp(join(tmpdir(), "afb-race-"));
    const fixtureRoot = await mkdtemp(join(tmpdir(), "afb-old-"));
    roots.push(root, fixtureRoot);
    const fixturePath = join(fixtureRoot, "legacy.sqlite3");
    const legacy = new Database(fixturePath);
    legacy.exec(`
      CREATE TABLE schema_migrations(version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL);
      INSERT INTO schema_migrations(version, applied_at) VALUES (14, '2026-07-12T00:00:00Z');
      CREATE TABLE legacy_sentinel(value TEXT NOT NULL);
      INSERT INTO legacy_sentinel(value) VALUES ('preserve-race-winner');
    `);
    legacy.close();
    const fixtureBytes = await readFile(fixturePath);
    const databasePath = join(root, "fabric.sqlite3");
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(root, "runtime");
    const previousFixture = process.env.AGENT_FABRIC_TEST_CUTOVER_RACE_FIXTURE_PATH;
    process.env.AGENT_FABRIC_TEST_CUTOVER_RACE_FIXTURE_PATH = fixturePath;
    let outcome: unknown;
    try {
      outcome = await startFabricDaemon({
        databasePath,
        stateDirectory,
        runtimeDirectory,
        socketPath: join(runtimeDirectory, "fabric.sock"),
        workspaceRoots: [root],
      }).then((handle) => {
        handles.push(handle);
        return handle;
      }, (error: unknown) => error);
    } finally {
      if (previousFixture === undefined) delete process.env.AGENT_FABRIC_TEST_CUTOVER_RACE_FIXTURE_PATH;
      else process.env.AGENT_FABRIC_TEST_CUTOVER_RACE_FIXTURE_PATH = previousFixture;
    }

    expect(outcome).toMatchObject({
      code: "SCHEMA_CUTOVER_REQUIRED",
      preserved: true,
      message: "database does not contain the current schema epoch; existing database preserved",
    });
    expect(await readFile(databasePath)).toEqual(fixtureBytes);
    expect(await readdir(root)).toEqual(["fabric.sqlite3"]);
  });

  it("reports a typed child cutover failure without removing an existing socket path", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-process-cutover-"));
    roots.push(root);
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(root, "runtime");
    const databasePath = join(stateDirectory, "legacy.sqlite3");
    const socketPath = join(runtimeDirectory, "fabric.sock");
    await Promise.all([
      mkdir(stateDirectory, { mode: 0o700 }),
      mkdir(runtimeDirectory, { mode: 0o700 }),
    ]);
    const legacy = new Database(databasePath);
    legacy.exec("CREATE TABLE legacy_sentinel(value TEXT NOT NULL); INSERT INTO legacy_sentinel VALUES ('preserve-me')");
    legacy.close();
    await writeFile(socketPath, "preserve-socket\n", { mode: 0o600 });
    const beforeBytes = await readFile(databasePath);
    const beforeStat = await stat(databasePath);
    const beforeRuntimeEntries = await readdir(runtimeDirectory);
    const processPath = fileURLToPath(new URL("../../../src/daemon/process.ts", import.meta.url));
    const child = spawn(process.execPath, ["--import", "tsx", processPath], {
      cwd: fileURLToPath(new URL("../../..", import.meta.url)),
      env: {
        PATH: process.env.PATH ?? "/usr/bin:/bin",
        HOME: process.env.HOME ?? root,
        AGENT_FABRIC_PRODUCT_ROOT: root,
        AGENT_FABRIC_DATABASE_PATH: databasePath,
        AGENT_FABRIC_SOCKET_PATH: socketPath,
        AGENT_FABRIC_STATE_DIRECTORY: stateDirectory,
        AGENT_FABRIC_RUNTIME_DIRECTORY: runtimeDirectory,
        AGENT_FABRIC_BOOTSTRAP_CAPABILITY: `afb_${"a".repeat(43)}`,
        AGENT_FABRIC_BOOTSTRAP_MODE: "production-election",
        AGENT_FABRIC_BOOTSTRAP_ACTION_ID: "bootstrap_cutover_child_01",
        AGENT_FABRIC_BOOTSTRAP_ELECTION_GENERATION: "1",
        AGENT_FABRIC_BOOTSTRAP_CUSTODY: "parent-pipe-v1",
        AGENT_FABRIC_DAEMON_INSTANCE_GENERATION: "1",
        AGENT_FABRIC_CAPABILITY_KEY: "b".repeat(43),
        AGENT_FABRIC_EXECUTION_PROFILE: "headless",
        AGENT_FABRIC_MAXIMUM_CONCURRENT_PROVIDER_TURNS: "1",
        AGENT_FABRIC_WORKSPACE_ROOTS_JSON: JSON.stringify([root]),
        AGENT_FABRIC_ADAPTERS_JSON: "{}",
      },
      stdio: ["pipe", "pipe", "pipe"],
    });
    if (child.stdout === null) throw new Error("daemon cutover child stdout is unavailable");
    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    const firstLine = await new Promise<string>((resolvePromise, reject) => {
      child.once("error", reject);
      lines.once("line", resolvePromise);
    });
    expect(JSON.parse(firstLine)).toEqual({
      ready: false,
      error: {
        code: "SCHEMA_CUTOVER_REQUIRED",
        message: "database does not contain the current schema epoch; existing database preserved",
        preserved: true,
      },
    });
    await expect(new Promise<number | null>((resolvePromise) => child.once("exit", resolvePromise))).resolves.toBe(1);
    expect(await readFile(databasePath)).toEqual(beforeBytes);
    expect((await stat(databasePath)).mode).toBe(beforeStat.mode);
    expect((await stat(databasePath)).mtimeMs).toBe(beforeStat.mtimeMs);
    expect(await readFile(socketPath, "utf8")).toBe("preserve-socket\n");
    expect(await readdir(runtimeDirectory)).toEqual(beforeRuntimeEntries);
  });

  it("accepts a production election proof without placeholder process-lock paths", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-production-process-proof-"));
    roots.push(root);
    const stateDirectory = join(root, "s");
    const runtimeDirectory = join(root, "r");
    await Promise.all([
      mkdir(stateDirectory, { recursive: true, mode: 0o700 }),
      mkdir(runtimeDirectory, { recursive: true, mode: 0o700 }),
    ]);
    const processPath = fileURLToPath(new URL("../../../src/daemon/process.ts", import.meta.url));
    const child = spawn(process.execPath, ["--import", "tsx", processPath], {
      cwd: fileURLToPath(new URL("../../..", import.meta.url)),
      env: {
        PATH: process.env.PATH ?? "/usr/bin:/bin",
        HOME: process.env.HOME ?? root,
        AGENT_FABRIC_PRODUCT_ROOT: root,
        AGENT_FABRIC_DATABASE_PATH: join(stateDirectory, "fabric.sqlite3"),
        AGENT_FABRIC_SOCKET_PATH: join(runtimeDirectory, "f.sock"),
        AGENT_FABRIC_STATE_DIRECTORY: stateDirectory,
        AGENT_FABRIC_RUNTIME_DIRECTORY: runtimeDirectory,
        AGENT_FABRIC_BOOTSTRAP_CAPABILITY: `afb_${"a".repeat(43)}`,
        AGENT_FABRIC_BOOTSTRAP_MODE: "production-election",
        AGENT_FABRIC_BOOTSTRAP_ACTION_ID: "bootstrap_process_proof_01",
        AGENT_FABRIC_BOOTSTRAP_ELECTION_GENERATION: "1",
        AGENT_FABRIC_BOOTSTRAP_CUSTODY: "parent-pipe-v1",
        AGENT_FABRIC_DAEMON_INSTANCE_GENERATION: "1",
        AGENT_FABRIC_CAPABILITY_KEY: "b".repeat(43),
        AGENT_FABRIC_EXECUTION_PROFILE: "headless",
        AGENT_FABRIC_MAXIMUM_CONCURRENT_PROVIDER_TURNS: "1",
        AGENT_FABRIC_WORKSPACE_ROOTS_JSON: JSON.stringify([root]),
        AGENT_FABRIC_ADAPTERS_JSON: "{}",
      },
      stdio: ["pipe", "pipe", "pipe"],
    });
    if (child.stdout === null || child.stderr === null) throw new Error("daemon proof child pipes are unavailable");
    let childStderr = "";
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk: string) => { childStderr += chunk; });
    const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
    const firstLine = await new Promise<string>((resolvePromise, reject) => {
      const timeout = setTimeout(() => reject(new Error("daemon proof child did not report readiness")), 20_000);
      lines.once("line", (line) => {
        clearTimeout(timeout);
        resolvePromise(line);
      });
      child.once("exit", (code) => {
        clearTimeout(timeout);
        reject(new Error(`daemon proof child exited before readiness: ${String(code)} ${childStderr}`));
      });
    });
    expect(JSON.parse(firstLine)).toEqual({ ready: true });
    child.kill("SIGTERM");
    const exitCode = await new Promise<number | null>((resolvePromise) => {
      child.once("exit", (code) => resolvePromise(code));
    });
    // This low-level fixture proves the process accepts a production election
    // proof, but deliberately has no published discovery owner to terminalise.
    // Teardown must therefore fail closed after readiness instead of claiming a
    // clean production stop.
    expect(exitCode).toBe(1);
    expect(childStderr).toContain("daemon shutdown failed during mark-terminal");
  });

  it("materialises trusted Git profiles through the production daemon composition boundary", async () => {
    const root = await mkdtemp(join(tmpdir(), "ftg-"));
    roots.push(root);
    const binaryDigest = `sha256:${createHash("sha256").update(await readFile("/usr/bin/git")).digest("hex")}` as Sha256Digest;
    const profileBase = {
      profileId: "production-sealed-git",
      revision: 1,
      gitBinaryPath: "/usr/bin/git",
      gitBinaryVersion: "system-git",
      gitBinaryDigest: binaryDigest,
      objectFormat: "sha1" as const,
      mergeBackendId: "disabled",
      rebaseBackendId: "disabled",
      environmentDigest: `sha256:${"a".repeat(64)}` as Sha256Digest,
      helperRegistryDigest: `sha256:${"b".repeat(64)}` as Sha256Digest,
      inspectorDigest: `sha256:${"c".repeat(64)}` as Sha256Digest,
    };
    const options = {
      databasePath: join(root, "s", "f.sqlite3"),
      stateDirectory: join(root, "s"),
      runtimeDirectory: join(root, "r"),
      socketPath: join(root, "r", "f.sock"),
      workspaceRoots: [root],
      trustedGitConfiguration: {
        executionProfiles: [{
          ...profileBase,
          profileDigest: deriveTrustedGitExecutionProfileDigest(profileBase),
        }],
      },
    };

    const daemon = await startFabricDaemon(options);
    handles.push(daemon);
    const database = new Database(options.databasePath, { readonly: true, fileMustExist: true });
    try {
      expect(database.prepare(`
        SELECT profile_id,revision,state FROM git_execution_profiles
         WHERE profile_id='production-sealed-git'
      `).get()).toEqual({ profile_id: "production-sealed-git", revision: 1, state: "active" });
    } finally {
      database.close();
    }
  });

  it("handshakes first and coalesces repeated starts through one flock election without lock databases", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-production-bootstrap-"));
    roots.push(root);
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(root, "runtime");
    const options = {
      databasePath: join(stateDirectory, "fabric.sqlite3"),
      stateDirectory,
      runtimeDirectory,
      socketPath: join(runtimeDirectory, "fabric.sock"),
      workspaceRoots: [root],
    };

    const first = await startFabricDaemon(options);
    handles.push(first);
    const second = await startFabricDaemon(options);
    handles.push(second);

    expect(second.pid).toBe(first.pid);
    const database = new Database(options.databasePath, { readonly: true, fileMustExist: true });
    try {
      expect(database.prepare(`
        SELECT instance_generation, state FROM daemon_runtime_epochs
        ORDER BY instance_generation DESC LIMIT 1
      `).get()).toEqual({ instance_generation: 1, state: "running" });
    } finally {
      database.close();
    }
    expect([
      ...await readdir(stateDirectory),
      ...await readdir(runtimeDirectory),
    ].filter((name) => name.endsWith(".lock.sqlite3"))).toEqual([]);
  });

  it("attaches to the elected daemon while its WAL is busy without inventing a schema cutover", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-production-busy-wal-"));
    roots.push(root);
    const options = {
      databasePath: join(root, "state", "fabric.sqlite3"),
      stateDirectory: join(root, "state"),
      runtimeDirectory: join(root, "runtime"),
      socketPath: join(root, "runtime", "fabric.sock"),
      workspaceRoots: [root],
    };
    const owner = await startFabricDaemon(options);
    handles.push(owner);
    const writer = await startBusyWalWriter(options.databasePath);
    try {
      const outcomes = await Promise.allSettled(
        Array.from({ length: 48 }, async () => await startFabricDaemon(options)),
      );
      const rejectedCodes = outcomes.flatMap((outcome) => outcome.status === "rejected"
        ? [typeof outcome.reason === "object" && outcome.reason !== null && "code" in outcome.reason
          ? outcome.reason.code
          : undefined]
        : []);
      expect(rejectedCodes).not.toContain("SCHEMA_CUTOVER_REQUIRED");
      expect(outcomes.every((outcome) => outcome.status === "fulfilled")).toBe(true);
      const attached = outcomes.flatMap((outcome) => outcome.status === "fulfilled" ? [outcome.value] : []);
      handles.push(...attached);
      expect(new Set(attached.map((handle) => handle.pid))).toEqual(new Set([owner.pid]));
      expect(attached.every((handle) => !handle.ownsProcess)).toBe(true);
    } finally {
      if (writer.exitCode === null && writer.signalCode === null) {
        writer.kill("SIGTERM");
        await new Promise<void>((resolvePromise) => writer.once("exit", () => resolvePromise()));
      }
    }
  });

  it("does not run cutover cleanup when an injected source change exhausts the retry bound", async () => {
    vi.stubEnv("AGENT_FABRIC_TEST_DATABASE_INSPECTION_ATTEMPTS", "1");
    const root = await mkdtemp(join(tmpdir(), "fabric-production-inspection-race-"));
    roots.push(root);
    const databaseDirectory = join(root, "database");
    const databasePath = join(databaseDirectory, "fabric.sqlite3");
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(root, "runtime");
    await mkdir(databaseDirectory, { mode: 0o700 });
    await mkdir(stateDirectory, { mode: 0o700 });
    await mkdir(runtimeDirectory, { mode: 0o700 });
    const seed = openFabricDatabase(databasePath);
    seed.prepare(`
      INSERT INTO daemon_runtime_epochs(
        instance_generation,instance_id,state,observed_global_revision,started_at,heartbeat_at,stopped_at
      ) VALUES(1,'inspection-writer','running',NULL,1,1,NULL)
    `).run();
    seed.close();
    const before = await readFile(databasePath);
    const inspectionHooks = {
      beforeSourceRecheck(sourcePath: string): void {
        const current = statSync(sourcePath);
        const nextTimestamp = new Date(current.mtimeMs + 1_000);
        // Explicitly reproduce the source identity race at the test seam.
        utimesSync(sourcePath, nextTimestamp, nextTimestamp);
      },
    };
    await expect(startFabricDaemon({
      databasePath,
      stateDirectory,
      runtimeDirectory,
      socketPath: join(runtimeDirectory, "fabric.sock"),
      workspaceRoots: [root],
      inspectionHooks,
    })).rejects.toMatchObject({
      code: "DATABASE_INSPECTION_UNSTABLE",
      preserved: false,
    });
    await expect(lstat(stateDirectory)).resolves.toMatchObject({ mode: expect.any(Number) });
    await expect(lstat(runtimeDirectory)).resolves.toMatchObject({ mode: expect.any(Number) });
    expect(await readdir(runtimeDirectory)).toEqual([]);
    expect((await readFile(databasePath)).byteLength).toBe(before.byteLength);
  });

  it("releases a bootstrap owner's local process handles without stopping the daemon", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-production-release-"));
    roots.push(root);
    const options = {
      databasePath: join(root, "state", "fabric.sqlite3"),
      stateDirectory: join(root, "state"),
      runtimeDirectory: join(root, "runtime"),
      socketPath: join(root, "runtime", "fabric.sock"),
      workspaceRoots: [root],
    };

    const owner = await startFabricDaemon(options);
    handles.push(owner);
    expect(owner.ownsProcess).toBe(true);
    owner.release();

    const attached = await startFabricDaemon(options);
    handles.push(attached);
    expect(attached.pid).toBe(owner.pid);
    expect(attached.ownsProcess).toBe(false);
    attached.release();
    await expect(attached.waitForExit()).resolves.toBeUndefined();
    process.kill(owner.pid, 0);
  });

  it("terminalises discovery after its released launcher exits and the daemon stops gracefully", async () => {
    const root = await mkdtemp(join(tmpdir(), "f-rs-"));
    roots.push(root);
    const options = {
      databasePath: join(root, "s", "f.sqlite3"),
      stateDirectory: join(root, "s"),
      runtimeDirectory: join(root, "r"),
      socketPath: join(root, "r", "f.sock"),
      workspaceRoots: [root],
    };

    const { pid } = await launchAndReleaseFromSeparateProcess(options);
    process.kill(pid, "SIGTERM");
    await expect(waitForOwnerState(options.runtimeDirectory, "stopped"))
      .resolves.toMatchObject({ pid, state: "stopped" });
    await expect(readdir(options.runtimeDirectory)).resolves.not.toContain(
      "fabric-v1.discovery.json",
    );
  });

  it("abandons a detached daemon when its launcher exits before releasing custody", async () => {
    const root = await mkdtemp(join(tmpdir(), "f-bootstrap-custody-"));
    roots.push(root);
    const options = {
      databasePath: join(root, "s", "f.sqlite3"),
      stateDirectory: join(root, "s"),
      runtimeDirectory: join(root, "r"),
      socketPath: join(root, "r", "f.sock"),
      workspaceRoots: [root],
    };

    const abandoned = await launchAndReleaseFromSeparateProcess(options, false);
    await waitForProcessExit(abandoned.pid, WIRE_WAIT);
    const restarted = await startFabricDaemon(options);
    handles.push(restarted);
    expect(restarted.pid).not.toBe(abandoned.pid);
  });

  it("removes its socket when launcher custody is lost immediately after bind", async () => {
    const root = await mkdtemp(join(tmpdir(), "f-pre-discovery-custody-"));
    roots.push(root);
    const barrierPath = join(root, "spawn.barrier");
    const options = {
      databasePath: join(root, "s", "f.sqlite3"),
      stateDirectory: join(root, "s"),
      runtimeDirectory: join(root, "r"),
      socketPath: join(root, "r", "f.sock"),
      workspaceRoots: [root],
    };
    const clientPath = fileURLToPath(new URL("../../../src/daemon/client.ts", import.meta.url));
    const script = `
      import { startFabricDaemon } from ${JSON.stringify(clientPath)};
      await startFabricDaemon(JSON.parse(process.argv[1]));
    `;
    const launcher = spawn(
      process.execPath,
      ["--import", "tsx", "--input-type=module", "-e", script, JSON.stringify(options)],
      {
        cwd: fileURLToPath(new URL("../../..", import.meta.url)),
        detached: true,
        env: {
          ...process.env,
          AGENT_FABRIC_TEST_BOOTSTRAP_SOCKET_BARRIER_PATH: barrierPath,
        },
        stdio: "ignore",
      },
    );
    if (launcher.pid === undefined) throw new Error("custody launcher pid is unavailable");
    const daemonPid = Number((await waitForFile(barrierPath, WIRE_WAIT)).trim());
    expect(Number.isSafeInteger(daemonPid)).toBe(true);
    expect(await readdir(options.runtimeDirectory)).not.toContain("fabric-v1.discovery.json");

    process.kill(launcher.pid, "SIGKILL");
    await waitForProcessExit(launcher.pid, WIRE_WAIT);
    await waitForProcessExit(daemonPid, WIRE_WAIT);
    await rm(barrierPath, { force: true });
    await expect(stat(options.socketPath)).rejects.toMatchObject({ code: "ENOENT" });

    const restarted = await startFabricDaemon(options);
    handles.push(restarted);
    expect(restarted.pid).not.toBe(daemonPid);
  });

  it("keeps a released active daemon outside its launcher's process group", async () => {
    const root = await mkdtemp(join(tmpdir(), "f-release-custody-"));
    roots.push(root);
    const options = {
      databasePath: join(root, "s", "f.sqlite3"),
      stateDirectory: join(root, "s"),
      runtimeDirectory: join(root, "r"),
      socketPath: join(root, "r", "f.sock"),
      workspaceRoots: [root],
    };

    const { pid, launcherPid } = await launchAndReleaseFromSeparateProcess(options);
    await createCurrentSessionRun({
      databasePath: options.databasePath,
      workspaceRoot: root,
      runId: "run_release_custody_01",
      projectRunDirectory: join(root, "project-run"),
      chair: { agentId: "chair_release_custody_01", authority: MCP_ROOT_AUTHORITY },
    });

    try {
      process.kill(-launcherPid, "SIGHUP");
    } catch (error: unknown) {
      if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
    }
    await settle(250, "SIGHUP to the launcher's process group never reaches the released daemon");

    try {
      const attached = await startFabricDaemon(options);
      expect(attached.pid).toBe(pid);
      expect(attached.ownsProcess).toBe(false);
      attached.release();
    } finally {
      try {
        process.kill(pid, "SIGKILL");
      } catch (error: unknown) {
        if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
      }
      await waitForProcessExit(pid, WIRE_WAIT);
    }
  });

  it("keeps serving after SIGTERM while authoritative run work remains active", async () => {
    const root = await mkdtemp(join(tmpdir(), "f-signal-busy-"));
    roots.push(root);
    const options = {
      databasePath: join(root, "s", "f.sqlite3"),
      stateDirectory: join(root, "s"),
      runtimeDirectory: join(root, "r"),
      socketPath: join(root, "r", "f.sock"),
      workspaceRoots: [root],
    };
    const daemon = await startFabricDaemon(options);
    handles.push(daemon);
    const bootstrap = await connectFabricDaemon({
      socketPath: options.socketPath,
      capability: daemon.bootstrapCapability,
    });
    await createCurrentSessionRun({
      databasePath: options.databasePath,
      workspaceRoot: root,
      runId: "run_signal_busy_01",
      projectRunDirectory: join(root, "project-run"),
      chair: { agentId: "chair_signal_busy_01", authority: MCP_ROOT_AUTHORITY },
    });
    await bootstrap.close();

    process.kill(daemon.pid, "SIGTERM");
    await settle(250, "SIGTERM does not stop a daemon with authoritative run work in flight");
    expect(() => process.kill(daemon.pid, 0)).not.toThrow();
    const owner = JSON.parse(await readFile(
      join(options.runtimeDirectory, "fabric-v1.discovery-owner.json"),
      "utf8",
    )) as { state: string };
    expect(owner.state).toBe("active");

    process.kill(daemon.pid, "SIGKILL");
    await daemon.waitForExit();
  });

  it("recovers a crashed daemon after its released launcher exits", async () => {
    const root = await mkdtemp(join(tmpdir(), "f-rc-"));
    roots.push(root);
    const options = {
      databasePath: join(root, "s", "f.sqlite3"),
      stateDirectory: join(root, "s"),
      runtimeDirectory: join(root, "r"),
      socketPath: join(root, "r", "f.sock"),
      workspaceRoots: [root],
    };

    const { pid: crashedPid } = await launchAndReleaseFromSeparateProcess(options);
    process.kill(crashedPid, "SIGKILL");
    await waitForProcessExit(crashedPid, WIRE_WAIT);
    const restarted = await startFabricDaemon(options);
    handles.push(restarted);
    expect(restarted.pid).not.toBe(crashedPid);
    expect(restarted.ownsProcess).toBe(true);
  });

  it("coalesces twelve production contenders onto exactly one child and one private discovery owner", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-production-contention-"));
    roots.push(root);
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(root, "runtime");
    const options = {
      databasePath: join(stateDirectory, "fabric.sqlite3"),
      stateDirectory,
      runtimeDirectory,
      socketPath: join(runtimeDirectory, "fabric.sock"),
      workspaceRoots: [root],
    };

    const contenders = await Promise.all(Array.from({ length: 12 }, async () => await startFabricDaemon(options)));
    handles.push(...contenders);

    expect(new Set(contenders.map((handle) => handle.pid)).size).toBe(1);
    expect(contenders.filter((handle) => handle.ownsProcess)).toHaveLength(1);
    const discovery = JSON.parse(await readFile(join(runtimeDirectory, "fabric-v1.discovery.json"), "utf8")) as Record<string, unknown>;
    expect(Object.keys(discovery).sort()).toEqual([
      "bootstrapCapability", "features", "lifecycleReceiptAuthorityId", "pid",
      "protocolVersion", "schemaVersion", "socketPath",
    ]);
    expect(discovery).toMatchObject({
      protocolVersion: 1,
      features: ["rpc", "mcp-bootstrap-credentials.v2"],
    });
    expect((await stat(join(runtimeDirectory, "fabric-v1.discovery.json"))).mode & 0o777).toBe(0o600);
    expect([
      ...await readdir(stateDirectory),
      ...await readdir(runtimeDirectory),
    ].filter((name) => name.endsWith(".lock.sqlite3"))).toEqual([]);
  });

  it("records clean terminal ownership and advances the daemon epoch before restart", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-production-restart-"));
    roots.push(root);
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(root, "runtime");
    const options = {
      databasePath: join(stateDirectory, "fabric.sqlite3"),
      stateDirectory,
      runtimeDirectory,
      socketPath: join(runtimeDirectory, "fabric.sock"),
      workspaceRoots: [root],
    };

    const first = await startFabricDaemon(options);
    handles.push(first);
    const firstOwner = JSON.parse(await readFile(join(runtimeDirectory, "fabric-v1.discovery-owner.json"), "utf8")) as {
      daemonInstanceGeneration: number;
      state: string;
    };
    await first.stop();
    const stoppedOwner = JSON.parse(await readFile(join(runtimeDirectory, "fabric-v1.discovery-owner.json"), "utf8")) as {
      daemonInstanceGeneration: number;
      state: string;
    };
    expect(stoppedOwner).toMatchObject({
      daemonInstanceGeneration: firstOwner.daemonInstanceGeneration,
      state: "stopped",
    });

    const restarted = await startFabricDaemon(options);
    handles.push(restarted);
    const restartedOwner = JSON.parse(await readFile(join(runtimeDirectory, "fabric-v1.discovery-owner.json"), "utf8")) as {
      daemonInstanceGeneration: number;
      state: string;
    };
    expect(restarted.pid).not.toBe(first.pid);
    expect(restartedOwner).toMatchObject({
      daemonInstanceGeneration: firstOwner.daemonInstanceGeneration + 1,
      state: "active",
    });
  });

  it("does not replace ambiguous legacy discovery with a blind spawn", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-production-ambiguous-"));
    roots.push(root);
    const stateDirectory = join(root, "state");
    const runtimeDirectory = join(root, "runtime");
    const socketPath = join(runtimeDirectory, "fabric.sock");
    await Promise.all([
      mkdir(stateDirectory, { mode: 0o700 }),
      mkdir(runtimeDirectory, { mode: 0o700 }),
    ]);
    await writeFile(join(runtimeDirectory, "fabric-v1.discovery.json"), `${JSON.stringify({
      schemaVersion: 1,
      socketPath,
      pid: 2_147_483_647,
      bootstrapCapability: `afb_${"a".repeat(43)}`,
      lifecycleReceiptAuthorityId: null,
    })}\n`, { mode: 0o600 });

    await expect(startFabricDaemon({
      databasePath: join(stateDirectory, "fabric.sqlite3"),
      stateDirectory,
      runtimeDirectory,
      socketPath,
      workspaceRoots: [root],
    })).rejects.toMatchObject({ code: "BOOTSTRAP_RECONCILIATION_REQUIRED" });
    expect(await readdir(runtimeDirectory)).not.toContain("fabric.sock");

    await rm(join(runtimeDirectory, "fabric-v1.discovery.json"));
    await writeFile(socketPath, "orphaned socket placeholder\n", { mode: 0o600 });
    await expect(startFabricDaemon({
      databasePath: join(stateDirectory, "fabric.sqlite3"),
      stateDirectory,
      runtimeDirectory,
      socketPath,
      workspaceRoots: [root],
    })).rejects.toMatchObject({ code: "BOOTSTRAP_RECONCILIATION_REQUIRED" });
  });
});
