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

/**
 * Every byte and inode timestamp under the project's seat root: the pointer,
 * the installed generation, the legacy marker if one exists, and the set of
 * sibling entries under `generations/` so an abandoned credential-bearing
 * staging tree cannot hide behind an unchanged generation directory.
 */
async function seatFootprint(paths: FabricPaths, projectRoot: string): Promise<Record<string, string>> {
  const seatRoot = join(paths.stateDirectory, "seats", projectKey(projectRoot));
  const footprint: Record<string, string> = {};
  for (const name of (await readdir(seatRoot)).sort()) {
    if (name === "generations" || name === "current.lock") continue;
    footprint[name] = await readFile(join(seatRoot, name), "utf8");
    footprint[`${name}:mtime`] = String((await lstat(join(seatRoot, name))).mtimeMs);
  }
  const generationsDirectory = join(seatRoot, "generations");
  const siblings = (await readdir(generationsDirectory)).sort();
  footprint["generations:entries"] = siblings.join(",");
  for (const sibling of siblings) {
    for (const entry of (await readdir(join(generationsDirectory, sibling))).sort()) {
      const path = join(generationsDirectory, sibling, entry);
      footprint[`${sibling}/${entry}`] = await readFile(path, "utf8");
      footprint[`${sibling}/${entry}:mtime`] = String((await lstat(path)).mtimeMs);
    }
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
      "custody",
      "seat-generation",
      "identity-smoke",
    ]);
    expect(action(installed.receipt, "workspace-trust")).toMatchObject({
      outcome: "resolved",
      mutated: false,
      trustRecordDigest: expect.stringMatching(/^sha256:[0-9a-f]{64}$/u),
    });
    expect(action(installed.receipt, "daemon")).toMatchObject({ outcome: "started", mutated: true });
    expect(action(installed.receipt, "custody")).toMatchObject({
      outcome: "committed",
      mutated: true,
      projectId: expect.any(String),
      runId: expect.any(String),
      generation: installed.generation,
    });
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

  it("reconciles a committed real-daemon bootstrap whose first result is malformed", async () => {
    const value = await fixture();
    const previous = process.env.AGENT_FABRIC_TEST_BOOTSTRAP_MALFORMED_RESULT_COUNT;
    process.env.AGENT_FABRIC_TEST_BOOTSTRAP_MALFORMED_RESULT_COUNT = "1";
    try {
      const installed = await bootstrap(value);

      expect(installed.receipt).toMatchObject({ healthy: true });
      expect(action(installed.receipt, "custody")).toMatchObject({
        outcome: "reconciled",
        mutated: false,
        generation: installed.generation,
      });
      expect(action(installed.receipt, "seat-generation")).toMatchObject({
        outcome: "installed",
        mutated: true,
        generation: installed.generation,
      });
      expect(custodyCounts(value.paths.databasePath)).toMatchObject({
        projects: 1,
        runs: 1,
        agents: 1,
        mcp_seat_generations: 1,
        activeGenerations: 1,
      });
    } finally {
      if (previous === undefined) delete process.env.AGENT_FABRIC_TEST_BOOTSTRAP_MALFORMED_RESULT_COUNT;
      else process.env.AGENT_FABRIC_TEST_BOOTSTRAP_MALFORMED_RESULT_COUNT = previous;
    }
  }, 30_000);

  it("keeps two malformed real-daemon responses terminally conservative", async () => {
    const value = await fixture();
    const previous = process.env.AGENT_FABRIC_TEST_BOOTSTRAP_MALFORMED_RESULT_COUNT;
    process.env.AGENT_FABRIC_TEST_BOOTSTRAP_MALFORMED_RESULT_COUNT = "2";
    try {
      const failure = await bootstrap(value).then(() => undefined, (error: unknown) => error as Error & {
        code?: string;
        receipt?: { actions: LifecycleAction[] };
      });
      if (failure === undefined) throw new Error("expected custody ambiguity failure");

      expect(failure).toMatchObject({
        code: "CUSTODY_AMBIGUOUS",
        receipt: { failure: { phase: "custody-ambiguous" } },
      });
      expect(failure.receipt?.actions.some(({ action: name }) => name === "custody")).toBe(false);
      expect(custodyCounts(value.paths.databasePath)).toMatchObject({
        projects: 1,
        runs: 1,
        agents: 1,
        mcp_seat_generations: 1,
        activeGenerations: 1,
      });
    } finally {
      if (previous === undefined) delete process.env.AGENT_FABRIC_TEST_BOOTSTRAP_MALFORMED_RESULT_COUNT;
      else process.env.AGENT_FABRIC_TEST_BOOTSTRAP_MALFORMED_RESULT_COUNT = previous;
    }
  }, 30_000);

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
    // `mutated` claims no logical custody change, so the assertions below prove
    // that claim, and separately prove no abandoned staging tree was left.
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
    // The healthy seat was not rotated: same bytes, same inode timestamps, and
    // the same set of entries under generations/ — a replay that abandoned a
    // credential-bearing .staging-* tree would change that set.
    const footprintAfter = await seatFootprint(value.paths, value.projectRoot);
    expect(footprintAfter).toEqual(footprintBefore);
    expect(footprintAfter["generations:entries"]).toBe(first.generation);
    // No compatible daemon restart and no rebuilt custody rows.
    expect(custodyCounts(value.paths.databasePath)).toEqual(countsBefore);
    expect(second.credential).toBe(first.credential);
  });

  it("reports first legacy bootstrap provenance exactly once without changing custody mutation semantics", async () => {
    const value = await fixture();
    const first = await bootstrap(value);
    const seatRoot = join(value.paths.stateDirectory, "seats", projectKey(value.projectRoot));
    const metadataPath = join(seatRoot, "generations", first.generation, "codex.json");
    // Age the roster into the legacy shape the replay path marks, exactly as
    // an installation predating originKind would present it.
    const legacy = JSON.parse(await readFile(metadataPath, "utf8")) as Record<string, unknown>;
    delete legacy.originKind;
    await writeFile(metadataPath, `${JSON.stringify(legacy, null, 2)}\n`, { mode: 0o600 });

    const marked = await bootstrap(value);
    const markerPath = join(seatRoot, "legacy-bootstrap.json");
    const markerBefore = await readFile(markerPath, "utf8");
    const markerMtimeBefore = (await lstat(markerPath)).mtimeMs;
    expect(marked.receipt.mutated).toBe(true);
    expect(marked.receipt.actions.filter(({ action: name }) => name !== "legacy-bootstrap-provenance")
      .every(({ mutated }) => !mutated)).toBe(true);
    expect(action(marked.receipt, "legacy-bootstrap-provenance")).toEqual({
      action: "legacy-bootstrap-provenance",
      outcome: "recorded",
      mutated: true,
      generation: first.generation,
    });

    const replayed = await bootstrap(value);

    expect(marked.generation).toBe(first.generation);
    expect(replayed.receipt.mutated).toBe(false);
    expect(replayed.receipt.actions.every(({ mutated }) => !mutated)).toBe(true);
    expect(replayed.receipt.actions.some(({ action: name }) => name === "legacy-bootstrap-provenance")).toBe(false);
    // An unconditional marker rewrite would contradict `mutated: false` on
    // every repeat invocation.
    await expect(readFile(markerPath, "utf8")).resolves.toBe(markerBefore);
    expect((await lstat(markerPath)).mtimeMs).toBe(markerMtimeBefore);
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
    expect(gate.consequences.join(" ")).toContain(
      "asks for ARCHIVE-AND-FRESH on its controlling terminal",
    );
    expect(gate.consequences.join(" ")).toContain("refuses non-interactive execution");
    // Routed to #215, never invoked automatically.
    expect(gate.command).toContain("database archive-and-fresh");
    expect(gate.command).toContain(`--confirm-source-set ${gate.sourceSetSha256!}`);

    // Nothing is displaced before approval: the source database is byte-identical
    // and no archive directory was created anywhere under the state directory.
    await expect(readFile(value.paths.databasePath)).resolves.toEqual(before);
    expect((await readdir(value.paths.stateDirectory)).sort()).not.toContain("archive");
    // Unlike its siblings this scenario builds the whole current schema
    // in-process rather than in a daemon child, then reclones it for the
    // cutover inspection and the census, so it needs more than the 15s default.
  }, 45_000);
});
