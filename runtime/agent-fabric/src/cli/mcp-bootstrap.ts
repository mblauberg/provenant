import { randomUUID } from "node:crypto";
import { createConnection } from "node:net";

import Database from "better-sqlite3";
import {
  FABRIC_OPERATIONS,
  MCP_BOOTSTRAP_CREDENTIALS_FEATURE,
  MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE,
  NdjsonRpcTransport,
} from "@local/agent-fabric-protocol";

import { connectFabricDaemon, startFabricDaemon } from "../daemon/client.js";
import type { BootstrapMcpSeatResult } from "../core/contracts.js";
import {
  inspectFabricDatabaseForCutover,
  inspectRetainedWork,
  stableSourceSetSha256,
  type RetainedWorkCensus,
  type SchemaCutoverFieldMismatch,
} from "../core/migrations.js";
import { defaultDaemonStartOptions } from "./default-daemon-options.js";
import type { FabricPaths } from "./paths.js";
import { projectBoundaryEvidenceDigest, resolveProjectBoundary, type ProjectBoundary } from "./project-boundary.js";
import {
  attachLifecycleReceipt,
  currentLedgerGeneration,
  lifecycleFailureReceipt,
  type LifecycleAction,
  type LifecycleActionReceipt,
} from "../lifecycle/lifecycle-receipt.js";
export type { LifecycleAction, LifecycleActionReceipt } from "../lifecycle/lifecycle-receipt.js";
import { fabricCliCommand } from "../domain/fabric-roots.js";
import {
  installSeatGeneration,
  markLegacyBootstrapSeatGeneration,
  parseMcpSeat,
  readActiveSeatGeneration,
  resolveSeatProject,
  SeatGenerationChangedError,
  type SeatMetadata,
} from "./seat-store.js";
import {
  ensureAutomaticBootstrapTrust,
  trustedWorkspaceIdentity,
  workspaceTrustFailureContext,
} from "./workspace-trust.js";

/**
 * Whole-smoke wall-clock budget covering connect, `whoami` and `mailbox.read`.
 *
 * The smoke runs immediately after this same call completed a bootstrap RPC on
 * the same Unix socket, so it is two loopback round trips against an already
 * warm daemon. Two seconds is roughly two orders of magnitude above the
 * observed local round trip yet an order of magnitude below the transport's
 * 30s request timeout, so the smoke does not inherit a transport stall.
 *
 * The budget bounds two different things, and only one of them absolutely.
 * Settling is bounded under a responsive event loop: a timer aborts the
 * connect or the pending RPCs. The reported outcome is bounded
 * unconditionally, because elapsed time is measured and a smoke that overran
 * is failed even if its work resolved — a synchronous frame parse can outlive
 * the timer it then clears. An unbounded health check is a hang, so neither
 * bound is left to the daemon answering.
 */
export const IDENTITY_SMOKE_DEADLINE_MS = 2_000;

/**
 * Material state displacement is one user decision, never an automatic action.
 * The gate reports what exists and what would move, and displaces nothing.
 */
export type SchemaCutoverGate = Readonly<{
  schemaVersion: 1;
  kind: "agent-fabric-schema-cutover-gate";
  decision: "archive-and-fresh";
  databasePath: string;
  mismatch: Readonly<{
    code: "SCHEMA_CUTOVER_REQUIRED";
    message: string;
    fields: readonly SchemaCutoverFieldMismatch[];
  }>;
  retained: RetainedWorkCensus;
  sourceSetSha256: string | null;
  consequences: readonly string[];
  command: string;
  displaced: false;
}>;

export type InstalledBootstrapMcpSeat = BootstrapMcpSeatResult & {
  credential: string;
  receipt: LifecycleActionReceipt;
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
    readonly code: "WORKSPACE_NOT_TRUSTED" | "BOOTSTRAP_GENERATION_CHANGED" | "CUSTODY_AMBIGUOUS" | "POST_CUSTODY_BOUNDARY",
    message: string,
    options?: ErrorOptions,
  ) {
    super(message, options);
    this.name = "McpBootstrapError";
  }
}

/** Raised instead of displacing state; the caller must obtain one user decision. */
export class McpBootstrapSchemaCutoverGateError extends Error {
  readonly code = "SCHEMA_CUTOVER_GATE_REQUIRED";

  constructor(readonly gate: SchemaCutoverGate, options?: ErrorOptions) {
    super(
      `Fabric cannot start against this database without displacing local coordination state; ` +
      `report the gate and obtain one user decision before running: ${gate.command}`,
      options,
    );
    this.name = "McpBootstrapSchemaCutoverGateError";
  }
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", `'"'"'`)}'`;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function isAmbiguousBootstrapResponse(error: unknown): boolean {
  if (typeof error !== "object" || error === null || !("code" in error) || typeof error.code !== "string") return false;
  return ["DAEMON_DISCONNECTED", "DAEMON_REQUEST_TIMEOUT", "DAEMON_PROTOCOL_INVALID", "DAEMON_CONNECT_TIMEOUT"].includes(error.code);
}

function workspaceTrustAction(
  trust: Awaited<ReturnType<typeof ensureAutomaticBootstrapTrust>>,
): Extract<LifecycleAction, { action: "workspace-trust" }> {
  const entry = trust.identity.entry;
  return {
    action: "workspace-trust",
    outcome: trust.alreadyTrusted
      ? (entry.establishmentKind === "automatic-bootstrap" ? "already-trusted" : "resolved")
      : trust.mutated
        ? "enrolled"
      : "resolved",
    mutated: trust.mutated,
    alreadyTrusted: trust.alreadyTrusted,
    trustRetained: true,
    trustRecordDigest: trust.identity.trustRecordDigest,
    establishmentKind: entry.establishmentKind,
    boundaryKind: trust.boundaryKind,
    boundaryEvidenceDigest: trust.boundaryEvidenceDigest,
    requestAttemptId: trust.requestAttemptId,
    bootstrapAttemptId: trust.bootstrapAttemptId,
  };
}

function failedWorkspaceTrustAction(
  boundary: ProjectBoundary,
  requestAttemptId: string,
  boundaryEvidenceDigest: `sha256:${string}` = projectBoundaryEvidenceDigest(boundary),
): Extract<LifecycleAction, { action: "workspace-trust" }> {
  return {
    action: "workspace-trust",
    outcome: "failed",
    mutated: false,
    alreadyTrusted: false,
    trustRetained: false,
    trustRecordDigest: null,
    establishmentKind: null,
    boundaryKind: boundary.evidence.kind === "git" || boundary.evidence.kind === "project-marker"
      ? boundary.evidence.kind
      : null,
    boundaryEvidenceDigest,
    requestAttemptId,
    bootstrapAttemptId: null,
  };
}

async function workspaceTrustRecoveryMessage(
  boundary: ProjectBoundary,
  environment: NodeJS.ProcessEnv = process.env,
): Promise<string> {
  const evidence = boundary.evidence;
  if (evidence.kind === "refused" && evidence.reason === "filesystem-root") {
    return `Fabric bootstrap cannot proceed from ${shellQuote(evidence.root)}: the filesystem root can never be trusted because root-wide authority is forbidden by policy. Choose the exact repository root or current non-Git project directory instead`;
  }
  if (evidence.kind === "refused" && evidence.reason === "home") {
    return `Fabric bootstrap cannot proceed from ${shellQuote(evidence.root)}: this exact path can never be trusted because home-wide trust is forbidden by policy. Choose the exact repository root or current non-Git project directory instead`;
  }
  if (evidence.kind === "git" && evidence.linkedWorktree) {
    return `Fabric bootstrap cannot proceed from ${shellQuote(evidence.root)} because it is a linked worktree; granting a worktree-path trust exception is a user-only decision, and the agent must not run a trust command unprompted`;
  }
  if (evidence.kind === "ambiguous" && evidence.reason === "repository-collection") {
    return `Fabric bootstrap cannot proceed from ${shellQuote(evidence.root)} because it looks like a parent collection of several repositories; policy forbids trusting a parent or collection directory. Run fabric_bootstrap again from inside the specific project it actually needs`;
  }
  if ((evidence.kind === "git" || evidence.kind === "project-marker") && evidence.root !== boundary.requestedDirectory) {
    return `Fabric bootstrap requires the exact current project root to be trusted; run ${fabricCliCommand({ environment })} workspace trust ${shellQuote(evidence.root)}; then retry fabric_bootstrap from ${shellQuote(evidence.root)}`;
  }
  if (evidence.kind === "refused") {
    return `Fabric bootstrap cannot safely determine the trust boundary for ${shellQuote(boundary.requestedDirectory)}: ${evidence.detail}. No trust command can be suggested. Inspect the workspace and retry from the exact repository root or current non-Git project directory`;
  }
  return `Fabric bootstrap requires the exact current project root to be trusted; run ${fabricCliCommand({ environment })} workspace trust ${shellQuote(boundary.requestedDirectory)}; then retry fabric_bootstrap`;
}

export function isSchemaCutoverRefusal(error: unknown): boolean {
  return typeof error === "object" && error !== null &&
    "code" in error && error.code === "SCHEMA_CUTOVER_REQUIRED" &&
    "preserved" in error && error.preserved === true;
}

export function schemaCutoverGate(
  databasePath: string,
  cause: unknown,
  environment: NodeJS.ProcessEnv = process.env,
): SchemaCutoverGate {
  const inspection = inspectFabricDatabaseForCutover(databasePath);
  const mismatch = inspection.state === "incompatible"
    ? inspection.mismatch
    : {
      code: "SCHEMA_CUTOVER_REQUIRED" as const,
      message: cause instanceof Error ? cause.message : String(cause),
      fields: [],
    };
  const retained = inspectRetainedWork(databasePath);
  const total = retained.tables.reduce<number>((sum, { rows }) => sum + (rows ?? 0), 0);
  // Digested once: the confirmation the cutover will demand must be the exact
  // digest reported here, and each recomputation reclones the source set.
  const sourceSetSha256 = inspection.state === "absent"
    ? null
    : stableSourceSetSha256(inspection.sources);
  return {
    schemaVersion: 1,
    kind: "agent-fabric-schema-cutover-gate",
    decision: "archive-and-fresh",
    databasePath,
    mismatch,
    retained,
    sourceSetSha256,
    consequences: [
      `${String(total)} coordination rows are currently in ${databasePath} and stay there until approval.`,
      "Approval moves the whole SQLite source set into a new archive directory and starts an empty current-schema database.",
      "Runs, tasks, agents and messages in the archive are no longer visible to Fabric; only the archive receipt links them.",
      "Nothing is deleted: the archive keeps every source byte, and the cutover refuses if the source set changed since this report.",
      "The command asks for ARCHIVE-AND-FRESH on its controlling terminal and refuses non-interactive execution unless an explicit named-principal unattended approval assertion is supplied.",
    ],
    command: `${fabricCliCommand({ environment })} database archive-and-fresh --archive ABSOLUTE_NEW_DIRECTORY` +
      (sourceSetSha256 === null ? "" : ` --confirm-source-set ${sourceSetSha256}`),
    displaced: false,
  };
}

/**
 * Bounded identity/mailbox smoke over the seat credential the caller just
 * installed. It only reads, so it never appears as a mutation, and it reports
 * its own failure as a receipt outcome rather than discarding the receipt.
 */
async function identityMailboxSmoke(input: {
  socketPath: string;
  credential: string;
  deadlineMs: number;
}): Promise<Extract<LifecycleAction, { action: "identity-smoke" }>> {
  const startedAt = Date.now();
  const socket = createConnection(input.socketPath);
  let transport: NdjsonRpcTransport | undefined;
  let timer: NodeJS.Timeout | undefined;
  const deadline = new Promise<never>((_resolve, reject) => {
    timer = setTimeout(
      () => reject(new Error(`identity/mailbox smoke exceeded its ${String(input.deadlineMs)}ms deadline`)),
      input.deadlineMs,
    );
  });
  const base = { action: "identity-smoke" as const, mutated: false as const, deadlineMs: input.deadlineMs };
  try {
    transport = await Promise.race([
      NdjsonRpcTransport.connect(socket, {
        protocolVersion: 1,
        client: { name: "agent-fabric-lifecycle-smoke", version: "1.0.0" },
        authentication: {
          scheme: "capability",
          credential: input.credential,
          clientNonce: `lifecycle_smoke_${randomUUID()}`,
        },
        expectedPrincipalKind: "agent",
        requiredFeatures: ["fabric-core.v1"],
        optionalFeatures: [],
      }),
      deadline,
    ]);
    const identity = await Promise.race([transport.call(FABRIC_OPERATIONS.whoami, {}), deadline]);
    const mailbox = await Promise.race([transport.call(FABRIC_OPERATIONS.getMailboxState, {}), deadline]);
    // The timer alone does not bound the outcome: a socket callback can begin
    // just before expiry, parse a frame synchronously past the deadline,
    // resolve, and clear the overdue timer before it ever fires. Measuring
    // elapsed time makes the deadline a real bound on what the receipt may
    // report, whatever the event loop did in between.
    const elapsedMs = Date.now() - startedAt;
    if (elapsedMs > input.deadlineMs) {
      return { ...base, outcome: "failed", elapsedMs, agentId: null, mailboxWatermark: null, code: "IDENTITY_SMOKE_DEADLINE_EXCEEDED" };
    }
    return {
      ...base,
      outcome: "passed",
      elapsedMs,
      agentId: identity.agentId,
      mailboxWatermark: mailbox.contiguousWatermark,
      code: null,
    };
  } catch (error: unknown) {
    const code = typeof error === "object" && error !== null && "code" in error &&
      typeof error.code === "string" && error.code.length > 0
      ? error.code
      : "IDENTITY_SMOKE_DEADLINE_EXCEEDED";
    return { ...base, outcome: "failed", elapsedMs: Date.now() - startedAt, agentId: null, mailboxWatermark: null, code };
  } finally {
    if (timer !== undefined) clearTimeout(timer);
    if (transport === undefined) socket.destroy();
    else await transport.close().catch(() => undefined);
  }
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
  if (seat !== "claude" && seat !== "codex") {
    throw new Error(
      `MCP bootstrap creates only chair seats claude or codex; run ` +
      `${fabricCliCommand({ environment: input.environment })} mcp peer-provision --project ${shellQuote(input.cwd)} --seat ${seat} instead`,
    );
  }
  const canonicalRoot = (await resolveProjectBoundary(input.cwd)).selectedProjectRoot;
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
  smokeDeadlineMs?: number;
  testOnly?: {
    beforeRegistryRename?: () => Promise<void>;
    afterRegistryRename?: () => Promise<void>;
  };
}): Promise<InstalledBootstrapMcpSeat> {
  const seat = parseMcpSeat(input.environment.AGENT_FABRIC_SEAT ?? "");
  if (seat !== "claude" && seat !== "codex") {
    throw new Error(
      `MCP bootstrap creates only chair seats claude or codex; run ` +
      `${fabricCliCommand({ environment: input.environment })} mcp peer-provision --project ${shellQuote(input.cwd)} --seat ${seat} instead`,
    );
  }
  let canonicalRoot = input.cwd;
  const bootstrapAttemptId = randomUUID();
  let trust: Awaited<ReturnType<typeof ensureAutomaticBootstrapTrust>>;
  try {
    trust = await ensureAutomaticBootstrapTrust({
      stateDirectory: input.paths.stateDirectory,
      bootstrapAttemptId,
      cwd: input.cwd,
      ...(input.now === undefined ? {} : { now: input.now }),
      ...(input.testOnly === undefined ? {} : { testOnly: input.testOnly }),
    });
  } catch (cause: unknown) {
    const failedContext = workspaceTrustFailureContext(cause);
    const failureBoundary = failedContext?.boundary;
    if (failureBoundary !== undefined) canonicalRoot = failureBoundary.selectedProjectRoot;
    const message = cause instanceof Error
      ? cause.message
      : failureBoundary === undefined
        ? `Fabric bootstrap cannot safely determine the trust boundary for ${shellQuote(canonicalRoot)}, so no trust command is suggested. Inspect the workspace and retry from the exact repository root or current non-Git project directory`
        : await workspaceTrustRecoveryMessage(failureBoundary, input.environment).catch(
          () => `Fabric bootstrap cannot safely determine the trust boundary for ${shellQuote(canonicalRoot)}, so no trust command is suggested. Inspect the workspace and retry from the exact repository root or current non-Git project directory`,
        );
    const error = new McpBootstrapError(
      "WORKSPACE_NOT_TRUSTED",
      message,
      { cause },
    );
    throw attachLifecycleReceipt(error, lifecycleFailureReceipt({
      canonicalRoot,
      seat,
      generation: "",
      actions: failureBoundary === undefined ? [] : [failedWorkspaceTrustAction(
        failureBoundary,
        bootstrapAttemptId,
        failedContext?.boundaryEvidenceDigest,
      )],
      phase: "workspace-trust",
      cause,
    }));
  }
  const identity = trust.identity;
  canonicalRoot = identity.canonicalRoot;
  const trustAction = workspaceTrustAction(trust);
  // Read before the daemon can rotate anything: comparing this pointer with the
  // generation the daemon replays is what distinguishes an installed roster
  // from a replayed one without inferring it from timing or file mtimes.
  const generationBefore = await readActiveSeatGeneration({
    stateDirectory: input.paths.stateDirectory,
    projectPath: identity.canonicalRoot,
  }).catch(() => null);
  const phaseLedger: LifecycleAction[] = [trustAction];
  let daemonHandle: Awaited<ReturnType<typeof startFabricDaemon>>;
  try {
    daemonHandle = await startFabricDaemon(
      defaultDaemonStartOptions(input.paths, {
        environment: input.environment,
        projectRoot: identity.canonicalRoot,
      }),
    );
  } catch (cause: unknown) {
    const actions = [trustAction];
    if (isSchemaCutoverRefusal(cause)) {
      const error = new McpBootstrapSchemaCutoverGateError(
        schemaCutoverGate(input.paths.databasePath, cause, input.environment),
        { cause },
      );
      throw attachLifecycleReceipt(error, lifecycleFailureReceipt({
        canonicalRoot,
        seat,
        generation: currentLedgerGeneration(actions, generationBefore?.generation ?? ""),
        actions,
        phase: "daemon-start",
        cause,
      }));
    }
    const error = cause instanceof Error ? cause : new Error(errorMessage(cause));
    throw attachLifecycleReceipt(error, lifecycleFailureReceipt({
      canonicalRoot,
      seat,
      generation: currentLedgerGeneration(actions, generationBefore?.generation ?? ""),
      actions,
      phase: "daemon-start",
      cause,
    }));
  }
  phaseLedger.push({
    action: "daemon",
    outcome: daemonHandle.ownsProcess ? "started" : "attached",
    mutated: daemonHandle.ownsProcess,
    pid: daemonHandle.pid,
    socketPath: daemonHandle.address.path,
  });
  let daemon: Awaited<ReturnType<typeof connectFabricDaemon>> | undefined;
  let fabricCustodyReferenced = false;
  try {
    daemon = await connectFabricDaemon({
      socketPath: daemonHandle.address.path,
      capability: daemonHandle.bootstrapCapability,
      requiredCapabilities: [MCP_BOOTSTRAP_CREDENTIALS_FEATURE, MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE],
    });
    const bootstrapRequest = Object.freeze({
      canonicalRoot: identity.canonicalRoot,
      trustRecordDigest: identity.trustRecordDigest,
      seat,
      expiresAt: new Date((input.now ?? new Date()).getTime() + 24 * 60 * 60 * 1_000).toISOString(),
    });
    let reconciled = false;
    let result: BootstrapMcpSeatResult;
    try {
      result = await daemon.bootstrapMcpSeat(bootstrapRequest);
    } catch (cause: unknown) {
      if (!isAmbiguousBootstrapResponse(cause)) throw cause;
      await daemon.close().catch(() => undefined);
      try {
        daemon = await connectFabricDaemon({
          socketPath: daemonHandle.address.path,
          capability: daemonHandle.bootstrapCapability,
          requiredCapabilities: [MCP_BOOTSTRAP_CREDENTIALS_FEATURE, MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE],
        });
        result = await daemon.bootstrapMcpSeat(bootstrapRequest);
        reconciled = true;
      } catch (replayCause: unknown) {
        throw new McpBootstrapError(
          "CUSTODY_AMBIGUOUS",
          "custody-ambiguous: Fabric could not observe either immutable bootstrap response; trust is retained and custody ownership is unknown",
          { cause: replayCause },
        );
      }
    }
    fabricCustodyReferenced = true;
    phaseLedger.push({
      action: "custody",
      outcome: reconciled ? "reconciled" : result.custodyMutated ? "committed" : "replayed",
      mutated: reconciled ? false : result.custodyMutated,
      projectId: result.projectId,
      runId: result.runId,
      generation: result.generation,
    });
    let postCustodyIdentity: Awaited<ReturnType<typeof trustedWorkspaceIdentity>>;
    try {
      postCustodyIdentity = await trustedWorkspaceIdentity({
        stateDirectory: input.paths.stateDirectory,
        canonicalRoot: identity.canonicalRoot,
      });
    } catch (cause: unknown) {
      throw new McpBootstrapError(
        "POST_CUSTODY_BOUNDARY",
        "post-custody-boundary: the exact workspace trust binding changed after custody; custody is retained and no new credentials were published",
        { cause },
      );
    }
    if (
      postCustodyIdentity.canonicalRoot !== identity.canonicalRoot ||
      postCustodyIdentity.trustRecordDigest !== identity.trustRecordDigest
    ) {
      throw new McpBootstrapError(
        "POST_CUSTODY_BOUNDARY",
        "post-custody-boundary: the exact workspace trust binding changed after custody; custody is retained and no new credentials were published",
      );
    }
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
      allowStaleGenerationReconciliation: true,
      ...(input.now === undefined ? {} : { now: input.now }),
    });
    const generationChangedError = (cause: unknown): McpBootstrapError => {
      const recordedGeneration = cause instanceof SeatGenerationChangedError
        ? cause.recordedGeneration
        : generationBefore?.generation ?? null;
      const recordedPreviousGeneration = cause instanceof SeatGenerationChangedError
        ? cause.recordedPreviousGeneration
        : generationBefore?.previousGeneration ?? null;
      return new McpBootstrapError(
        "BOOTSTRAP_GENERATION_CHANGED",
        `Fabric bootstrap could not complete the local seat cutover for project ${result.canonicalRoot}. ` +
        `Recorded on-disk generation: ${recordedGeneration ?? "none"}. ` +
        `Recorded on-disk previous generation: ${recordedPreviousGeneration ?? "none"}. ` +
        `Daemon-expected prior generation: ${result.expectedPreviousGeneration ?? "none"}. ` +
        `Daemon-computed generation: ${result.generation}. ` +
        `Inspect ${seatProject.directory}/current.json and ${seatProject.directory}/generations, ` +
        `confirm no other bootstrap is running, then escalate with those values before changing seat state.`,
        { cause },
      );
    };
    let installed: Awaited<ReturnType<typeof installSeatGeneration>>;
    let legacyBootstrapProvenanceRecorded = false;
    try {
      installed = await install(stagedSeats(true));
    } catch (cause: unknown) {
      if (cause instanceof Error && cause.message.includes("existing MCP seat generation differs")) {
        try {
          installed = await install(stagedSeats(false));
          legacyBootstrapProvenanceRecorded = await markLegacyBootstrapSeatGeneration({
            stateDirectory: input.paths.stateDirectory,
            projectPath: result.canonicalRoot,
            generation: result.generation,
          }) === "recorded";
        } catch (legacyCause: unknown) {
          if (!(legacyCause instanceof SeatGenerationChangedError)) {
            throw legacyCause;
          }
          throw generationChangedError(legacyCause);
        }
      } else {
        if (!(cause instanceof SeatGenerationChangedError)) throw cause;
        throw generationChangedError(cause);
      }
    }
    const replayed = generationBefore?.generation === result.generation;
    phaseLedger.push({
      action: "seat-generation",
      outcome: replayed ? "replayed" : "installed",
      mutated: !replayed,
      generation: result.generation,
      previousGeneration: result.expectedPreviousGeneration,
    });
    const selected = installed.find((candidate) => candidate.seat === seat);
    const selectedCredential = result.credentials.find((candidate) => candidate.seat === seat)?.capability;
    if (selected === undefined || selectedCredential === undefined) throw new Error("bootstrap did not install the caller seat");
    const smoke = await identityMailboxSmoke({
      socketPath: daemonHandle.address.path,
      credential: selectedCredential,
      deadlineMs: input.smokeDeadlineMs ?? IDENTITY_SMOKE_DEADLINE_MS,
    });
    if (legacyBootstrapProvenanceRecorded) {
      phaseLedger.push({
        action: "legacy-bootstrap-provenance",
        outcome: "recorded",
        mutated: true,
        generation: result.generation,
      });
    }
    const actions: LifecycleAction[] = [...phaseLedger, smoke];
    return {
      ...result,
      credential: selectedCredential,
      receipt: {
        schemaVersion: 1,
        kind: "agent-fabric-lifecycle-action",
        canonicalRoot: result.canonicalRoot,
        seat,
        generation: result.generation,
        mutated: actions.some((action) => action.mutated),
        healthy: actions.every((action) => action.outcome !== "failed"),
        actions,
      },
    };
  } catch (cause: unknown) {
    const error = cause instanceof Error ? cause : new Error(errorMessage(cause));
    const actions = [...phaseLedger];
    throw attachLifecycleReceipt(error, lifecycleFailureReceipt({
      canonicalRoot,
      seat,
      generation: currentLedgerGeneration(actions, generationBefore?.generation ?? ""),
      actions,
      phase: cause instanceof McpBootstrapError && cause.code === "CUSTODY_AMBIGUOUS"
        ? "custody-ambiguous"
        : cause instanceof McpBootstrapError && cause.code === "POST_CUSTODY_BOUNDARY"
          ? "post-custody-boundary"
        : fabricCustodyReferenced ? "post-custody" : "daemon-bootstrap",
      cause,
    }));
  } finally {
    try {
      await daemon?.close();
    } finally {
      daemonHandle.release();
    }
  }
}
