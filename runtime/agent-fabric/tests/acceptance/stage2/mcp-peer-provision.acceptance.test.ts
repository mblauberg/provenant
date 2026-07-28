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
  });
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
      seats: Array<{
        seat: string;
        role: string;
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
  });
});
