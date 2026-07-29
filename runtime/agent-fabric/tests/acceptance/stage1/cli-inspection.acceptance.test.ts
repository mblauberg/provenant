import { chmod, mkdir, mkdtemp, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";

import { afterEach, describe, expect, it } from "vitest";

import { AUTHORITY_ACTION_VOCABULARY, openFabric, startFabricDaemon } from "../../../src/index.ts";
import { TEST_AUTHORITY_V2_FIELDS } from "../../support/authority-v2-testkit.ts";
import { parseCliJson, runSourceCli } from "../../support/cli-process.ts";
import { writeDeliveryRunFixture } from "../../support/delivery-run-fixture.ts";
import { createCurrentSessionRun } from "../../support/current-session-testkit.ts";

const cleanup: Array<() => Promise<void>> = [];

afterEach(async () => {
  await Promise.all(cleanup.splice(0).map((close) => close()));
});

async function createInspectionDatabase(databasePath: string, runId: string, projectRunDirectory: string): Promise<void> {
  await mkdir(dirname(databasePath), { recursive: true });
  await mkdir(projectRunDirectory, { recursive: true });
  const fabric = await openFabric({ databasePath, workspaceRoots: [dirname(dirname(projectRunDirectory))] });
  try {
    await createCurrentSessionRun({
      databasePath,
      workspaceRoot: dirname(dirname(projectRunDirectory)),
      runId,
      projectRunDirectory,
      chair: {
        agentId: "chair",
        authority: {
          ...TEST_AUTHORITY_V2_FIELDS,
          workspaceRoots: ["."],
          sourcePaths: ["."],
          artifactPaths: [".agent-run"],
          actions: [...AUTHORITY_ACTION_VOCABULARY],
          disclosure: { level: "scoped", scopes: ["local"] } as const,
          expiresAt: "2099-01-01T00:00:00.000Z",
          budget: { turns: 10, "cost:USD": 5 },
        },
      },
    });
  } finally {
    await fabric.close();
  }
}

describe("Stage 1 command-line inspection", () => {
  it("inspects an explicitly selected database as machine-readable JSON", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-cli-inspect-"));
    cleanup.push(async () => rm(root, { recursive: true, force: true }));
    const databasePath = join(root, "fabric.sqlite3");
    await createInspectionDatabase(databasePath, "run-inspect", join(root, ".agent-run", "run-inspect"));

    const result = await runSourceCli(["inspect", "--database", databasePath, "--json"]);
    const output = parseCliJson(result);

    expect(result.stderr).toBe("");
    expect(output).toMatchObject({
      schemaVersion: 1,
      databasePath,
      runs: [{ runId: "run-inspect", chairAgentId: "chair" }],
    });
  });

  it("reports the live serving socket recorded by private generation-bound discovery", async () => {
    const root = await mkdtemp(join(tmpdir(), "afcli-"));
    const databasePath = join(root, "state", "fabric.sqlite3");
    const runtimeDirectory = join(root, "r");
    const socketPath = join(runtimeDirectory, "f.sock");
    const daemon = await startFabricDaemon({
      databasePath,
      stateDirectory: join(root, "state"),
      runtimeDirectory,
      socketPath,
      workspaceRoots: [root],
    });
    cleanup.push(async () => {
      await daemon.stop();
      await rm(root, { recursive: true, force: true });
    });

    const result = await runSourceCli([
      "inspect",
      "--database",
      databasePath,
      "--runtime-directory",
      runtimeDirectory,
      "--json",
    ]);
    expect(parseCliJson(result)).toMatchObject({ databasePath, runtimeDirectory, socketPath });
  });

  it("verifies the fabric receipt declared by RUN.json without starting a daemon", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-cli-receipt-"));
    cleanup.push(async () => rm(root, { recursive: true, force: true }));
    const runDirectory = join(root, ".agent-run", "run-receipt");
    const databasePath = join(root, "fabric.sqlite3");
    await mkdir(runDirectory, { recursive: true });
    const fabric = await openFabric({ databasePath, workspaceRoots: [root] });
    const run = await createCurrentSessionRun({
      databasePath,
      workspaceRoot: root,
      runId: "run-receipt",
      projectRunDirectory: runDirectory,
      chair: {
        agentId: "chair",
        authority: {
          ...TEST_AUTHORITY_V2_FIELDS,
          workspaceRoots: ["."],
          sourcePaths: ["."],
          artifactPaths: [".agent-run/run-receipt"],
          actions: [...AUTHORITY_ACTION_VOCABULARY],
          disclosure: { level: "scoped", scopes: ["local"] } as const,
          expiresAt: "2099-01-01T00:00:00.000Z",
          budget: { turns: 10, "cost:USD": 5 },
        },
      },
    });
    const exported = await fabric.connect(run.chairCapability).exportReceipt({ commandId: "cli:receipt:export:1" });
    await fabric.close();
    const runReceiptPath = await writeDeliveryRunFixture({
      runDirectory,
      runId: "run-receipt",
      artifactPath: `.agent-run/run-receipt/${exported.relativePath}`,
      artifactSha256: exported.sha256,
    });

    const result = await runSourceCli(["receipt", "verify", "--run-receipt", runReceiptPath]);

    expect(result).toMatchObject({ exitCode: 0, signal: null, stderr: "" });
    expect(result.stdout).toMatch(/verified/u);
    expect(result.stdout).toContain(exported.sha256);
  });

  it("resolves one stable state/runtime root without mutating directories", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-cli-paths-"));
    cleanup.push(async () => rm(root, { recursive: true, force: true }));
    const home = join(root, "home");
    const temporary = join(root, "tmp");
    const stateDirectory = join(home, ".local", "state", "agent-harness", "fabric");
    const databasePath = join(stateDirectory, "fabric-v1.sqlite3");
    await mkdir(temporary, { recursive: true, mode: 0o755 });
    await mkdir(stateDirectory, { recursive: true, mode: 0o755 });
    await chmod(stateDirectory, 0o755);
    await createInspectionDatabase(databasePath, "run-fallback", join(root, ".agent-run", "run-fallback"));

    const result = await runSourceCli(["inspect", "--json"], {
      environment: {
        HOME: home,
        TMPDIR: temporary,
        XDG_RUNTIME_DIR: undefined,
        XDG_STATE_HOME: undefined,
        AGENT_FABRIC_DATABASE_PATH: undefined,
        AGENT_FABRIC_RUNTIME_DIRECTORY: undefined,
        AGENT_FABRIC_STATE_DIRECTORY: undefined,
      },
    });
    const output = parseCliJson(result);

    expect(output).toMatchObject({ schemaVersion: 1, databasePath, stateDirectory });
    expect(output).toHaveProperty("runtimeDirectory");
    expect(output).toHaveProperty("socketPath");
    if (
      typeof output !== "object" ||
      output === null ||
      !("runtimeDirectory" in output) ||
      typeof output.runtimeDirectory !== "string" ||
      !("socketPath" in output) ||
      typeof output.socketPath !== "string"
    ) {
      throw new TypeError("CLI path inspection did not return string runtime paths");
    }
    expect(output.runtimeDirectory).not.toBe(temporary);
    expect(output.runtimeDirectory).toBe(join(stateDirectory, "runtime"));
    expect(output.socketPath).toBe(join(output.runtimeDirectory, "fabric-v1.sock"));
    expect((await stat(stateDirectory)).mode & 0o777).toBe(0o755);
    await expect(stat(output.runtimeDirectory)).rejects.toMatchObject({ code: "ENOENT" });
  });
});
