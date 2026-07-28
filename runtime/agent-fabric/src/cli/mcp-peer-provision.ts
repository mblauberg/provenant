import { createHash } from "node:crypto";
import Database from "better-sqlite3";
import { constants } from "node:fs";
import { lstat, open } from "node:fs/promises";

import { readStoredAuthority } from "../authority/stored-authority.js";
import { connectFabricDaemon } from "../daemon/client.js";
import type { AuthorityInput } from "../domain/types.js";
import {
  bindProvisionedSeatRoster,
  startMcpProvisionDaemon,
  type McpProvisionOutput,
  type ParsedSeatBinding,
} from "./mcp-provision.js";
import { peerSeatAuthority } from "./observer-provision.js";
import type { FabricPaths } from "./paths.js";
import {
  MCP_SEATS,
  parseMcpSeat,
  resolveSeatPaths,
  type McpSeat,
  type SeatMetadata,
} from "./seat-store.js";

const MAXIMUM_SEAT_LIFETIME_MS = 31 * 24 * 60 * 60 * 1_000;
const ROSTER_CONVERGENCE_TIMEOUT_MS = 5_000;
const ROSTER_CONVERGENCE_POLL_MS = 25;

type InstalledSeat = {
  metadata: SeatMetadata;
  credentialPath: string;
  metadataPath: string;
};

export class McpPeerProvisionChairRequiredError extends Error {
  readonly code = "MCP_CHAIR_SEAT_REQUIRED" as const;

  constructor(project: string) {
    super(
      `MCP peer provisioning requires an active chair seat for ${project}; ` +
      `run agent-fabric bootstrap --seat claude or agent-fabric bootstrap --seat codex from that project`,
    );
    this.name = "McpPeerProvisionChairRequiredError";
  }
}

function errorCode(error: unknown): string | undefined {
  return typeof error === "object" && error !== null && "code" in error && typeof error.code === "string"
    ? error.code
    : undefined;
}

async function privateRead(path: string): Promise<string> {
  const before = await lstat(path);
  if (!before.isFile() || before.isSymbolicLink() || (before.mode & 0o077) !== 0) {
    throw new Error("peer provision source must be a private regular file");
  }
  const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try {
    return await handle.readFile("utf8");
  } finally {
    await handle.close();
  }
}

function seatMetadata(value: unknown): SeatMetadata {
  if (
    typeof value !== "object" || value === null || Array.isArray(value) ||
    !("schemaVersion" in value) || value.schemaVersion !== 1 ||
    !("projectKey" in value) || typeof value.projectKey !== "string" ||
    !("projectPath" in value) || typeof value.projectPath !== "string" ||
    !("generation" in value) || typeof value.generation !== "string" ||
    !("previousGeneration" in value) ||
    !("projectSessionId" in value) || typeof value.projectSessionId !== "string" ||
    !("sessionRevision" in value) || typeof value.sessionRevision !== "number" ||
    !("sessionGeneration" in value) || typeof value.sessionGeneration !== "number" ||
    !("runId" in value) || typeof value.runId !== "string" ||
    !("runRevision" in value) || typeof value.runRevision !== "number" ||
    !("chairAgentId" in value) || typeof value.chairAgentId !== "string" ||
    !("chairGeneration" in value) || typeof value.chairGeneration !== "number" ||
    !("chairLeaseId" in value) || typeof value.chairLeaseId !== "string" ||
    !("seat" in value) || typeof value.seat !== "string" ||
    !("agentId" in value) || typeof value.agentId !== "string" ||
    !("principalGeneration" in value) || typeof value.principalGeneration !== "number" ||
    !("role" in value) || (value.role !== "chair" && value.role !== "peer") ||
    !("credentialPath" in value) || typeof value.credentialPath !== "string" ||
    !("expiresAt" in value) || typeof value.expiresAt !== "string"
  ) {
    throw new Error("MCP seat metadata is invalid");
  }
  return value as SeatMetadata;
}

async function installedRoster(paths: FabricPaths, project: string): Promise<InstalledSeat[]> {
  const roster: InstalledSeat[] = [];
  for (const seat of MCP_SEATS) {
    try {
      const location = await resolveSeatPaths({
        stateDirectory: paths.stateDirectory,
        project,
        seat,
      });
      const metadata = seatMetadata(JSON.parse(await privateRead(location.metadataPath)));
      if (
        metadata.seat !== seat ||
        metadata.credentialPath !== location.credentialPath ||
        metadata.generation !== location.generation
      ) {
        throw new Error(`MCP seat metadata does not match the active ${seat} path`);
      }
      roster.push({
        metadata,
        credentialPath: location.credentialPath,
        metadataPath: location.metadataPath,
      });
    } catch (error: unknown) {
      if (errorCode(error) !== "ENOENT") throw error;
    }
  }
  return roster.sort((left, right) => left.metadata.seat.localeCompare(right.metadata.seat));
}

function rosterOutput(chair: InstalledSeat, roster: InstalledSeat[]): McpProvisionOutput {
  const metadata = chair.metadata;
  return {
    schemaVersion: 1,
    projectKey: metadata.projectKey,
    projectPath: metadata.projectPath,
    expectedPreviousGeneration: metadata.previousGeneration,
    generation: metadata.generation,
    projectSessionId: metadata.projectSessionId,
    sessionRevision: metadata.sessionRevision,
    sessionGeneration: metadata.sessionGeneration,
    runId: metadata.runId,
    runRevision: metadata.runRevision,
    chairAgentId: metadata.chairAgentId,
    chairGeneration: metadata.chairGeneration,
    chairLeaseId: metadata.chairLeaseId,
    chairSeat: metadata.seat,
    expiresAt: metadata.expiresAt,
    seats: roster.map((member) => ({
      seat: member.metadata.seat,
      role: member.metadata.role,
      agentId: member.metadata.agentId,
      principalGeneration: member.metadata.principalGeneration,
      credentialPath: member.credentialPath,
      metadataPath: member.metadataPath,
    })),
  };
}

function peerExpiry(requested: string | undefined, parent: AuthorityInput, currentExpiresAt: string): string {
  const value = requested ?? currentExpiresAt;
  const expiresAt = Date.parse(value);
  const now = Date.now();
  if (!Number.isFinite(expiresAt) || new Date(expiresAt).toISOString() !== value) {
    throw new Error("mcp peer-provision --expires-at must be an ISO timestamp");
  }
  if (
    expiresAt <= now ||
    expiresAt - now > MAXIMUM_SEAT_LIFETIME_MS ||
    expiresAt > Date.parse(parent.expiresAt)
  ) {
    throw new Error(
      "mcp peer-provision --expires-at must be in the future, no more than 31 days away, and not outlive the chair authority",
    );
  }
  return value;
}

function peerAgentId(projectKey: string, runId: string, seat: McpSeat): string {
  const digest = createHash("sha256")
    .update(JSON.stringify({ projectKey, runId, seat }))
    .digest("hex")
    .slice(0, 16);
  return `${seat}_bootstrap_peer_${digest}`;
}

function rosterCasChanged(error: unknown): boolean {
  return error instanceof Error && /active MCP seat generation changed/u.test(error.message);
}

async function waitForRosterConvergence(): Promise<void> {
  await new Promise<void>((resolve) => setTimeout(resolve, ROSTER_CONVERGENCE_POLL_MS));
}

function parseArguments(arguments_: string[]): {
  project: string;
  seats: McpSeat[];
  expiresAt?: string;
} {
  let project: string | undefined;
  let expiresAt: string | undefined;
  const seats: McpSeat[] = [];
  for (let index = 0; index < arguments_.length; index += 2) {
    const name = arguments_[index];
    const value = arguments_[index + 1];
    if (value === undefined || value.startsWith("--")) {
      throw new Error("mcp peer-provision options require values");
    }
    if (name === "--project" && project === undefined) project = value;
    else if (name === "--seat") seats.push(parseMcpSeat(value));
    else if (name === "--expires-at" && expiresAt === undefined) expiresAt = value;
    else throw new Error("mcp peer-provision received an unknown or duplicate option");
  }
  if (project === undefined || seats.length === 0) {
    throw new Error("mcp peer-provision requires --project PATH and at least one --seat SEAT");
  }
  if (new Set(seats).size !== seats.length) {
    throw new Error("mcp peer-provision received a duplicate seat");
  }
  return { project, seats: [...seats].sort(), ...(expiresAt === undefined ? {} : { expiresAt }) };
}

export async function provisionMcpPeerSeats(
  arguments_: string[],
  paths: FabricPaths,
): Promise<McpProvisionOutput> {
  const request = parseArguments(arguments_);
  const roster = await installedRoster(paths, request.project);
  let chair: InstalledSeat | undefined;
  for (const member of roster) {
    if (member.metadata.role === "chair") chair = member;
  }
  if (chair === undefined) throw new McpPeerProvisionChairRequiredError(request.project);
  if (request.seats.includes(chair.metadata.seat)) {
    throw new Error(`mcp peer-provision refuses to provision the active chair seat ${chair.metadata.seat}`);
  }
  const present = new Set(roster.map(({ metadata }) => metadata.seat));
  if (request.seats.every((seat) => present.has(seat))) return rosterOutput(chair, roster);

  const daemonHandle = await startMcpProvisionDaemon(paths);
  let client: Awaited<ReturnType<typeof connectFabricDaemon>> | undefined;
  let database: Database.Database | undefined;
  try {
    const chairCapability = (await privateRead(chair.credentialPath)).trim();
    client = await connectFabricDaemon({
      socketPath: daemonHandle.address.path,
      capability: chairCapability,
    });
    database = new Database(paths.databasePath, { readonly: true, fileMustExist: true });
    const authorityRow = database.prepare(`
      SELECT agent.authority_id,authority.authority_json,authority.authority_hash
        FROM agents agent
        JOIN authorities authority
          ON authority.run_id=agent.run_id AND authority.authority_id=agent.authority_id
       WHERE agent.run_id=? AND agent.agent_id=?
    `).get(chair.metadata.runId, chair.metadata.agentId) as {
      authority_id?: unknown;
      authority_json?: unknown;
      authority_hash?: unknown;
    } | undefined;
    if (authorityRow === undefined || typeof authorityRow.authority_id !== "string") {
      throw new Error("chair authority is unavailable");
    }
    const parentAuthorityId = authorityRow.authority_id;
    const parentAuthority = readStoredAuthority(authorityRow, "chair authority");
    const expiresAt = peerExpiry(request.expiresAt, parentAuthority, chair.metadata.expiresAt);
    const registeredBindings: ParsedSeatBinding[] = [];
    for (const seat of request.seats) {
      if (present.has(seat)) continue;
      const agentId = peerAgentId(chair.metadata.projectKey, chair.metadata.runId, seat);
      const delegated = await client.delegateAuthority({
        parentAuthorityId,
        authority: peerSeatAuthority(parentAuthority),
        commandId: `peer-seat:${chair.metadata.projectKey}:${chair.metadata.runId}:${seat}`,
      });
      const registration = await client.registerAgent({ agentId, authorityId: delegated.authorityId });
      void registration.capability;
      const capability = database.prepare(`
        SELECT principal_generation
          FROM capabilities
         WHERE run_id=? AND agent_id=? AND revoked_at IS NULL AND expires_at>?
         ORDER BY principal_generation DESC LIMIT 1
      `).get(chair.metadata.runId, agentId, Date.now()) as { principal_generation?: unknown } | undefined;
      if (
        capability === undefined ||
        typeof capability.principal_generation !== "number" ||
        !Number.isSafeInteger(capability.principal_generation)
      ) {
        throw new Error(`registered peer ${seat} has no live capability`);
      }
      registeredBindings.push({
        seat,
        agentId,
        expectedPrincipalGeneration: capability.principal_generation,
      });
    }
    const convergenceDeadline = Date.now() + ROSTER_CONVERGENCE_TIMEOUT_MS;
    while (true) {
      const latestRoster = await installedRoster(paths, chair.metadata.projectPath);
      const bindings = new Map<McpSeat, ParsedSeatBinding>();
      for (const { metadata } of latestRoster) {
        bindings.set(metadata.seat, {
          seat: metadata.seat,
          agentId: metadata.agentId,
          expectedPrincipalGeneration: metadata.principalGeneration,
        });
      }
      for (const binding of registeredBindings) bindings.set(binding.seat, binding);
      try {
        return await bindProvisionedSeatRoster({
          project: chair.metadata.projectPath,
          projectSessionId: chair.metadata.projectSessionId,
          sessionRevision: chair.metadata.sessionRevision,
          sessionGeneration: chair.metadata.sessionGeneration,
          runId: chair.metadata.runId,
          runRevision: chair.metadata.runRevision,
          chairSeat: chair.metadata.seat,
          chairAgentId: chair.metadata.chairAgentId,
          chairGeneration: chair.metadata.chairGeneration,
          chairLeaseId: chair.metadata.chairLeaseId,
          bindings: [...bindings.values()].sort((left, right) => left.seat.localeCompare(right.seat)),
          expiresAt,
        }, paths);
      } catch (error: unknown) {
        if (!rosterCasChanged(error) || Date.now() >= convergenceDeadline) throw error;
        await waitForRosterConvergence();
      }
    }
  } finally {
    try {
      database?.close();
    } finally {
      try {
        await client?.close();
      } finally {
        daemonHandle.release();
      }
    }
  }
}
