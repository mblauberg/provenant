import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFile, lstat, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import { MCP_BOOTSTRAP_CREDENTIALS_FEATURE } from "@local/agent-fabric-protocol";
import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { currentMcpSeatGeneration } from "../../../src/core/mcp-seat-generation.ts";
import { connectFabricDaemon } from "../../../src/daemon/client.ts";
import { shellCommandArguments } from "../../support/shell-command-arguments.ts";
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
      expectedPreviousGeneration: string | null;
      generation: string;
      projectSessionId: string;
      sessionRevision: number;
      sessionGeneration: number;
      runId: string;
      runRevision: number;
      chairSeat: string;
      chairAgentId: string;
      chairGeneration: number;
      chairLeaseId: string;
      expiresAt: string;
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
    expect(Object.fromEntries(await Promise.all(first.seats.map(async ({ seat, metadataPath }) => [
      seat,
      (JSON.parse(await readFile(metadataPath, "utf8")) as { originKind: string }).originKind,
    ] as const)))).toEqual({ agy: "provisioned", codex: "bootstrap" });
    await trackCurrentDaemon(value);
    const afterFirst = counts(value.databasePath);

    const secondResult = await cli(value, [
      "mcp", "peer-provision", "--project", value.project, "--seat", "agy",
    ]);
    expect(JSON.parse(secondResult.stdout)).toEqual(JSON.parse(firstResult.stdout));
    expect(counts(value.databasePath)).toEqual(afterFirst);
    expect(secondResult.stdout).not.toMatch(/af[bc]_[A-Za-z0-9_-]{43}/u);

    const exactProvisionReplay = await cli(value, [
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
      "--expires-at", first.expiresAt,
    ]);
    expect(JSON.parse(exactProvisionReplay.stdout)).toEqual(JSON.parse(firstResult.stdout));
    expect(counts(value.databasePath)).toEqual(afterFirst);

    const provisionedExpiry = new Date(Date.now() + 20 * 60 * 1_000).toISOString();
    const provisionedResult = await cli(value, [
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
      "--expires-at", provisionedExpiry,
    ]);
    const provisioned = JSON.parse(provisionedResult.stdout) as typeof first;
    expect(Object.fromEntries(await Promise.all(provisioned.seats.map(async ({ seat, metadataPath }) => [
      seat,
      (JSON.parse(await readFile(metadataPath, "utf8")) as { originKind: string }).originKind,
    ] as const)))).toEqual({ agy: "provisioned", codex: "provisioned" });

    const renewalExpiry = new Date(Date.now() + 30 * 60 * 1_000).toISOString();
    const renewalResult = await cli(value, [
      "mcp", "peer-provision",
      "--project", value.project,
      "--seat", "agy",
      "--expires-at", renewalExpiry,
    ]);
    const renewal = JSON.parse(renewalResult.stdout) as {
      expectedPreviousGeneration: string | null;
      generation: string;
      expiresAt: string;
      seats: Array<{
        seat: string;
        agentId: string;
        principalGeneration: number;
        metadataPath: string;
      }>;
    };
    expect(renewal).toMatchObject({
      expectedPreviousGeneration: provisioned.generation,
      expiresAt: renewalExpiry,
      seats: provisioned.seats.map(({ seat, agentId, principalGeneration }) => ({
        seat,
        agentId,
        principalGeneration,
      })),
    });
    expect(renewal.generation).not.toBe(first.generation);
    expect(Object.fromEntries(await Promise.all(renewal.seats.map(async ({ seat, metadataPath }) => [
      seat,
      (JSON.parse(await readFile(metadataPath, "utf8")) as { originKind: string }).originKind,
    ] as const)))).toEqual({ agy: "provisioned", codex: "provisioned" });
    expect(renewalResult.stdout).not.toMatch(/af[bc]_[A-Za-z0-9_-]{43}/u);

    const database = new Database(value.databasePath, { readonly: true, fileMustExist: true });
    try {
      expect(database.prepare(`
        SELECT previous_generation
          FROM mcp_seat_generations
         WHERE generation=?
      `).get(renewal.generation)).toEqual({ previous_generation: provisioned.generation });
      expect(database.prepare(`
        SELECT count(*) AS count
          FROM capabilities
         WHERE revoked_at IS NOT NULL
           AND token_hash IN (
             SELECT token_hash FROM mcp_seat_generation_members WHERE generation=?
           )
      `).get(provisioned.generation)).toEqual({ count: provisioned.seats.length });
    } finally {
      database.close();
    }
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

  it("recovers an expired provisioned roster with the advertised renewal command", async () => {
    const value = await fixture();
    await cli(value, ["workspace", "trust", value.project]);
    await cli(value, ["bootstrap", "--seat", "codex"]);
    const bootstrapPid = await trackCurrentDaemon(value);
    await terminateTrackedTestProcess(bootstrapPid);
    daemonPids.delete(bootstrapPid);

    const rosterResult = await cli(value, [
      "mcp", "peer-provision", "--project", value.project, "--seat", "agy",
    ]);
    const roster = JSON.parse(rosterResult.stdout) as {
      generation: string;
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
        agentId: string;
        principalGeneration: number;
        credentialPath: string;
        metadataPath: string;
      }>;
    };
    // Convert the whole roster to provisioned origins, the shape that renews
    // manually rather than through bootstrap interception.
    const provisionedResult = await cli(value, [
      "mcp", "provision",
      "--project", value.project,
      "--project-session-id", roster.projectSessionId,
      "--session-revision", String(roster.sessionRevision),
      "--session-generation", String(roster.sessionGeneration),
      "--run-id", roster.runId,
      "--run-revision", String(roster.runRevision),
      "--chair-seat", roster.chairSeat,
      "--chair-agent-id", roster.chairAgentId,
      "--chair-generation", String(roster.chairGeneration),
      "--chair-lease-id", roster.chairLeaseId,
      "--seat-bindings", roster.seats
        .map(({ seat, agentId, principalGeneration }) => `${seat}=${agentId}@${String(principalGeneration)}`)
        .join(","),
      "--expires-at", new Date(Date.now() + 20 * 60 * 1_000).toISOString(),
    ]);
    const provisioned = JSON.parse(provisionedResult.stdout) as typeof roster;
    await trackCurrentDaemon(value);

    // Lapse the roster exactly as time would: the seat metadata, the
    // generation record and the generation's seat credentials all move into
    // the past. The agents' underlying capabilities outlive the roster, as in
    // a real provisioned run, so the material for recovery stays live.
    const expiredAt = new Date(Date.now() - 60 * 60 * 1_000).toISOString();
    for (const seat of provisioned.seats) {
      const metadata = JSON.parse(await readFile(seat.metadataPath, "utf8")) as Record<string, unknown>;
      metadata.expiresAt = expiredAt;
      await writeFile(seat.metadataPath, `${JSON.stringify(metadata, null, 2)}\n`, { mode: 0o600 });
    }
    const database = new Database(value.databasePath);
    try {
      const expiredMs = Date.parse(expiredAt);
      // The generation records themselves are immutable by trigger and are
      // never consulted for liveness; the operative expiry surfaces are the
      // seat metadata files above and the credential rows here.
      database.prepare(`
        UPDATE capabilities SET expires_at=?
         WHERE token_hash IN (SELECT token_hash FROM mcp_seat_generation_members WHERE generation=?)
      `).run(expiredMs, provisioned.generation);
      for (const seat of provisioned.seats) {
        database.prepare(`
          INSERT INTO capabilities(token_hash, run_id, agent_id, principal_generation, expires_at)
          VALUES (?, ?, ?, ?, ?)
        `).run(
          createHash("sha256").update(`recovery-fixture-${seat.seat}`).digest("hex"),
          roster.runId,
          seat.agentId,
          seat.principalGeneration,
          Date.now() + 24 * 60 * 60 * 1_000,
        );
      }
    } finally {
      database.close();
    }

    // The bare request is no longer a dead end: it names the exact recovery
    // command, and that command recovers the roster in place.
    const refusal = await cliFailure(value, [
      "mcp", "peer-provision", "--project", value.project, "--seat", "agy",
    ]);
    const advertised = refusal.stderr.match(/can be recovered with (.+)$/mu)?.[1];
    expect(advertised).toBeDefined();
    const recoveryArguments = await shellCommandArguments(String(advertised), value.stateDirectory);
    expect(recoveryArguments.slice(0, 2)).toEqual(["mcp", "peer-provision"]);
    expect(recoveryArguments).toContain("--expires-at");

    const recoveredResult = await cli(value, recoveryArguments);
    const recovered = JSON.parse(recoveredResult.stdout) as typeof roster & { expiresAt: string };
    await trackCurrentDaemon(value);
    expect(recovered).toMatchObject({
      expectedPreviousGeneration: provisioned.generation,
      seats: provisioned.seats.map(({ seat, agentId, principalGeneration }) => ({
        seat,
        agentId,
        principalGeneration,
      })),
    });
    expect(recovered.generation).not.toBe(provisioned.generation);
    expect(Date.parse(recovered.expiresAt)).toBeGreaterThan(Date.now());
    expect(recoveredResult.stdout).not.toMatch(/af[bc]_[A-Za-z0-9_-]{43}/u);

    const verify = new Database(value.databasePath, { readonly: true, fileMustExist: true });
    try {
      expect(verify.prepare(`
        SELECT previous_generation FROM mcp_seat_generations WHERE generation=?
      `).get(recovered.generation)).toEqual({ previous_generation: provisioned.generation });
      expect(verify.prepare("SELECT generation FROM mcp_active_seat_generations").get())
        .toEqual({ generation: recovered.generation });
    } finally {
      verify.close();
    }

    // The recovered roster behaves like any healthy roster again, and the
    // replay is byte-identical on disk: the installed credential and metadata
    // files carry exactly the same bytes afterwards, not merely an equivalent
    // parsed shape.
    const seatFileBytes = async (): Promise<string[]> => Promise.all(
      recovered.seats.flatMap(({ credentialPath, metadataPath }) => [
        readFile(credentialPath).then((bytes) => bytes.toString("hex")),
        readFile(metadataPath).then((bytes) => bytes.toString("hex")),
      ]),
    );
    const bytesBeforeReplay = await seatFileBytes();
    const replay = await cli(value, [
      "mcp", "peer-provision", "--project", value.project, "--seat", "agy",
    ]);
    expect(JSON.parse(replay.stdout)).toEqual(JSON.parse(recoveredResult.stdout));
    expect(await seatFileBytes()).toEqual(bytesBeforeReplay);
  });

  it("rejects a live non-bootstrap capability on the expired-roster rebind gate", async () => {
    const value = await fixture();
    await cli(value, ["workspace", "trust", value.project]);
    await cli(value, ["bootstrap", "--seat", "codex"]);
    const bootstrapPid = await trackCurrentDaemon(value);
    await terminateTrackedTestProcess(bootstrapPid);
    daemonPids.delete(bootstrapPid);

    const rosterResult = await cli(value, [
      "mcp", "peer-provision", "--project", value.project, "--seat", "agy",
    ]);
    const roster = JSON.parse(rosterResult.stdout) as {
      generation: string;
      projectPath: string;
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
        agentId: string;
        principalGeneration: number;
        metadataPath: string;
      }>;
    };
    const provisionedResult = await cli(value, [
      "mcp", "provision",
      "--project", value.project,
      "--project-session-id", roster.projectSessionId,
      "--session-revision", String(roster.sessionRevision),
      "--session-generation", String(roster.sessionGeneration),
      "--run-id", roster.runId,
      "--run-revision", String(roster.runRevision),
      "--chair-seat", roster.chairSeat,
      "--chair-agent-id", roster.chairAgentId,
      "--chair-generation", String(roster.chairGeneration),
      "--chair-lease-id", roster.chairLeaseId,
      "--seat-bindings", roster.seats
        .map(({ seat, agentId, principalGeneration }) => `${seat}=${agentId}@${String(principalGeneration)}`)
        .join(","),
      "--expires-at", new Date(Date.now() + 20 * 60 * 1_000).toISOString(),
    ]);
    const provisioned = JSON.parse(provisionedResult.stdout) as typeof roster;
    await trackCurrentDaemon(value);

    // Lapse the roster exactly as in the recovery test, but mint the live
    // replacement capabilities from raw tokens this test knows, so one of them
    // can be presented to the daemon directly.
    const expiredAt = new Date(Date.now() - 60 * 60 * 1_000).toISOString();
    for (const seat of provisioned.seats) {
      const metadata = JSON.parse(await readFile(seat.metadataPath, "utf8")) as Record<string, unknown>;
      metadata.expiresAt = expiredAt;
      await writeFile(seat.metadataPath, `${JSON.stringify(metadata, null, 2)}\n`, { mode: 0o600 });
    }
    const liveTokens = new Map(provisioned.seats.map(({ seat }) => [
      seat,
      `afc_${createHash("sha256").update(`live-non-bootstrap-${seat}`).digest("base64url").slice(0, 43)}`,
    ] as const));
    const identityDatabase = new Database(value.databasePath);
    let liveIdentity: { sessionRevision: number; sessionGeneration: number; runRevision: number; chairGeneration: number; chairLeaseId: string };
    try {
      identityDatabase.prepare(`
        UPDATE capabilities SET expires_at=?
         WHERE token_hash IN (SELECT token_hash FROM mcp_seat_generation_members WHERE generation=?)
      `).run(Date.parse(expiredAt), provisioned.generation);
      for (const seat of provisioned.seats) {
        identityDatabase.prepare(`
          INSERT INTO capabilities(token_hash, run_id, agent_id, principal_generation, expires_at)
          VALUES (?, ?, ?, ?, ?)
        `).run(
          createHash("sha256").update(String(liveTokens.get(seat.seat))).digest("hex"),
          roster.runId,
          seat.agentId,
          seat.principalGeneration,
          Date.now() + 24 * 60 * 60 * 1_000,
        );
      }
      const live = identityDatabase.prepare(`
        SELECT session.revision AS session_revision, session.generation AS session_generation,
               run.revision AS run_revision, run.chair_generation, run.chair_lease_id
          FROM project_sessions session
          JOIN runs run ON run.project_session_id=session.project_session_id
         WHERE session.project_session_id=? AND run.run_id=?
      `).get(roster.projectSessionId, roster.runId) as {
        session_revision: number;
        session_generation: number;
        run_revision: number;
        chair_generation: number;
        chair_lease_id: string;
      };
      liveIdentity = {
        sessionRevision: live.session_revision,
        sessionGeneration: live.session_generation,
        runRevision: live.run_revision,
        chairGeneration: live.chair_generation,
        chairLeaseId: live.chair_lease_id,
      };
    } finally {
      identityDatabase.close();
    }

    // Build the exact expired-roster rebind request the recovery path would
    // send, so the daemon's capability gate is the only thing that can refuse
    // it: same identity, same bindings, a valid future expiry, and the
    // generation derived the same way the daemon re-derives it.
    const expiresAt = new Date(Date.now() + 30 * 60 * 1_000).toISOString();
    const bindings = provisioned.seats
      .map(({ seat, agentId, principalGeneration }) => ({
        seat,
        agentId,
        expectedPrincipalGeneration: principalGeneration,
      }))
      .sort((left, right) => left.seat.localeCompare(right.seat));
    const rebindInput = {
      canonicalRoot: provisioned.projectPath,
      expectedPreviousGeneration: provisioned.generation,
      generation: currentMcpSeatGeneration({
        canonicalRoot: provisioned.projectPath,
        projectSessionId: roster.projectSessionId,
        sessionRevision: liveIdentity.sessionRevision,
        sessionGeneration: liveIdentity.sessionGeneration,
        runId: roster.runId,
        runRevision: liveIdentity.runRevision,
        chairAgentId: roster.chairAgentId,
        chairGeneration: liveIdentity.chairGeneration,
        chairLeaseId: liveIdentity.chairLeaseId,
        expiresAt,
        bindings,
      }).generation,
      projectSessionId: roster.projectSessionId,
      expectedSessionRevision: liveIdentity.sessionRevision,
      expectedSessionGeneration: liveIdentity.sessionGeneration,
      runId: roster.runId,
      expectedRunRevision: liveIdentity.runRevision,
      chairAgentId: roster.chairAgentId,
      expectedChairGeneration: liveIdentity.chairGeneration,
      chairLeaseId: liveIdentity.chairLeaseId,
      expiresAt,
      bindings,
    };
    const receipt = JSON.parse(
      await readFile(join(value.runtimeDirectory, "fabric-v1.discovery.json"), "utf8"),
    ) as { socketPath: string; bootstrapCapability: string };

    // A live, unexpired, provisioned peer capability is still not the
    // bootstrap capability, so the daemon dispatch gate must refuse to route
    // bindCurrentMcpSeats for it (src/daemon/process.ts confines the method to
    // the bootstrap branch; every other capability falls through to the client
    // dispatcher, which does not carry the method at all).
    const peerClient = await connectFabricDaemon({
      socketPath: receipt.socketPath,
      capability: String(liveTokens.get("agy")),
    });
    try {
      await expect(peerClient.bindCurrentMcpSeats(rebindInput))
        .rejects.toThrow(/unsupported daemon method bindCurrentMcpSeats/u);
    } finally {
      await peerClient.close();
    }
    const afterRefusal = new Database(value.databasePath, { readonly: true, fileMustExist: true });
    try {
      expect(afterRefusal.prepare("SELECT generation FROM mcp_active_seat_generations").get())
        .toEqual({ generation: provisioned.generation });
      expect(afterRefusal.prepare(
        "SELECT count(*) AS count FROM mcp_seat_generations WHERE generation=?",
      ).get(rebindInput.generation)).toEqual({ count: 0 });
    } finally {
      afterRefusal.close();
    }

    // Positive control with the identical request: only the presented
    // capability changes, and the bootstrap capability is admitted. This pins
    // the refusal above on the daemon's capability gate rather than on any
    // defect in the request, so removing the gate makes this test fail.
    const bootstrapClient = await connectFabricDaemon({
      socketPath: receipt.socketPath,
      capability: receipt.bootstrapCapability,
      requiredCapabilities: [MCP_BOOTSTRAP_CREDENTIALS_FEATURE],
    });
    try {
      const bound = await bootstrapClient.bindCurrentMcpSeats(rebindInput);
      expect(bound.generation).toBe(rebindInput.generation);
      expect(bound.expectedPreviousGeneration).toBe(provisioned.generation);
    } finally {
      await bootstrapClient.close();
    }
    const afterRebind = new Database(value.databasePath, { readonly: true, fileMustExist: true });
    try {
      expect(afterRebind.prepare("SELECT generation FROM mcp_active_seat_generations").get())
        .toEqual({ generation: rebindInput.generation });
    } finally {
      afterRebind.close();
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
