import { chmod, copyFile, lstat, mkdir, mkdtemp, readFile, realpath, rm, symlink, unlink, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { connectFabricDaemon, startFabricDaemon, type FabricDaemonHandle } from "../../../src/daemon/client.ts";
import { currentMcpSeatGeneration } from "../../../src/core/mcp-seat-generation.ts";
import { MCP_ROOT_AUTHORITY } from "../../support/mcp-testkit.ts";
import { parseCliJson, runSourceCli } from "../../support/cli-process.ts";
import { createCurrentSessionRun } from "../../support/current-session-testkit.ts";
import { terminateTrackedTestProcess, trackTestProcess } from "../../support/test-process-registry.ts";

const roots: string[] = [];
const daemons: FabricDaemonHandle[] = [];
const daemonPids = new Set<number>();
const compatibilitySource = fileURLToPath(new URL("../../../../../config/adapter-compatibility.yaml", import.meta.url));
const compatibilitySchemaSource = fileURLToPath(new URL("../../../schemas/adapter-compatibility.schema.json", import.meta.url));

afterEach(async () => {
  await Promise.allSettled([...daemonPids].map(async (pid) => {
    await terminateTrackedTestProcess(pid);
    daemonPids.delete(pid);
  }));
  await Promise.allSettled(daemons.splice(0).map(async (daemon) => daemon.stop()));
  await Promise.allSettled(roots.splice(0).map(async (root) => rm(root, { recursive: true, force: true })));
});

type Fixture = {
  environment: Record<string, string>;
  projectPath: string;
  stateDirectory: string;
  databasePath: string;
  identity: {
    projectSessionId: string;
    sessionRevision: number;
    sessionGeneration: number;
    runId: string;
    runRevision: number;
    chairAgentId: string;
    chairGeneration: number;
    chairLeaseId: string;
  };
  seatBindings: string;
};

const CURRENT_BINDINGS = [
  { seat: "agy", agentId: "agent_agy", expectedPrincipalGeneration: 1 },
  { seat: "claude", agentId: "agent_claude", expectedPrincipalGeneration: 1 },
  { seat: "codex", agentId: "agent_codex_chair", expectedPrincipalGeneration: 1 },
  { seat: "cursor", agentId: "agent_cursor", expectedPrincipalGeneration: 1 },
  { seat: "kiro", agentId: "agent_kiro", expectedPrincipalGeneration: 1 },
] as const;

async function fixture(options: { broaderRoot?: boolean; daemonIdle?: boolean } = {}): Promise<Fixture> {
  const root = await mkdtemp(join(tmpdir(), "fabric-mcp-provision-"));
  roots.push(root);
  const stateDirectory = join(root, "state");
  const runtimeDirectory = join(root, "runtime");
  const agentsHome = join(root, "agents-home");
  const configDirectory = join(agentsHome, "config");
  const schemaDirectory = join(agentsHome, "runtime", "agent-fabric", "schemas");
  const requestedProjectPath = options.broaderRoot === true
    ? join(root, "projects", "one")
    : join(root, "project");
  await Promise.all([
    mkdir(requestedProjectPath, { recursive: true }),
    mkdir(configDirectory, { recursive: true, mode: 0o700 }),
    mkdir(schemaDirectory, { recursive: true, mode: 0o700 }),
  ]);
  const projectPath = await realpath(requestedProjectPath);
  await Promise.all([
    writeFile(join(configDirectory, "agent-fabric.yaml"), [
      "schemaVersion: 1",
      "allowedAdapters: []",
      "activeAdapters: []",
      "allowedProfiles: [headless]",
      "adapters: {}",
      "workspaceRoots:",
      `  - ${JSON.stringify(options.broaderRoot === true ? root : projectPath)}`,
      "limits:",
      "  maximumConcurrentProviderTurns: 8",
      "",
    ].join("\n"), { mode: 0o600 }),
    copyFile(compatibilitySource, join(configDirectory, "adapter-compatibility.yaml")),
    copyFile(compatibilitySchemaSource, join(schemaDirectory, "adapter-compatibility.schema.json")),
  ]);
  const databasePath = join(stateDirectory, "fabric-v1.sqlite3");
  const socketPath = join(runtimeDirectory, "fabric-v1.sock");
  const daemon = await startFabricDaemon({
    databasePath,
    stateDirectory,
    runtimeDirectory,
    socketPath,
    workspaceRoots: [options.broaderRoot === true ? root : projectPath],
  });
  daemons.push(daemon);
  const discoveryPath = join(runtimeDirectory, "fabric-v1.discovery.json");
  await writeFile(
    discoveryPath,
    `${JSON.stringify({
      schemaVersion: 1,
      socketPath,
      pid: daemon.pid,
      bootstrapCapability: daemon.bootstrapCapability,
      lifecycleReceiptAuthorityId: null,
    })}\n`,
    { mode: 0o600 },
  );
  await chmod(discoveryPath, 0o600);

  const current = await createCurrentSessionRun({
    databasePath,
    workspaceRoot: projectPath,
    runId: "run_current_mcp",
    projectRunDirectory: join(projectPath, ".agent-run", "run_current_mcp"),
    chair: {
      agentId: "agent_codex_chair",
      authority: {
        ...MCP_ROOT_AUTHORITY,
        workspaceRoots: ["."],
        sourcePaths: ["."],
        artifactPaths: [".agent-run"],
      },
    },
  });
  const chair = await connectFabricDaemon({ socketPath, capability: current.chairCapability });
  try {
    for (const agentId of ["agent_agy", "agent_claude", "agent_cursor", "agent_kiro"]) {
      const delegated = await chair.delegateAuthority({
        parentAuthorityId: current.chairAuthorityId,
        commandId: `mcp-current-fixture:authority:${agentId}`,
        authority: {
          ...MCP_ROOT_AUTHORITY,
          workspaceRoots: ["."],
          sourcePaths: ["."],
          artifactPaths: [".agent-run"],
          budget: { turns: 8, "cost:USD": 8, descendants: 0 },
        },
      });
      await chair.registerAgent({ agentId, authorityId: delegated.authorityId });
    }
  } finally {
    await chair.close();
  }
  if (options.daemonIdle === true) {
    await daemon.stop();
    daemons.splice(daemons.indexOf(daemon), 1);
  }

  return {
    environment: {
      AGENTS_HOME: agentsHome,
      AGENT_FABRIC_STATE_DIRECTORY: stateDirectory,
      AGENT_FABRIC_RUNTIME_DIRECTORY: runtimeDirectory,
    },
    projectPath,
    stateDirectory,
    databasePath,
    identity: {
      projectSessionId: current.projectSessionId,
      sessionRevision: current.sessionRevision,
      sessionGeneration: current.sessionGeneration,
      runId: current.runId,
      runRevision: current.runRevision,
      chairAgentId: current.chairAgentId,
      chairGeneration: current.chairGeneration,
      chairLeaseId: current.chairLeaseId,
    },
    seatBindings: CURRENT_BINDINGS
      .map(({ seat, agentId, expectedPrincipalGeneration }) => `${seat}=${agentId}@${String(expectedPrincipalGeneration)}`)
      .join(","),
  };
}

type ProvisionOutput = {
  schemaVersion: 1;
  projectKey: string;
  projectPath: string;
  expectedPreviousGeneration: string | null;
  generation: string;
  projectSessionId: string;
  sessionRevision: number;
  sessionGeneration: number;
  runId: string;
  runRevision: number;
  chairAgentId: string;
  chairGeneration: number;
  chairLeaseId: string;
  chairSeat: string;
  expiresAt: string;
  seats: Array<{
    seat: string;
    role: "chair" | "peer";
    agentId: string;
    principalGeneration: number;
    credentialPath: string;
    metadataPath: string;
  }>;
};

function provisionArguments(fixture_: Fixture, expiresAt: string): string[] {
  const identity = fixture_.identity;
  return [
    "mcp", "provision",
    "--project", fixture_.projectPath,
    "--project-session-id", identity.projectSessionId,
    "--session-revision", String(identity.sessionRevision),
    "--session-generation", String(identity.sessionGeneration),
    "--run-id", identity.runId,
    "--run-revision", String(identity.runRevision),
    "--chair-seat", "codex",
    "--chair-agent-id", identity.chairAgentId,
    "--chair-generation", String(identity.chairGeneration),
    "--chair-lease-id", identity.chairLeaseId,
    "--seat-bindings", fixture_.seatBindings,
    "--expires-at", expiresAt,
  ];
}

type PersistenceCounts = {
  projects: number;
  project_sessions: number;
  runs: number;
  authorities: number;
  agents: number;
  capabilities: number;
};

function persistenceCounts(databasePath: string): PersistenceCounts {
  const database = new Database(databasePath, { readonly: true });
  try {
    const count = (table: keyof PersistenceCounts): number =>
      (database.prepare(`SELECT count(*) AS count FROM ${table}`).get() as { count: number }).count;
    return {
      projects: count("projects"),
      project_sessions: count("project_sessions"),
      runs: count("runs"),
      authorities: count("authorities"),
      agents: count("agents"),
      capabilities: count("capabilities"),
    };
  } finally {
    database.close();
  }
}

describe("MCP current project seat provisioning", () => {
  it("starts the on-demand daemon when provisioning begins from the healthy idle state", async () => {
    const current = await fixture({ daemonIdle: true });
    const result = await runSourceCli(
      provisionArguments(current, new Date(Date.now() + 14 * 24 * 60 * 60 * 1_000).toISOString()),
      { environment: current.environment },
    );

    expect(result.exitCode).toBe(0);
    expect(parseCliJson(result)).toMatchObject({ projectPath: current.projectPath });
    const discovery = JSON.parse(
      await readFile(join(current.stateDirectory, "..", "runtime", "fabric-v1.discovery.json"), "utf8"),
    ) as { pid: number };
    daemonPids.add(discovery.pid);
    trackTestProcess(discovery.pid, "mcp provision on-demand daemon");
  });

  it("idempotently binds private seat credentials without creating a project, run, authority or agent", async () => {
    const current = await fixture();
    const expiresAt = new Date(Date.now() + 30 * 24 * 60 * 60 * 1_000).toISOString();
    const arguments_ = provisionArguments(current, expiresAt);
    const before = persistenceCounts(current.databasePath);

    const firstResult = await runSourceCli(arguments_, { environment: current.environment });
    const first = parseCliJson(firstResult) as ProvisionOutput;
    expect(first).toMatchObject({
      schemaVersion: 1,
      projectPath: current.projectPath,
      expectedPreviousGeneration: null,
      ...current.identity,
      chairSeat: "codex",
      expiresAt,
      seats: [
        { seat: "agy", agentId: "agent_agy", role: "peer", principalGeneration: 1 },
        { seat: "claude", agentId: "agent_claude", role: "peer", principalGeneration: 1 },
        { seat: "codex", agentId: "agent_codex_chair", role: "chair", principalGeneration: 1 },
        { seat: "cursor", agentId: "agent_cursor", role: "peer", principalGeneration: 1 },
        { seat: "kiro", agentId: "agent_kiro", role: "peer", principalGeneration: 1 },
      ],
    });
    expect(firstResult.stdout).not.toMatch(/af[bc]_[A-Za-z0-9_-]+/u);
    expect(firstResult.stdout).not.toContain("capability");

    const credentials = await Promise.all(first.seats.map(async (seat) => {
      expect(seat.credentialPath).toMatch(new RegExp(`/seats/${first.projectKey}/generations/[0-9a-f]{64}/${seat.seat}\\.cap$`, "u"));
      const [credentialStat, metadataStat, credential, metadataText] = await Promise.all([
        lstat(seat.credentialPath),
        lstat(seat.metadataPath),
        readFile(seat.credentialPath, "utf8"),
        readFile(seat.metadataPath, "utf8"),
      ]);
      expect(credentialStat.isFile()).toBe(true);
      expect(credentialStat.isSymbolicLink()).toBe(false);
      expect(credentialStat.mode & 0o777).toBe(0o600);
      expect(metadataStat.isFile()).toBe(true);
      expect(metadataStat.isSymbolicLink()).toBe(false);
      expect(metadataStat.mode & 0o777).toBe(0o600);
      expect(credential).toMatch(/^afc_[A-Za-z0-9_-]{43}$/u);
      expect(metadataText).not.toMatch(/afc_[A-Za-z0-9_-]+/u);
      expect(JSON.parse(metadataText)).toMatchObject({
        schemaVersion: 1,
        projectKey: first.projectKey,
        projectPath: current.projectPath,
        generation: first.generation,
        previousGeneration: null,
        ...current.identity,
        seat: seat.seat,
        agentId: seat.agentId,
        principalGeneration: 1,
        role: seat.role,
        credentialPath: seat.credentialPath,
        expiresAt,
      });
      return credential;
    }));
    expect(new Set(credentials).size).toBe(5);
    expect((await lstat(join(current.stateDirectory, "seats"))).mode & 0o777).toBe(0o700);
    expect((await lstat(join(current.stateDirectory, "seats", first.projectKey))).mode & 0o777).toBe(0o700);

    const afterFirst = persistenceCounts(current.databasePath);
    expect(afterFirst).toEqual({ ...before, capabilities: before.capabilities + 5 });
    const secondResult = await runSourceCli(arguments_, { environment: current.environment });
    const second = parseCliJson(secondResult) as ProvisionOutput;
    expect(second).toEqual(first);
    expect(persistenceCounts(current.databasePath)).toEqual(afterFirst);
    await expect(Promise.all(second.seats.map(async (seat) => readFile(seat.credentialPath, "utf8"))))
      .resolves.toEqual(credentials);

    const seatPathResult = await runSourceCli(
      ["mcp", "seat-path", "--project", current.projectPath, "--seat", "claude"],
      { environment: current.environment },
    );
    expect(parseCliJson(seatPathResult)).toEqual({
      schemaVersion: 1,
      projectKey: first.projectKey,
      generation: first.generation,
      seat: "claude",
      credentialPath: first.seats[1]?.credentialPath,
      metadataPath: first.seats[1]?.metadataPath,
    });
  });

  it("rejects stale exact identity and leaves all authority state untouched", async () => {
    const current = await fixture();
    const expiresAt = new Date(Date.now() + 14 * 24 * 60 * 60 * 1_000).toISOString();
    const arguments_ = provisionArguments(current, expiresAt);
    const revisionIndex = arguments_.indexOf("--run-revision") + 1;
    arguments_[revisionIndex] = "2";
    const before = persistenceCounts(current.databasePath);

    const result = await runSourceCli(arguments_, { environment: current.environment });
    expect(result.exitCode).toBe(1);
    expect(result.stderr).toMatch(/stale or crossed/u);
    expect(result.stderr).not.toMatch(/afc_[A-Za-z0-9_-]+/u);
    expect(persistenceCounts(current.databasePath)).toEqual(before);
  });

  it("refuses to replace a symlinked credential", async () => {
    const current = await fixture();
    const arguments_ = provisionArguments(
      current,
      new Date(Date.now() + 30 * 24 * 60 * 60 * 1_000).toISOString(),
    );
    const provisioned = parseCliJson(await runSourceCli(arguments_, { environment: current.environment })) as ProvisionOutput;
    const target = join(current.projectPath, "must-not-change");
    await writeFile(target, "safe");
    const credentialPath = provisioned.seats[0]?.credentialPath;
    if (credentialPath === undefined) throw new Error("test fixture did not return the agy credential path");
    await unlink(credentialPath);
    await symlink(target, credentialPath);

    const result = await runSourceCli(arguments_, { environment: current.environment });
    expect(result.exitCode).toBe(1);
    expect(result.stderr).toMatch(/private regular file|symbolic link/u);
    expect(result.stderr).not.toMatch(/afc_[A-Za-z0-9_-]+/u);
    await expect(readFile(target, "utf8")).resolves.toBe("safe");
  });

  it("renews credentials on the same explicit run and rejects a one-seat binding", async () => {
    const current = await fixture();
    const firstExpiry = new Date(Date.now() + 14 * 24 * 60 * 60 * 1_000).toISOString();
    const secondExpiry = new Date(Date.now() + 30 * 24 * 60 * 60 * 1_000).toISOString();
    const first = parseCliJson(await runSourceCli(provisionArguments(current, firstExpiry), {
      environment: current.environment,
    })) as ProvisionOutput;
    const firstCredentials = await Promise.all(first.seats.map(async (seat) => await readFile(seat.credentialPath, "utf8")));
    const second = parseCliJson(await runSourceCli(provisionArguments(current, secondExpiry), {
      environment: current.environment,
    })) as ProvisionOutput;
    expect(second.runId).toBe(first.runId);
    expect(second.seats.map((seat) => seat.credentialPath)).not.toEqual(first.seats.map((seat) => seat.credentialPath));
    expect(persistenceCounts(current.databasePath).runs).toBe(1);
    const database = new Database(current.databasePath);
    try {
      expect(database.prepare(`
        SELECT active.generation
          FROM mcp_active_seat_generations active
          JOIN projects project ON project.project_id=active.project_id
         WHERE project.canonical_root=?
      `).get(current.projectPath)).toEqual({ generation: second.generation });
      expect(database.prepare(`
        SELECT count(*) AS count
          FROM mcp_seat_generation_members member
          JOIN capabilities capability ON capability.token_hash=member.token_hash
         WHERE member.generation=? AND capability.revoked_at IS NOT NULL
      `).get(first.generation)).toEqual({ count: CURRENT_BINDINGS.length });
      expect(() => database.prepare(`
        UPDATE mcp_active_seat_generations SET generation=? WHERE generation=?
      `).run(first.generation, second.generation)).toThrow(/INVARIANT_mcp_active_seat_generation_forward_only/u);
    } finally {
      database.close();
    }
    const discovery = JSON.parse(await readFile(join(current.environment.AGENT_FABRIC_RUNTIME_DIRECTORY ?? "", "fabric-v1.discovery.json"), "utf8")) as {
      socketPath: string;
      bootstrapCapability: string;
    };
    for (const credential of firstCredentials) {
      const client = await connectFabricDaemon({ socketPath: discovery.socketPath, capability: credential });
      await expect(client.getMailboxState())
        .rejects.toThrow(/expired or revoked|authentication failed/iu);
      await client.close();
    }
    const staleCredential = firstCredentials[0];
    if (staleCredential === undefined) throw new Error("first generation did not contain a credential");
    const staleHash = createHash("sha256").update(staleCredential).digest("hex");
    const corrupted = new Database(current.databasePath);
    try {
      corrupted.prepare("UPDATE capabilities SET revoked_at=NULL WHERE token_hash=?").run(staleHash);
    } finally {
      corrupted.close();
    }
    const staleClient = await connectFabricDaemon({ socketPath: discovery.socketPath, capability: staleCredential });
    await expect(staleClient.getMailboxState()).rejects.toThrow(/inactive MCP seat generation/iu);
    await staleClient.close();
    const repaired = new Database(current.databasePath);
    try {
      repaired.prepare("UPDATE capabilities SET revoked_at=? WHERE token_hash=?").run(Date.now(), staleHash);
    } finally {
      repaired.close();
    }
    for (const seat of second.seats) {
      const client = await connectFabricDaemon({
        socketPath: discovery.socketPath,
        capability: await readFile(seat.credentialPath, "utf8"),
      });
      await expect(client.getMailboxState()).resolves.toMatchObject({ contiguousWatermark: 0 });
      await client.close();
    }
    const currentSeat = second.seats[0];
    if (currentSeat === undefined) throw new Error("second generation did not contain a credential");
    const currentCredential = await readFile(currentSeat.credentialPath, "utf8");
    const topology = new Database(current.databasePath);
    try {
      topology.prepare("UPDATE run_chair_leases SET status='frozen' WHERE lease_id=?")
        .run(current.identity.chairLeaseId);
    } finally {
      topology.close();
    }
    const topologyStaleClient = await connectFabricDaemon({
      socketPath: discovery.socketPath,
      capability: currentCredential,
    });
    await expect(topologyStaleClient.getMailboxState()).rejects.toThrow(/inactive MCP seat generation/iu);
    await topologyStaleClient.close();
    const restoredTopology = new Database(current.databasePath);
    try {
      restoredTopology.prepare("UPDATE run_chair_leases SET status='active' WHERE lease_id=?")
        .run(current.identity.chairLeaseId);
    } finally {
      restoredTopology.close();
    }
    const control = await connectFabricDaemon({
      socketPath: discovery.socketPath,
      capability: discovery.bootstrapCapability,
    });
    try {
      await expect(control.bindCurrentMcpSeats({
        canonicalRoot: current.projectPath,
        expectedPreviousGeneration: first.expectedPreviousGeneration,
        generation: first.generation,
        projectSessionId: first.projectSessionId,
        expectedSessionRevision: first.sessionRevision,
        expectedSessionGeneration: first.sessionGeneration,
        runId: first.runId,
        expectedRunRevision: first.runRevision,
        chairAgentId: first.chairAgentId,
        expectedChairGeneration: first.chairGeneration,
        chairLeaseId: first.chairLeaseId,
        expiresAt: first.expiresAt,
        bindings: CURRENT_BINDINGS.map((binding) => ({ ...binding })),
      })).rejects.toThrow(/replay is stale|crossed or changed/iu);

      const thirdExpiry = new Date(Date.now() + 29 * 24 * 60 * 60 * 1_000).toISOString();
      const third = currentMcpSeatGeneration({
        canonicalRoot: current.projectPath,
        projectSessionId: current.identity.projectSessionId,
        sessionRevision: current.identity.sessionRevision,
        sessionGeneration: current.identity.sessionGeneration,
        runId: current.identity.runId,
        runRevision: current.identity.runRevision,
        chairAgentId: current.identity.chairAgentId,
        chairGeneration: current.identity.chairGeneration,
        chairLeaseId: current.identity.chairLeaseId,
        expiresAt: thirdExpiry,
        bindings: CURRENT_BINDINGS,
      });
      await expect(control.bindCurrentMcpSeats({
        canonicalRoot: current.projectPath,
        expectedPreviousGeneration: first.generation,
        generation: third.generation,
        projectSessionId: current.identity.projectSessionId,
        expectedSessionRevision: current.identity.sessionRevision,
        expectedSessionGeneration: current.identity.sessionGeneration,
        runId: current.identity.runId,
        expectedRunRevision: current.identity.runRevision,
        chairAgentId: current.identity.chairAgentId,
        expectedChairGeneration: current.identity.chairGeneration,
        chairLeaseId: current.identity.chairLeaseId,
        expiresAt: thirdExpiry,
        bindings: CURRENT_BINDINGS.map((binding) => ({ ...binding })),
      })).rejects.toThrow(/active MCP seat generation changed/iu);
    } finally {
      await control.close();
    }

    const invalidArguments = provisionArguments(current, secondExpiry);
    const bindingsIndex = invalidArguments.indexOf("--seat-bindings") + 1;
    invalidArguments[bindingsIndex] = "codex=agent_codex_chair@1";
    const invalid = await runSourceCli(invalidArguments, { environment: current.environment });
    expect(invalid.exitCode).toBe(1);
    expect(invalid.stderr).toMatch(/at least two distinct seat bindings/u);
  });

  it("binds only the requested current project under a broader trusted root", async () => {
    const current = await fixture({ broaderRoot: true });
    const result = parseCliJson(await runSourceCli(
      provisionArguments(current, new Date(Date.now() + 14 * 24 * 60 * 60 * 1_000).toISOString()),
      { environment: current.environment },
    )) as ProvisionOutput;
    expect(result.projectPath).toBe(current.projectPath);
    expect(result.runId).toBe(current.identity.runId);
    const database = new Database(current.databasePath, { readonly: true });
    try {
      expect(database.prepare("SELECT workspace_root FROM runs WHERE run_id = ?").pluck().get(result.runId))
        .toBe(current.projectPath);
    } finally {
      database.close();
    }

    const siblingProject = join(dirname(current.projectPath), "two");
    await mkdir(siblingProject);
    const crossedArguments = provisionArguments(
      current,
      new Date(Date.now() + 13 * 24 * 60 * 60 * 1_000).toISOString(),
    );
    crossedArguments[crossedArguments.indexOf("--project") + 1] = siblingProject;
    const crossed = await runSourceCli(crossedArguments, { environment: current.environment });
    expect(crossed.exitCode).toBe(1);
    expect(crossed.stderr).toMatch(/stale or crossed/u);
  });
});
