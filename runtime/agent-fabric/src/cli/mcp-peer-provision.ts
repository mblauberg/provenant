import { createHash } from "node:crypto";
import Database from "better-sqlite3";
import { constants } from "node:fs";
import { lstat, open } from "node:fs/promises";

import { readStoredAuthority } from "../authority/stored-authority.js";
import { connectFabricDaemon } from "../daemon/client.js";
import { resolveFabricRoots } from "../domain/fabric-roots.js";
import type { AuthorityInput } from "../domain/types.js";
import {
  bindProvisionedSeatRoster,
  startMcpProvisionDaemon,
  type McpProvisionOutput,
  type ParsedSeatBinding,
} from "./mcp-provision.js";
import { peerSeatAuthority } from "./observer-provision.js";
import {
  MAXIMUM_SEAT_LIFETIME_MS,
  mcpBootstrapRenewalCommand,
  mcpRosterRenewalCommand,
  openRosterReadPort,
} from "./mcp-roster-renewal.js";
import type { FabricPaths } from "./paths.js";
import {
  MCP_SEATS,
  parseMcpSeat,
  readLegacyBootstrapSeatGeneration,
  resolveSeatPaths,
  type McpSeat,
  type SeatMetadata,
} from "./seat-store.js";

const ROSTER_CONVERGENCE_MAX_ATTEMPTS = 200;
const ROSTER_CONVERGENCE_POLL_MS = 25;

function rosterConvergenceMaxAttempts(): number {
  const configured = process.env.NODE_ENV === "test"
    ? Number.parseInt(process.env.AGENT_FABRIC_TEST_ROSTER_CONVERGENCE_ATTEMPTS ?? "", 10)
    : Number.NaN;
  return Number.isSafeInteger(configured) && configured > 0
    ? configured
    : ROSTER_CONVERGENCE_MAX_ATTEMPTS;
}

type InstalledSeat = {
  metadata: SeatMetadata;
  credentialPath: string;
  metadataPath: string;
};

export class McpPeerProvisionChairRequiredError extends Error {
  readonly code = "MCP_CHAIR_SEAT_REQUIRED" as const;

  constructor(project: string, recovery?: string) {
    super(
      `MCP peer provisioning requires an active chair seat for ${project}; ` +
      `run agent-fabric bootstrap --seat claude or agent-fabric bootstrap --seat codex from that project` +
      (recovery === undefined ? "" : `; ${recovery}`),
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
    const opened = await handle.stat();
    if (
      !opened.isFile() ||
      opened.dev !== before.dev ||
      opened.ino !== before.ino ||
      (opened.mode & 0o077) !== 0
    ) {
      throw new Error("peer provision source changed while opening");
    }
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
    ("originKind" in value && value.originKind !== "bootstrap" && value.originKind !== "provisioned") ||
    !("credentialPath" in value) || typeof value.credentialPath !== "string" ||
    !("expiresAt" in value) || typeof value.expiresAt !== "string"
  ) {
    throw new Error("MCP seat metadata is invalid");
  }
  return value as SeatMetadata;
}

async function installedRoster(
  paths: FabricPaths,
  project: string,
  options: { includeExpired?: boolean } = {},
): Promise<InstalledSeat[]> {
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
      // A renewal request must still see an expired roster: recovery reuses
      // the installed identity, so filtering it out here would leave expiry a
      // dead end that only a lineage-discarding bootstrap could exit (#526).
      if (options.includeExpired !== true && !(Date.parse(metadata.expiresAt) > Date.now())) continue;
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

// When no active chair exists but an expired provisioned roster does, the
// refusal names the recovery command instead of steering the operator to a
// bootstrap that would discard the roster's peer seats and lineage (#526).
async function chairRequiredError(
  paths: FabricPaths,
  project: string,
  productRoot: string,
): Promise<McpPeerProvisionChairRequiredError> {
  try {
    const roster = await installedRoster(paths, project, { includeExpired: true });
    const chair = roster.find((member) => member.metadata.role === "chair");
    const peer = roster.find((member) => member.metadata.role === "peer");
    if (
      chair !== undefined &&
      peer !== undefined &&
      !(Date.parse(chair.metadata.expiresAt) > Date.now()) &&
      roster.every((member) => member.metadata.originKind === "provisioned")
    ) {
      // This refusal performs exactly one lookup, so its request-scoped read
      // port opens and closes around that single read.
      const rosterPort = openRosterReadPort(paths.databasePath);
      let chairAuthorityExpiresAt: string | null;
      try {
        chairAuthorityExpiresAt = rosterPort.chairAuthorityExpiresAt({
          runId: chair.metadata.runId,
          chairAgentId: chair.metadata.chairAgentId,
        });
      } finally {
        rosterPort.close();
      }
      const recovery = mcpRosterRenewalCommand({
        project: chair.metadata.projectPath,
        peerSeat: peer.metadata.seat,
        currentExpiresAt: chair.metadata.expiresAt,
        chairAuthorityExpiresAt,
        productRoot,
      });
      if (recovery !== null) {
        return new McpPeerProvisionChairRequiredError(
          project,
          `the installed roster expired at ${chair.metadata.expiresAt} and can be recovered with ${recovery}`,
        );
      }
    }
  } catch {
    // Recovery advice is best-effort; the base refusal stands on its own.
  }
  return new McpPeerProvisionChairRequiredError(project);
}

// The stored seat metadata records the session identity as it stood when the
// generation was minted, while the daemon compares a rebind against the live
// session revision. A renewal therefore reads the live identity from the
// database; the stored value cannot be transcribed into a working bind once
// the session has advanced (#526).
function liveRosterIdentity(
  database: Database.Database,
  metadata: SeatMetadata,
): Pick<
  SeatMetadata,
  "sessionRevision" | "sessionGeneration" | "runRevision" | "chairGeneration" | "chairLeaseId"
> {
  const row = database.prepare(`
    SELECT session.revision AS session_revision, session.generation AS session_generation,
           run.revision AS run_revision, run.chair_agent_id, run.chair_generation, run.chair_lease_id
      FROM project_sessions session
      JOIN runs run ON run.project_session_id=session.project_session_id
     WHERE session.project_session_id=? AND run.run_id=?
  `).get(metadata.projectSessionId, metadata.runId) as {
    session_revision?: unknown;
    session_generation?: unknown;
    run_revision?: unknown;
    chair_agent_id?: unknown;
    chair_generation?: unknown;
    chair_lease_id?: unknown;
  } | undefined;
  if (row === undefined) {
    throw new Error(
      `MCP roster session or run no longer exists for ${metadata.projectPath}; ` +
      `rebuild the roster with a fresh bootstrap`,
    );
  }
  if (
    !Number.isSafeInteger(row.session_revision) ||
    !Number.isSafeInteger(row.session_generation) ||
    !Number.isSafeInteger(row.run_revision) ||
    !Number.isSafeInteger(row.chair_generation) ||
    typeof row.chair_lease_id !== "string" ||
    row.chair_agent_id !== metadata.chairAgentId
  ) {
    throw new Error(
      `MCP roster chair identity changed for ${metadata.projectPath}; ` +
      `rebuild the roster with a fresh bootstrap`,
    );
  }
  return {
    sessionRevision: row.session_revision as number,
    sessionGeneration: row.session_generation as number,
    runRevision: row.run_revision as number,
    chairGeneration: row.chair_generation as number,
    chairLeaseId: row.chair_lease_id,
  };
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

export function peerExpiry(
  requested: string | undefined,
  parent: AuthorityInput,
  currentExpiresAt: string,
): string {
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

function chairCredentialChanged(error: unknown): boolean {
  return error instanceof Error && /capability is expired or revoked|inactive MCP seat generation/u.test(error.message);
}

async function waitForRosterConvergence(): Promise<void> {
  await new Promise<void>((resolve) => setTimeout(resolve, ROSTER_CONVERGENCE_POLL_MS));
}

async function installedOriginKinds(
  paths: FabricPaths,
  project: string,
  chairSeat: McpSeat,
  roster: InstalledSeat[],
  productRoot: string,
): Promise<Partial<Record<McpSeat, NonNullable<SeatMetadata["originKind"]>>>> {
  const legacyBootstrapGeneration = await readLegacyBootstrapSeatGeneration({
    stateDirectory: paths.stateDirectory,
    projectPath: project,
  });
  const generations = new Set(roster.map(({ metadata }) => metadata.generation));
  if (generations.size !== 1) {
    throw new Error(`active MCP seat generation changed while reading origins for ${project}`);
  }
  const originKinds: Partial<Record<McpSeat, NonNullable<SeatMetadata["originKind"]>>> = {};
  for (const { metadata } of roster) {
    if (metadata.originKind !== undefined) {
      originKinds[metadata.seat] = metadata.originKind;
      continue;
    }
    if (legacyBootstrapGeneration === metadata.generation) {
      originKinds[metadata.seat] = "bootstrap";
      continue;
    }
    throw new Error(
      `mcp peer-provision cannot rebind ${project} because seat ${metadata.seat} origin is unknown; ` +
      `repair the bootstrap-managed roster with ${mcpBootstrapRenewalCommand(project, chairSeat, productRoot)}`,
    );
  }
  return originKinds;
}

function assertProvisionedRenewal(
  project: string,
  chairSeat: McpSeat,
  originKinds: Partial<Record<McpSeat, NonNullable<SeatMetadata["originKind"]>>>,
  productRoot: string,
): void {
  if (Object.values(originKinds).includes("bootstrap")) {
    throw new Error(
      `mcp peer-provision refuses to renew the bootstrap-managed roster for ${project}; ` +
      `use ${mcpBootstrapRenewalCommand(project, chairSeat, productRoot)}`,
    );
  }
}

export function parseMcpPeerProvisionArguments(arguments_: string[]): {
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
  const { productRoot } = resolveFabricRoots({});
  const request = parseMcpPeerProvisionArguments(arguments_);
  const renewalRequested = request.expiresAt !== undefined;
  const initialRoster = await installedRoster(paths, request.project, {
    includeExpired: renewalRequested,
  });
  const initialChair = initialRoster.find((member) => member.metadata.role === "chair");
  if (initialChair === undefined) throw await chairRequiredError(paths, request.project, productRoot);
  if (request.seats.includes(initialChair.metadata.seat)) {
    throw new Error(`mcp peer-provision refuses to provision the active chair seat ${initialChair.metadata.seat}`);
  }
  if (
    request.expiresAt === undefined &&
    request.seats.every((seat) => initialRoster.some((member) => member.metadata.seat === seat))
  ) {
    return rosterOutput(initialChair, initialRoster);
  }
  const initialOriginKinds = await installedOriginKinds(
    paths,
    initialChair.metadata.projectPath,
    initialChair.metadata.seat,
    initialRoster,
    productRoot,
  );
  if (request.expiresAt !== undefined) {
    assertProvisionedRenewal(
      initialChair.metadata.projectPath,
      initialChair.metadata.seat,
      initialOriginKinds,
      productRoot,
    );
  }
  const daemonHandle = await startMcpProvisionDaemon(paths);
  const convergenceMaxAttempts = rosterConvergenceMaxAttempts();
  let convergenceAttempts = 0;
  try {
    while (true) {
      convergenceAttempts += 1;
      let client: Awaited<ReturnType<typeof connectFabricDaemon>> | undefined;
      let database: Database.Database | undefined;
      try {
        const roster = await installedRoster(paths, request.project, {
          includeExpired: renewalRequested,
        });
        const chair = roster.find((member) => member.metadata.role === "chair");
        if (chair === undefined) throw await chairRequiredError(paths, request.project, productRoot);
        if (request.seats.includes(chair.metadata.seat)) {
          throw new Error(`mcp peer-provision refuses to provision the active chair seat ${chair.metadata.seat}`);
        }
        const present = new Set(roster.map(({ metadata }) => metadata.seat));
        if (
          request.expiresAt === undefined &&
          request.seats.every((seat) => present.has(seat))
        ) return rosterOutput(chair, roster);
        const currentOriginKinds = await installedOriginKinds(
          paths,
          chair.metadata.projectPath,
          chair.metadata.seat,
          roster,
          productRoot,
        );
        if (request.expiresAt !== undefined) {
          assertProvisionedRenewal(
            chair.metadata.projectPath,
            chair.metadata.seat,
            currentOriginKinds,
            productRoot,
          );
        }

        const chairExpired = !(Date.parse(chair.metadata.expiresAt) > Date.now());
        const missingSeats = request.seats.filter((seat) => !present.has(seat));
        if (chairExpired && missingSeats.length > 0) {
          throw new Error(
            `mcp peer-provision cannot register new seats for ${chair.metadata.projectPath} ` +
            `because the roster expired at ${chair.metadata.expiresAt}; ` +
            `recover the roster first by replaying --expires-at with an installed seat, then add the new seats`,
          );
        }
        // Registering a new seat delegates from the chair, which needs the
        // live chair credential. A pure renewal registers nothing, and once
        // the roster has expired that credential is unusable by definition, so
        // recovery proceeds on the installed roster and the daemon's own
        // bootstrap-capability gate instead (#526).
        if (!chairExpired) {
          const chairCapability = (await privateRead(chair.credentialPath)).trim();
          client = await connectFabricDaemon({
            socketPath: daemonHandle.address.path,
            capability: chairCapability,
          });
        }
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
        const identity = renewalRequested
          ? liveRosterIdentity(database, chair.metadata)
          : {
              sessionRevision: chair.metadata.sessionRevision,
              sessionGeneration: chair.metadata.sessionGeneration,
              runRevision: chair.metadata.runRevision,
              chairGeneration: chair.metadata.chairGeneration,
              chairLeaseId: chair.metadata.chairLeaseId,
            };
        const registeredBindings: ParsedSeatBinding[] = [];
        for (const seat of missingSeats) {
          if (client === undefined) {
            throw new Error("mcp peer-provision requires a chair connection to register new seats");
          }
          const agentId = peerAgentId(chair.metadata.projectKey, chair.metadata.runId, seat);
          const delegated = await client.delegateAuthority({
            parentAuthorityId,
            authority: peerSeatAuthority(parentAuthority),
            commandId: `peer-seat:${chair.metadata.projectKey}:${chair.metadata.runId}:${seat}`,
          });
          const registration = await client.registerAgent({ agentId, authorityId: delegated.authorityId });
          const capability = database.prepare(`
        SELECT principal_generation
          FROM capabilities
         WHERE token_hash=? AND run_id=? AND agent_id=? AND revoked_at IS NULL AND expires_at>?
         ORDER BY principal_generation DESC LIMIT 1
          `).get(
            createHash("sha256").update(registration.capability).digest("hex"),
            chair.metadata.runId,
            agentId,
            Date.now(),
          ) as { principal_generation?: unknown } | undefined;
          if (
            capability === undefined ||
            typeof capability.principal_generation !== "number" ||
            !Number.isSafeInteger(capability.principal_generation)
          ) {
            throw new Error(`registered peer ${seat} capability does not match live custody`);
          }
          registeredBindings.push({
            seat,
            agentId,
            expectedPrincipalGeneration: capability.principal_generation,
          });
        }
        const bindings = new Map<McpSeat, ParsedSeatBinding>();
        const originKinds = { ...currentOriginKinds };
        for (const { metadata } of roster) {
          bindings.set(metadata.seat, {
            seat: metadata.seat,
            agentId: metadata.agentId,
            expectedPrincipalGeneration: metadata.principalGeneration,
          });
        }
        for (const binding of registeredBindings) {
          bindings.set(binding.seat, binding);
          originKinds[binding.seat] = "provisioned";
        }
        return await bindProvisionedSeatRoster({
          project: chair.metadata.projectPath,
          projectSessionId: chair.metadata.projectSessionId,
          sessionRevision: identity.sessionRevision,
          sessionGeneration: identity.sessionGeneration,
          runId: chair.metadata.runId,
          runRevision: identity.runRevision,
          chairSeat: chair.metadata.seat,
          chairAgentId: chair.metadata.chairAgentId,
          chairGeneration: identity.chairGeneration,
          chairLeaseId: identity.chairLeaseId,
          bindings: [...bindings.values()].sort((left, right) => left.seat.localeCompare(right.seat)),
          originKinds,
          expectedActiveGeneration: chair.metadata.generation,
          requireProvisionedOrigins: request.expiresAt !== undefined,
          expiresAt,
        }, paths);
      } catch (error: unknown) {
        if (
          (!rosterCasChanged(error) && !chairCredentialChanged(error)) ||
          convergenceAttempts >= convergenceMaxAttempts
        ) throw error;
        await waitForRosterConvergence();
      } finally {
        try {
          database?.close();
        } finally {
          await client?.close();
        }
      }
    }
  } finally {
    daemonHandle.release();
  }
}
