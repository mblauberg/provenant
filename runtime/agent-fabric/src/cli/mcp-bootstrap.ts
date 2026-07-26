import { realpath } from "node:fs/promises";

import Database from "better-sqlite3";
import { MCP_BOOTSTRAP_CREDENTIALS_FEATURE } from "@local/agent-fabric-protocol";

import { connectFabricDaemon, startFabricDaemon } from "../daemon/client.js";
import type { BootstrapMcpSeatResult } from "../core/contracts.js";
import { defaultDaemonStartOptions } from "./default-daemon-options.js";
import type { FabricPaths } from "./paths.js";
import {
  installSeatGeneration,
  markLegacyBootstrapSeatGeneration,
  parseMcpSeat,
  resolveSeatProject,
  type SeatMetadata,
} from "./seat-store.js";
import { trustedWorkspaceIdentity } from "./workspace-trust.js";

export type InstalledBootstrapMcpSeat = BootstrapMcpSeatResult & {
  credential: string;
};

export type BootstrapMcpSeatIdentity = {
  seat: string;
  agentId: string;
  runId: string;
  authorityId: string;
  generation: string;
  lease: {
    leaseId: string;
    holderAgentId: string;
    generation: number;
    state: "active" | "frozen" | "revoked";
  };
};

export class McpBootstrapError extends Error {
  constructor(
    readonly code: "WORKSPACE_NOT_TRUSTED" | "BOOTSTRAP_GENERATION_CHANGED",
    message: string,
    options?: ErrorOptions,
  ) {
    super(message, options);
    this.name = "McpBootstrapError";
  }
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", `'"'"'`)}'`;
}

function identityFromBootstrapResult(
  result: BootstrapMcpSeatResult,
  seat: string,
): BootstrapMcpSeatIdentity {
  const credential = result.credentials.find((candidate) => candidate.seat === seat);
  if (credential === undefined) throw new Error(`bootstrap did not bind the caller seat ${seat}`);
  return {
    seat,
    agentId: credential.agentId,
    runId: result.runId,
    authorityId: credential.authorityId,
    generation: result.generation,
    lease: {
      leaseId: result.chairLeaseId,
      holderAgentId: result.chairAgentId,
      generation: result.chairGeneration,
      state: "active",
    },
  };
}

export function bootstrapMcpSeatIdentity(
  result: BootstrapMcpSeatResult,
  seat: string,
): BootstrapMcpSeatIdentity {
  return identityFromBootstrapResult(result, parseMcpSeat(seat));
}

export async function inspectBootstrapMcpSeat(input: {
  environment: NodeJS.ProcessEnv;
  cwd: string;
  paths: FabricPaths;
}): Promise<BootstrapMcpSeatIdentity> {
  const seat = parseMcpSeat(input.environment.AGENT_FABRIC_SEAT ?? "");
  if (seat !== "claude" && seat !== "codex") throw new Error("MCP bootstrap supports only claude or codex seats");
  const canonicalRoot = await realpath(input.cwd);
  const database = new Database(input.paths.databasePath, { readonly: true, fileMustExist: true });
  try {
    const value = database.prepare(`
      SELECT member.seat,member.agent_id,member.run_id,agent.authority_id,
             generation.generation,lease.lease_id,lease.holder_agent_id,
             lease.generation AS lease_generation,lease.status
        FROM projects project
        JOIN mcp_active_seat_generations active ON active.project_id=project.project_id
        JOIN mcp_seat_generations generation ON generation.generation=active.generation
        JOIN mcp_seat_generation_members member ON member.generation=generation.generation
        JOIN agents agent ON agent.run_id=member.run_id AND agent.agent_id=member.agent_id
        JOIN run_chair_leases lease ON lease.lease_id=generation.chair_lease_id
       WHERE project.canonical_root=? AND member.seat=?
    `).get(canonicalRoot, seat);
    if (typeof value !== "object" || value === null) {
      throw new Error(`bootstrap seat ${seat} is not installed for ${canonicalRoot}`);
    }
    const row = value as Record<string, unknown>;
    const text = (name: string): string => {
      const field = row[name];
      if (typeof field !== "string" || field.length === 0) throw new Error(`stored bootstrap ${name} is invalid`);
      return field;
    };
    const leaseGeneration = row.lease_generation;
    if (!Number.isSafeInteger(leaseGeneration) || (leaseGeneration as number) < 1) {
      throw new Error("stored bootstrap lease generation is invalid");
    }
    const state = text("status");
    if (state !== "active" && state !== "frozen" && state !== "revoked") {
      throw new Error("stored bootstrap lease state is invalid");
    }
    return {
      seat: text("seat"),
      agentId: text("agent_id"),
      runId: text("run_id"),
      authorityId: text("authority_id"),
      generation: text("generation"),
      lease: {
        leaseId: text("lease_id"),
        holderAgentId: text("holder_agent_id"),
        generation: leaseGeneration as number,
        state,
      },
    };
  } finally {
    database.close();
  }
}

export async function bootstrapMcpSeat(input: {
  environment: NodeJS.ProcessEnv;
  cwd: string;
  paths: FabricPaths;
  now?: Date;
}): Promise<InstalledBootstrapMcpSeat> {
  const seat = parseMcpSeat(input.environment.AGENT_FABRIC_SEAT ?? "");
  if (seat !== "claude" && seat !== "codex") throw new Error("MCP bootstrap supports only claude or codex seats");
  const canonicalRoot = await realpath(input.cwd);
  let identity: Awaited<ReturnType<typeof trustedWorkspaceIdentity>>;
  try {
    identity = await trustedWorkspaceIdentity({
      stateDirectory: input.paths.stateDirectory,
      canonicalRoot,
    });
  } catch (cause: unknown) {
    throw new McpBootstrapError(
      "WORKSPACE_NOT_TRUSTED",
      `Fabric bootstrap requires the exact current project root to be trusted; run "$HOME/.agents/scripts/agent-fabric" workspace trust ${shellQuote(canonicalRoot)}; then retry fabric_bootstrap`,
      { cause },
    );
  }
  const daemonHandle = await startFabricDaemon(
    defaultDaemonStartOptions(input.paths, input.environment.AGENTS_HOME),
  );
  let daemon: Awaited<ReturnType<typeof connectFabricDaemon>> | undefined;
  try {
    daemon = await connectFabricDaemon({
      socketPath: daemonHandle.address.path,
      capability: daemonHandle.bootstrapCapability,
      requiredCapabilities: [MCP_BOOTSTRAP_CREDENTIALS_FEATURE],
    });
    const result = await daemon.bootstrapMcpSeat({
      canonicalRoot: identity.canonicalRoot,
      trustRecordDigest: identity.trustRecordDigest,
      seat,
      expiresAt: new Date((input.now ?? new Date()).getTime() + 24 * 60 * 60 * 1_000).toISOString(),
    });
    const chairSeat = result.credentials.find(({ agentId }) => agentId === result.chairAgentId)?.seat;
    if (chairSeat === undefined) throw new Error("daemon bootstrap result did not bind the current chair");
    const seatProject = await resolveSeatProject({
      stateDirectory: input.paths.stateDirectory,
      project: result.canonicalRoot,
      createDirectories: true,
    });
    const stagedSeats = (includeOriginKind: boolean): Array<{
      metadata: Omit<SeatMetadata, "credentialPath">;
      credential: string;
    }> => result.credentials.map((binding) => ({
        credential: binding.capability,
        metadata: {
          schemaVersion: 1,
          projectKey: seatProject.projectKey,
          projectPath: result.canonicalRoot,
          generation: result.generation,
          previousGeneration: result.expectedPreviousGeneration,
          ...(includeOriginKind ? { originKind: "bootstrap" as const } : {}),
          projectSessionId: result.projectSessionId,
          sessionRevision: result.sessionRevision,
          sessionGeneration: result.sessionGeneration,
          runId: result.runId,
          runRevision: result.runRevision,
          chairAgentId: result.chairAgentId,
          chairGeneration: result.chairGeneration,
          chairLeaseId: result.chairLeaseId,
          seat: parseMcpSeat(binding.seat),
          agentId: binding.agentId,
          principalGeneration: binding.expectedPrincipalGeneration,
          role: binding.seat === chairSeat ? "chair" : "peer",
          expiresAt: result.expiresAt,
        },
      }));
    const install = async (seats: ReturnType<typeof stagedSeats>) => await installSeatGeneration({
      stateDirectory: input.paths.stateDirectory,
      projectPath: result.canonicalRoot,
      generation: result.generation,
      expectedPreviousGeneration: result.expectedPreviousGeneration,
      seats,
      allowMissingPreviousGeneration: true,
    });
    let installed: Awaited<ReturnType<typeof installSeatGeneration>>;
    try {
      installed = await install(stagedSeats(true));
    } catch (cause: unknown) {
      if (cause instanceof Error && cause.message.includes("existing MCP seat generation differs")) {
        try {
          installed = await install(stagedSeats(false));
          await markLegacyBootstrapSeatGeneration({
            stateDirectory: input.paths.stateDirectory,
            projectPath: result.canonicalRoot,
            generation: result.generation,
          });
        } catch (legacyCause: unknown) {
          if (!(legacyCause instanceof Error) || !legacyCause.message.includes("active MCP seat generation changed")) {
            throw legacyCause;
          }
          throw new McpBootstrapError(
            "BOOTSTRAP_GENERATION_CHANGED",
            "Fabric bootstrap seat generation changed during local cutover",
            { cause: legacyCause },
          );
        }
      } else {
        if (!(cause instanceof Error) || !cause.message.includes("active MCP seat generation changed")) throw cause;
        throw new McpBootstrapError(
          "BOOTSTRAP_GENERATION_CHANGED",
          "Fabric bootstrap seat generation changed during local cutover",
          { cause },
        );
      }
    }
    const selected = installed.find((candidate) => candidate.seat === seat);
    const selectedCredential = result.credentials.find((candidate) => candidate.seat === seat)?.capability;
    if (selected === undefined || selectedCredential === undefined) throw new Error("bootstrap did not install the caller seat");
    return { ...result, credential: selectedCredential };
  } finally {
    try {
      await daemon?.close();
    } finally {
      daemonHandle.release();
    }
  }
}
