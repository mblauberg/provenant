import Database from "better-sqlite3";
import { constants } from "node:fs";
import { chmod, lstat, open, rename, rm } from "node:fs/promises";
import { join } from "node:path";

import { readStoredAuthority } from "../authority/stored-authority.js";
import { connectFabricDaemon } from "../daemon/client.js";
import { FABRIC_OPERATIONS } from "../domain/operations.js";
import type { AuthorityInput } from "../domain/types.js";
import type { FabricPaths } from "./paths.js";
import { startMcpProvisionDaemon } from "./mcp-provision.js";
import { MCP_SEATS, resolveSeatPaths, type SeatMetadata } from "./seat-store.js";

async function privateRead(path: string): Promise<string> {
  const before = await lstat(path);
  if (!before.isFile() || before.isSymbolicLink() || (before.mode & 0o077) !== 0) throw new Error("observer provision source must be private");
  const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
  try { return await handle.readFile("utf8"); } finally { await handle.close(); }
}

async function privateWrite(path: string, value: string): Promise<void> {
  const temporary = `${path}.tmp-${process.pid}`;
  const handle = await open(temporary, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
  try { await handle.writeFile(value, "utf8"); await handle.sync(); } finally { await handle.close(); }
  try { await rename(temporary, path); await chmod(path, 0o600); } finally { await rm(temporary, { force: true }); }
}

function seatMetadata(value: unknown): SeatMetadata {
  if (
    typeof value !== "object" || value === null || Array.isArray(value) ||
    !("schemaVersion" in value) || value.schemaVersion !== 1 ||
    !("projectKey" in value) || typeof value.projectKey !== "string" ||
    !("projectPath" in value) || typeof value.projectPath !== "string" ||
    !("generation" in value) || typeof value.generation !== "string" || !/^[0-9a-f]{64}$/u.test(value.generation) ||
    !("previousGeneration" in value) ||
      (value.previousGeneration !== null &&
        (typeof value.previousGeneration !== "string" || !/^[0-9a-f]{64}$/u.test(value.previousGeneration))) ||
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
  ) throw new Error("MCP seat metadata is invalid");
  return value as SeatMetadata;
}

export function observerAuthority(parent: AuthorityInput): AuthorityInput {
  const disclosure = parent.disclosure.level === "allowed" ||
      (parent.disclosure.level === "scoped" && parent.disclosure.scopes.includes("local"))
    ? { level: "scoped", scopes: ["local"] } as const
    : { level: "forbidden" } as const;
  return {
    schemaVersion: 2,
    approval: parent.approval,
    workspaceRoots: [...parent.workspaceRoots],
    sourcePaths: [],
    artifactPaths: [],
    actions: [FABRIC_OPERATIONS.observeEvents],
    deniedPaths: [...parent.deniedPaths],
    deniedActions: [...parent.deniedActions],
    prohibitedActions: [...parent.prohibitedActions],
    disclosure,
    secrets: { access: "none" },
    deployment: { allowed: false },
    irreversibleActions: { allowed: false },
    network: { toolEgress: "none" },
    expiresAt: parent.expiresAt,
    budget: {},
  };
}

const PEER_SEAT_ACTIONS = [
  FABRIC_OPERATIONS.sendMessage,
  FABRIC_OPERATIONS.createDiscussionGroup,
  FABRIC_OPERATIONS.receiveMessages,
  FABRIC_OPERATIONS.acknowledgeDelivery,
  FABRIC_OPERATIONS.getMailboxState,
  FABRIC_OPERATIONS.getTask,
  FABRIC_OPERATIONS.getTeam,
  FABRIC_OPERATIONS.whoami,
  FABRIC_OPERATIONS.getRunStatus,
  FABRIC_OPERATIONS.listTasks,
  FABRIC_OPERATIONS.listAgents,
  FABRIC_OPERATIONS.listReceipts,
] as const;

export function peerSeatAuthority(parent: AuthorityInput): AuthorityInput {
  const parentActions = new Set(parent.actions);
  return {
    schemaVersion: 2,
    approval: parent.approval,
    workspaceRoots: [...parent.workspaceRoots],
    sourcePaths: [],
    artifactPaths: [],
    actions: PEER_SEAT_ACTIONS.filter((action) => parentActions.has(action)),
    deniedPaths: [...parent.deniedPaths],
    deniedActions: [...parent.deniedActions],
    prohibitedActions: [...parent.prohibitedActions],
    disclosure: { level: "forbidden" },
    secrets: { access: "none" },
    deployment: { allowed: false },
    irreversibleActions: { allowed: false },
    network: { toolEgress: "none" },
    expiresAt: parent.expiresAt,
    budget: {},
  };
}

export async function provisionObserverCredential(input: { project: string; paths: FabricPaths }): Promise<{
  schemaVersion: 1;
  runId: string;
  agentId: string;
  credentialPath: string;
  metadataPath: string;
  expiresAt: string;
}> {
  let chair: SeatMetadata | undefined;
  for (const seat of MCP_SEATS) {
    const paths = await resolveSeatPaths({ stateDirectory: input.paths.stateDirectory, project: input.project, seat });
    try {
      const metadata = seatMetadata(JSON.parse(await privateRead(paths.metadataPath)));
      if (metadata.role === "chair") chair = metadata;
    } catch (error: unknown) {
      if (typeof error !== "object" || error === null || !("code" in error) || error.code !== "ENOENT") throw error;
    }
  }
  if (chair === undefined) throw new Error("observer provision requires one active chair seat");
  const capability = (await privateRead(chair.credentialPath)).trim();
  const database = new Database(input.paths.databasePath, { readonly: true, fileMustExist: true });
  let parentAuthorityId: string;
  let parentAuthority: AuthorityInput;
  try {
    const row = database.prepare(`
      SELECT agent.authority_id, authority.authority_json, authority.authority_hash
        FROM agents agent
        JOIN authorities authority ON authority.run_id=agent.run_id AND authority.authority_id=agent.authority_id
       WHERE agent.run_id=? AND agent.agent_id=?
    `).get(chair.runId, chair.agentId) as {
      authority_id?: unknown;
      authority_json?: unknown;
      authority_hash?: unknown;
    } | undefined;
    if (row === undefined || typeof row.authority_id !== "string") {
      throw new Error("chair authority is unavailable");
    }
    parentAuthorityId = row.authority_id;
    parentAuthority = readStoredAuthority(row, "chair authority");
  } finally {
    database.close();
  }
  const daemonHandle = await startMcpProvisionDaemon(input.paths, process.env, input.project);
  try {
    const client = await connectFabricDaemon({
      socketPath: daemonHandle.address.path,
      capability,
    });
    try {
    const observerExpiresAt = parentAuthority.expiresAt;
    const authority = observerAuthority(parentAuthority);
    const delegated = await client.delegateAuthority({
      parentAuthorityId,
      authority,
      commandId: `observer-provision:${chair.projectKey}:${chair.runId}`,
    });
    const agentId = "fabric-observer";
    const registration = await client.registerAgent({ agentId, authorityId: delegated.authorityId });
    const directory = join(input.paths.stateDirectory, "seats", chair.projectKey);
    const credentialPath = join(directory, "observer.cap");
    const metadataPath = join(directory, "observer.json");
    await privateWrite(credentialPath, `${registration.capability}\n`);
    await privateWrite(metadataPath, `${JSON.stringify({
      schemaVersion: 1, projectKey: chair.projectKey, projectPath: chair.projectPath, runId: chair.runId,
      agentId, role: "observer", credentialPath, expiresAt: observerExpiresAt,
    }, null, 2)}\n`);
    return { schemaVersion: 1, runId: chair.runId, agentId, credentialPath, metadataPath, expiresAt: observerExpiresAt };
    } finally {
      await client.close();
    }
  } finally {
    daemonHandle.release();
  }
}
