import { execFile } from "node:child_process";
import { copyFile, lstat, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { terminateTrackedTestProcess, trackTestProcess } from "../../support/test-process-registry.ts";

const execFileAsync = promisify(execFile);
const cliMain = fileURLToPath(new URL("../../../src/cli/main.ts", import.meta.url));
const tsxLoader = fileURLToPath(import.meta.resolve("tsx"));
const compatibilitySource = fileURLToPath(new URL("../../../../../config/adapter-compatibility.yaml", import.meta.url));
const compatibilitySchemaSource = fileURLToPath(new URL("../../../schemas/adapter-compatibility.schema.json", import.meta.url));
const roots: string[] = [];
const daemonPids = new Set<number>();

afterEach(async () => {
  await Promise.allSettled([...daemonPids].map(async (pid) => {
    await terminateTrackedTestProcess(pid);
    daemonPids.delete(pid);
  }));
  await Promise.allSettled(roots.splice(0).map(async (root) => rm(root, { recursive: true, force: true })));
});

type Fixture = {
  project: string;
  stateDirectory: string;
  runtimeDirectory: string;
  databasePath: string;
  environment: NodeJS.ProcessEnv;
};

async function fixture(): Promise<Fixture> {
  const temporaryRoot = await realpath(await mkdtemp("/tmp/afp-"));
  roots.push(temporaryRoot);
  const project = join(temporaryRoot, "fresh project");
  const stateDirectory = join(temporaryRoot, "state");
  const runtimeDirectory = join(temporaryRoot, "runtime");
  const agentsHome = join(temporaryRoot, "agents-home");
  const configDirectory = join(agentsHome, "config");
  const schemaDirectory = join(agentsHome, "runtime", "agent-fabric", "schemas");
  await Promise.all([
    mkdir(project),
    mkdir(stateDirectory, { mode: 0o700 }),
    mkdir(configDirectory, { recursive: true, mode: 0o700 }),
    mkdir(schemaDirectory, { recursive: true, mode: 0o700 }),
  ]);
  await Promise.all([
    writeFile(join(configDirectory, "agent-fabric.yaml"), [
      "schemaVersion: 1",
      "allowedAdapters: []",
      "activeAdapters: []",
      "allowedProfiles: [headless]",
      "adapters: {}",
      "workspaceRoots:",
      `  - ${JSON.stringify(project)}`,
      "limits:",
      "  maximumConcurrentProviderTurns: 8",
      "",
    ].join("\n"), { mode: 0o600 }),
    copyFile(compatibilitySource, join(configDirectory, "adapter-compatibility.yaml")),
    copyFile(compatibilitySchemaSource, join(schemaDirectory, "adapter-compatibility.schema.json")),
  ]);
  return {
    project,
    stateDirectory,
    runtimeDirectory,
    databasePath: join(stateDirectory, "fabric-v1.sqlite3"),
    environment: {
      ...process.env,
      AGENTS_HOME: agentsHome,
      AGENT_FABRIC_STATE_DIRECTORY: stateDirectory,
      AGENT_FABRIC_RUNTIME_DIRECTORY: runtimeDirectory,
    },
  };
}

async function cli(
  value: Fixture,
  arguments_: string[],
): Promise<{ stdout: string; stderr: string }> {
  return await execFileAsync(process.execPath, ["--import", tsxLoader, cliMain, ...arguments_], {
    cwd: value.project,
    env: value.environment,
    timeout: 20_000,
    killSignal: "SIGKILL",
  });
}

async function cliFailure(value: Fixture, arguments_: string[]): Promise<{ stdout: string; stderr: string }> {
  try {
    await cli(value, arguments_);
  } catch (error: unknown) {
    if (
      typeof error === "object" && error !== null &&
      "stdout" in error && typeof error.stdout === "string" &&
      "stderr" in error && typeof error.stderr === "string"
    ) {
      return { stdout: error.stdout, stderr: error.stderr };
    }
    throw error;
  }
  throw new Error("expected CLI command to fail");
}

async function trackCurrentDaemon(value: Fixture): Promise<number> {
  const receipt = JSON.parse(
    await readFile(join(value.runtimeDirectory, "fabric-v1.discovery.json"), "utf8"),
  ) as { pid: number };
  daemonPids.add(receipt.pid);
  trackTestProcess(receipt.pid, "MCP peer provision acceptance daemon");
  return receipt.pid;
}

function counts(databasePath: string): { agents: number; authorities: number } {
  const database = new Database(databasePath, { readonly: true, fileMustExist: true });
  try {
    const count = (table: "agents" | "authorities"): number =>
      (database.prepare(`SELECT count(*) AS count FROM ${table}`).get() as { count: number }).count;
    return { agents: count("agents"), authorities: count("authorities") };
  } finally {
    database.close();
  }
}

describe("MCP peer provisioning from a fresh project", () => {
  it("bootstraps, starts from daemon idle, adds one narrow peer, and replays as a no-op", async () => {
    const value = await fixture();
    await cli(value, ["workspace", "trust", value.project]);
    const bootstrap = await cli(value, ["bootstrap", "--seat", "codex"]);
    expect(bootstrap.stdout).not.toMatch(/af[bc]_[A-Za-z0-9_-]{43}/u);
    const bootstrapPid = await trackCurrentDaemon(value);
    await terminateTrackedTestProcess(bootstrapPid);
    daemonPids.delete(bootstrapPid);

    const firstResult = await cli(value, [
      "mcp", "peer-provision", "--project", value.project, "--seat", "agy",
    ]);
    const first = JSON.parse(firstResult.stdout) as {
      projectSessionId: string;
      sessionRevision: number;
      sessionGeneration: number;
      runId: string;
      runRevision: number;
      chairSeat: string;
      chairAgentId: string;
      chairGeneration: number;
      chairLeaseId: string;
      seats: Array<{
        seat: string;
        role: string;
        agentId: string;
        principalGeneration: number;
        credentialPath: string;
        metadataPath: string;
      }>;
    };
    expect(first.seats.map(({ seat }) => seat)).toEqual(["agy", "codex"]);
    expect(first.seats.find(({ seat }) => seat === "agy")).toMatchObject({ role: "peer" });
    expect(firstResult.stdout).not.toMatch(/af[bc]_[A-Za-z0-9_-]{43}/u);
    expect(firstResult.stdout).not.toContain("capability");
    for (const seat of first.seats) {
      expect((await lstat(seat.credentialPath)).mode & 0o777).toBe(0o600);
      expect((await lstat(seat.metadataPath)).mode & 0o777).toBe(0o600);
    }
    await trackCurrentDaemon(value);
    const afterFirst = counts(value.databasePath);

    const secondResult = await cli(value, [
      "mcp", "peer-provision", "--project", value.project, "--seat", "agy",
    ]);
    expect(JSON.parse(secondResult.stdout)).toEqual(JSON.parse(firstResult.stdout));
    expect(counts(value.databasePath)).toEqual(afterFirst);
    expect(secondResult.stdout).not.toMatch(/af[bc]_[A-Za-z0-9_-]{43}/u);

    const renewalExpiry = new Date(Date.now() + 30 * 60 * 1_000).toISOString();
    await cli(value, [
      "mcp", "provision",
      "--project", value.project,
      "--project-session-id", first.projectSessionId,
      "--session-revision", String(first.sessionRevision),
      "--session-generation", String(first.sessionGeneration),
      "--run-id", first.runId,
      "--run-revision", String(first.runRevision),
      "--chair-seat", first.chairSeat,
      "--chair-agent-id", first.chairAgentId,
      "--chair-generation", String(first.chairGeneration),
      "--chair-lease-id", first.chairLeaseId,
      "--seat-bindings", first.seats
        .map(({ seat, agentId, principalGeneration }) => `${seat}=${agentId}@${String(principalGeneration)}`)
        .join(","),
      "--expires-at", renewalExpiry,
    ]);
    const renewalResult = await cli(value, ["bootstrap", "--seat", "codex"]);
    const renewal = JSON.parse(renewalResult.stdout) as {
      credentials: Array<{ seat: string }>;
    };
    expect(renewal.credentials.map(({ seat }) => seat)).toEqual(["agy", "codex"]);
    expect(renewalResult.stdout).not.toMatch(/af[bc]_[A-Za-z0-9_-]{43}/u);
  });

  it("converges concurrent optional seat additions without silently dropping either seat", async () => {
    const value = await fixture();
    await cli(value, ["workspace", "trust", value.project]);
    await cli(value, ["bootstrap", "--seat", "codex"]);
    const bootstrapPid = await trackCurrentDaemon(value);
    await terminateTrackedTestProcess(bootstrapPid);
    daemonPids.delete(bootstrapPid);

    const results = await Promise.all([
      cli(value, ["mcp", "peer-provision", "--project", value.project, "--seat", "agy"]),
      cli(value, ["mcp", "peer-provision", "--project", value.project, "--seat", "cursor"]),
    ]);
    for (const result of results) {
      expect(result.stdout + result.stderr).not.toMatch(/af[bc]_[A-Za-z0-9_-]{43}/u);
    }
    await trackCurrentDaemon(value);

    const database = new Database(value.databasePath, { readonly: true, fileMustExist: true });
    try {
      const members = database.prepare(`
        SELECT member.seat
          FROM mcp_active_seat_generations active
          JOIN mcp_seat_generation_members member ON member.generation=active.generation
         ORDER BY member.seat
      `).all() as Array<{ seat: string }>;
      expect(members.map(({ seat }) => seat)).toEqual(["agy", "codex", "cursor"]);
    } finally {
      database.close();
    }
  });

  it("keeps chair-seat and revoked-lease failures on their real enforcement paths", async () => {
    const value = await fixture();
    await cli(value, ["workspace", "trust", value.project]);
    await cli(value, ["bootstrap", "--seat", "codex"]);
    await trackCurrentDaemon(value);

    const chairFailure = await cliFailure(value, [
      "mcp", "peer-provision", "--project", value.project, "--seat", "codex",
    ]);
    expect(chairFailure.stderr).toMatch(/refuses to provision the active chair seat codex/u);
    expect(chairFailure.stdout + chairFailure.stderr).not.toMatch(/af[bc]_[A-Za-z0-9_-]{43}/u);

    const database = new Database(value.databasePath);
    try {
      database.prepare("UPDATE run_chair_leases SET status='revoked' WHERE status='active'").run();
    } finally {
      database.close();
    }
    const leaseFailure = await cliFailure(value, [
      "mcp", "peer-provision", "--project", value.project, "--seat", "agy",
    ]);
    expect(leaseFailure.stderr).toMatch(/active operator-launched run|chair lease/iu);
    expect(leaseFailure.stderr).not.toMatch(/authori[sz]ation step/iu);
    expect(leaseFailure.stdout + leaseFailure.stderr).not.toMatch(/af[bc]_[A-Za-z0-9_-]{43}/u);
  });
});
