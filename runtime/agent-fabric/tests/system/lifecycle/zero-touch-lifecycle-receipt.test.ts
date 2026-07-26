import { copyFile, lstat, mkdir, mkdtemp, readFile, readdir, realpath, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import {
  bootstrapMcpSeat,
  McpBootstrapSchemaCutoverGateError,
  type LifecycleAction,
  type LifecycleActionReceipt,
} from "../../../src/cli/mcp-bootstrap.ts";
import { Fabric } from "../../../src/core/fabric.ts";
import type { FabricPaths } from "../../../src/cli/paths.ts";
import { projectKey } from "../../../src/cli/seat-store.ts";
import { runWorkspaceTrust } from "../../../src/cli/workspace-trust.ts";
import { terminateTrackedTestProcess, trackTestProcess } from "../../support/test-process-registry.ts";

const roots: string[] = [];
const daemonPids = new Set<number>();
const compatibilitySource = fileURLToPath(new URL("../../../../../config/adapter-compatibility.yaml", import.meta.url));
const compatibilitySchemaSource = fileURLToPath(new URL("../../../schemas/adapter-compatibility.schema.json", import.meta.url));

type Fixture = { projectRoot: string; paths: FabricPaths; agentsHome: string };

async function fixture(): Promise<Fixture> {
  const root = await realpath(await mkdtemp("/tmp/afb-lifecycle-"));
  roots.push(root);
  const projectRoot = join(root, "project");
  const stateDirectory = join(root, "state");
  const runtimeDirectory = join(root, "runtime");
  const agentsHome = join(root, "agents-home");
  const configDirectory = join(agentsHome, "config");
  const schemaDirectory = join(agentsHome, "runtime", "agent-fabric", "schemas");
  await Promise.all([
    mkdir(projectRoot),
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
      '  - "${AGENTS_HOME}"',
      "limits:",
      "  maximumConcurrentProviderTurns: 8",
      "",
    ].join("\n"), { mode: 0o600 }),
    copyFile(compatibilitySource, join(configDirectory, "adapter-compatibility.yaml")),
    copyFile(compatibilitySchemaSource, join(schemaDirectory, "adapter-compatibility.schema.json")),
  ]);
  const paths: FabricPaths = {
    stateDirectory,
    runtimeDirectory,
    databasePath: join(stateDirectory, "fabric-v1.sqlite3"),
    socketPath: join(runtimeDirectory, "fabric-v1.sock"),
  };
  await runWorkspaceTrust(["trust", projectRoot], paths);
  return { projectRoot, paths, agentsHome };
}

async function bootstrap(fixtureValue: Fixture, seat: "claude" | "codex" = "codex") {
  const installed = await bootstrapMcpSeat({
    environment: { AGENT_FABRIC_SEAT: seat, AGENTS_HOME: fixtureValue.agentsHome },
    cwd: fixtureValue.projectRoot,
    paths: fixtureValue.paths,
  });
  const daemonAction = installed.receipt.actions.find((action) => action.action === "daemon");
  if (daemonAction?.action === "daemon") {
    trackTestProcess(daemonAction.pid, "zero-touch-lifecycle-daemon");
    daemonPids.add(daemonAction.pid);
  }
  return installed;
}

function action<Name extends LifecycleAction["action"]>(
  receipt: LifecycleActionReceipt,
  name: Name,
): Extract<LifecycleAction, { action: Name }> {
  const found = receipt.actions.find((candidate) => candidate.action === name);
  if (found === undefined) throw new Error(`receipt is missing the ${name} action`);
  return found as Extract<LifecycleAction, { action: Name }>;
}

/** Every byte and inode timestamp of the installed generation and its pointer. */
async function seatFootprint(paths: FabricPaths, projectRoot: string): Promise<Record<string, string>> {
  const seatRoot = join(paths.stateDirectory, "seats", projectKey(projectRoot));
  const pointer = await readFile(join(seatRoot, "current.json"), "utf8");
  const generation = (JSON.parse(pointer) as { generation: string }).generation;
  const generationDirectory = join(seatRoot, "generations", generation);
  const footprint: Record<string, string> = { "current.json": pointer };
  const pointerInfo = await lstat(join(seatRoot, "current.json"));
  footprint["current.json:mtime"] = String(pointerInfo.mtimeMs);
  for (const entry of (await readdir(generationDirectory)).sort()) {
    footprint[entry] = await readFile(join(generationDirectory, entry), "utf8");
    footprint[`${entry}:mtime`] = String((await lstat(join(generationDirectory, entry))).mtimeMs);
  }
  return footprint;
}

/** Row counts a seat rotation or daemon restart would necessarily change. */
function custodyCounts(databasePath: string): Record<string, number> {
  const database = new Database(databasePath, { readonly: true });
  try {
    const counts: Record<string, number> = {};
    for (const table of ["projects", "runs", "agents", "capabilities", "mcp_seat_generations", "mcp_seat_generation_members"]) {
      counts[table] = (database.prepare(`SELECT count(*) AS rows FROM "${table}"`).get() as { rows: number }).rows;
    }
    counts.activeGenerations = (database.prepare(
      "SELECT count(*) AS rows FROM mcp_active_seat_generations",
    ).get() as { rows: number }).rows;
    return counts;
  } finally {
    database.close();
  }
}

afterEach(async () => {
  const currentRoots = roots.splice(0);
  const pids = new Set(daemonPids);
  daemonPids.clear();
  await Promise.allSettled([...pids].map(async (pid) => terminateTrackedTestProcess(pid)));
  await Promise.allSettled(currentRoots.map(async (root) => rm(root, { recursive: true, force: true })));
});

describe("zero-touch lifecycle action receipt", () => {
  it("represents every automatic action of a first bootstrap in one receipt", async () => {
    const value = await fixture();

    const installed = await bootstrap(value);

    expect(installed.receipt).toMatchObject({
      schemaVersion: 1,
      kind: "agent-fabric-lifecycle-action",
      canonicalRoot: value.projectRoot,
      seat: "codex",
      mutated: true,
      healthy: true,
    });
    expect(installed.receipt.actions.map(({ action: name }) => name)).toEqual([
      "workspace-trust",
      "daemon",
      "seat-generation",
      "identity-smoke",
    ]);
    expect(action(installed.receipt, "workspace-trust")).toMatchObject({
      outcome: "resolved",
      mutated: false,
      trustRecordDigest: expect.stringMatching(/^sha256:[0-9a-f]{64}$/u),
    });
    expect(action(installed.receipt, "daemon")).toMatchObject({ outcome: "started", mutated: true });
    expect(action(installed.receipt, "seat-generation")).toMatchObject({
      outcome: "installed",
      mutated: true,
      generation: installed.generation,
    });
    const smoke = action(installed.receipt, "identity-smoke");
    expect(smoke).toMatchObject({
      outcome: "passed",
      mutated: false,
      deadlineMs: 2_000,
      mailboxWatermark: expect.any(Number),
      code: null,
    });
    // Bounded means it terminates on its own deadline, not merely that it
    // finished: an unbounded health check is a hang.
    expect(smoke.elapsedMs).toBeLessThanOrEqual(smoke.deadlineMs);
    expect(smoke.agentId).toBe(installed.credentials.find(({ seat }) => seat === "codex")?.agentId);
    expect(JSON.stringify(installed.receipt)).not.toMatch(/afc_|afb_/u);
  });

  it("reports a bounded smoke failure without discarding the receipt", async () => {
    const value = await fixture();

    const installed = await bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex", AGENTS_HOME: value.agentsHome },
      cwd: value.projectRoot,
      paths: value.paths,
      smokeDeadlineMs: 1,
    });
    const daemonAction = action(installed.receipt, "daemon");
    trackTestProcess(daemonAction.pid, "zero-touch-lifecycle-daemon");
    daemonPids.add(daemonAction.pid);

    expect(installed.receipt.healthy).toBe(false);
    expect(action(installed.receipt, "identity-smoke")).toMatchObject({
      outcome: "failed",
      mutated: false,
      deadlineMs: 1,
      agentId: null,
      mailboxWatermark: null,
    });
    // The seat is still installed and durable; only the smoke is unhealthy.
    expect(action(installed.receipt, "seat-generation").outcome).toBe("installed");
  });

  it("performs no mutation at all on a converged repeat invocation", async () => {
    const value = await fixture();
    const first = await bootstrap(value);
    const footprintBefore = await seatFootprint(value.paths, value.projectRoot);
    const countsBefore = custodyCounts(value.paths.databasePath);

    const second = await bootstrap(value);

    // Not merely "exited zero": each action independently reports no mutation.
    expect(second.receipt.mutated).toBe(false);
    expect(second.receipt.actions.every(({ mutated }) => !mutated)).toBe(true);
    expect(action(second.receipt, "daemon")).toMatchObject({
      outcome: "attached",
      mutated: false,
      pid: action(first.receipt, "daemon").pid,
    });
    expect(action(second.receipt, "seat-generation")).toMatchObject({
      outcome: "replayed",
      mutated: false,
      generation: first.generation,
    });
    expect(action(second.receipt, "identity-smoke").outcome).toBe("passed");
    // The healthy seat was not rotated: same bytes, same inode timestamps.
    await expect(seatFootprint(value.paths, value.projectRoot)).resolves.toEqual(footprintBefore);
    // No compatible daemon restart and no rebuilt custody rows.
    expect(custodyCounts(value.paths.databasePath)).toEqual(countsBefore);
    expect(second.credential).toBe(first.credential);
  });

  it("renews an expiring roster on the next ordinary invocation and reports it as a mutation", async () => {
    const value = await fixture();
    // A seat lives 24 hours from the caller's `now` and enters the daemon's
    // renewal window with an hour left, so an origin 23.5 hours in the past
    // stores a roster that is already expiring. No user action, no new
    // conversation and no database tampering are involved.
    const first = await bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex", AGENTS_HOME: value.agentsHome },
      cwd: value.projectRoot,
      paths: value.paths,
      now: new Date(Date.now() - (23 * 60 + 30) * 60 * 1_000),
    });
    const firstDaemon = action(first.receipt, "daemon");
    trackTestProcess(firstDaemon.pid, "zero-touch-lifecycle-daemon");
    daemonPids.add(firstDaemon.pid);
    const footprintBefore = await seatFootprint(value.paths, value.projectRoot);

    const renewed = await bootstrap(value);

    expect(renewed.generation).not.toBe(first.generation);
    expect(renewed.receipt.mutated).toBe(true);
    expect(action(renewed.receipt, "seat-generation")).toMatchObject({
      outcome: "installed",
      mutated: true,
      previousGeneration: first.generation,
    });
    // Renewal is a seat mutation only; it never restarts a compatible daemon.
    expect(action(renewed.receipt, "daemon")).toMatchObject({ outcome: "attached", mutated: false });
    expect(action(renewed.receipt, "identity-smoke").outcome).toBe("passed");
    await expect(seatFootprint(value.paths, value.projectRoot)).resolves.not.toEqual(footprintBefore);
  });

  it("requests exactly one user decision for a schema mismatch and displaces nothing", async () => {
    const value = await fixture();
    // Seed non-terminal coordination work without ever electing a daemon, then
    // break the recorded schema fingerprint so the next start must gate.
    const fabric = new Fabric({ databasePath: value.paths.databasePath, workspaceRoots: [value.projectRoot] });
    try {
      fabric.bootstrapCurrentMcpSeat({
        canonicalRoot: value.projectRoot,
        trustRecordDigest: `sha256:${"a".repeat(64)}`,
        seat: "codex",
        expiresAt: new Date(Date.now() + 60 * 60 * 1_000).toISOString(),
      });
    } finally {
      await fabric.close();
    }
    const database = new Database(value.paths.databasePath);
    try {
      database.pragma("journal_mode = DELETE");
      database.prepare("UPDATE fabric_schema SET catalog_sha256=?").run("f".repeat(64));
    } finally {
      database.close();
    }
    const before = await readFile(value.paths.databasePath);

    const failure = await bootstrapMcpSeat({
      environment: { AGENT_FABRIC_SEAT: "codex", AGENTS_HOME: value.agentsHome },
      cwd: value.projectRoot,
      paths: value.paths,
    }).then(() => undefined, (error: unknown) => error);

    expect(failure).toBeInstanceOf(McpBootstrapSchemaCutoverGateError);
    const gate = (failure as McpBootstrapSchemaCutoverGateError).gate;
    expect(gate).toMatchObject({
      kind: "agent-fabric-schema-cutover-gate",
      decision: "archive-and-fresh",
      displaced: false,
      mismatch: { code: "SCHEMA_CUTOVER_REQUIRED" },
      sourceSetSha256: expect.stringMatching(/^sha256:[0-9a-f]{64}$/u),
    });
    // Counts and consequences are reported before, not after, the decision.
    const counts = new Map(gate.retained.tables.map(({ table, rows }) => [table, rows] as const));
    expect(counts.get("runs")).toBe(1);
    expect(counts.get("agents")).toBe(1);
    expect(counts.get("projects")).toBe(1);
    expect(gate.consequences.length).toBeGreaterThan(0);
    expect(gate.consequences.join(" ")).toContain("stay there until approval");
    // Routed to #215, never invoked automatically.
    expect(gate.command).toContain("database archive-and-fresh");
    expect(gate.command).toContain(`--confirm-source-set ${gate.sourceSetSha256!}`);

    // Nothing is displaced before approval: the source database is byte-identical
    // and no archive directory was created anywhere under the state directory.
    await expect(readFile(value.paths.databasePath)).resolves.toEqual(before);
    expect((await readdir(value.paths.stateDirectory)).sort()).not.toContain("archive");
  });
});
