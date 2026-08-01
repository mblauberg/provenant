import { randomUUID } from "node:crypto";
import { lstat, readFile, readdir, realpath } from "node:fs/promises";
import { createConnection } from "node:net";
import { homedir } from "node:os";
import { basename, dirname, join, parse, resolve } from "node:path";

import Database from "better-sqlite3";
import {
  FABRIC_OPERATIONS,
  MCP_BOOTSTRAP_CREDENTIALS_FEATURE,
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
import { trustedWorkspaceIdentity } from "./workspace-trust.js";

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

export type LifecycleAction =
  | Readonly<{
    action: "workspace-trust";
    outcome: "resolved";
    mutated: false;
    trustRecordDigest: string;
  }>
  | Readonly<{
    action: "daemon";
    outcome: "attached" | "started";
    mutated: boolean;
    pid: number;
    socketPath: string;
  }>
  | Readonly<{
    action: "seat-generation";
    outcome: "installed" | "replayed";
    mutated: boolean;
    generation: string;
    previousGeneration: string | null;
  }>
  | Readonly<{
    action: "legacy-bootstrap-provenance";
    outcome: "recorded";
    mutated: false;
    generation: string;
  }>
  | Readonly<{
    action: "identity-smoke";
    outcome: "passed" | "failed";
    mutated: false;
    deadlineMs: number;
    elapsedMs: number;
    agentId: string | null;
    mailboxWatermark: number | null;
    code: string | null;
  }>;

/**
 * One concise machine-readable record of every automatic action this lifecycle
 * call took.
 *
 * `mutated` is the idempotency assertion surface, and it means exactly one
 * thing: no logical custody state changed. That is the daemon process
 * identity, the active seat generation and its installed roster, and the
 * database rows behind them. A converged repeat invocation reports `false`.
 *
 * It is deliberately not a claim that no bytes were written. A replay still
 * stages the roster through a private temporary tree and re-verifies the
 * installed files before discarding it, which touches directory metadata.
 * Callers reasoning about disk effects must observe the filesystem; callers
 * reasoning about whether a seat rotated or a daemon restarted read this.
 */
export type LifecycleActionReceipt = Readonly<{
  schemaVersion: 1;
  kind: "agent-fabric-lifecycle-action";
  canonicalRoot: string;
  seat: "claude" | "codex";
  generation: string;
  mutated: boolean;
  healthy: boolean;
  actions: readonly LifecycleAction[];
}>;

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
    readonly code: "WORKSPACE_NOT_TRUSTED" | "BOOTSTRAP_GENERATION_CHANGED",
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

type GitWorkspace = Readonly<{ root: string; linkedWorktree: boolean }>;

function isMissingPathError(error: unknown): boolean {
  return typeof error === "object" && error !== null && "code" in error &&
    (error.code === "ENOENT" || error.code === "ENOTDIR");
}

function hasErrorCode(error: unknown, code: string): boolean {
  return typeof error === "object" && error !== null && "code" in error && error.code === code;
}

// Gitfiles also identify submodules, so the file alone does not prove a linked
// worktree. Only its canonical gitdir under another repository's
// `.git/worktrees/` boundary activates the user-only exception guidance.
function pointsToLinkedWorktree(gitDirectory: string, workspaceRoot: string): boolean {
  let candidate = gitDirectory;
  for (;;) {
    const parent = dirname(candidate);
    if (basename(parent) === "worktrees" && basename(dirname(parent)) === ".git") {
      return dirname(dirname(parent)) !== workspaceRoot;
    }
    if (parent === candidate) return false;
    candidate = parent;
  }
}

export async function nearestGitWorkspace(canonicalRoot: string): Promise<GitWorkspace | null> {
  let candidate = canonicalRoot;
  for (;;) {
    const filesystemRoot = candidate === parse(candidate).root;
    let marker: Awaited<ReturnType<typeof lstat>>;
    try {
      marker = await lstat(join(candidate, ".git"));
    } catch (error: unknown) {
      // macOS may report EINVAL rather than ENOENT for `/.git` in a sandbox.
      // It is a missing root marker only at this terminal probe; the same
      // error anywhere else leaves the boundary indeterminate and fail-closed.
      if (!isMissingPathError(error) && !(filesystemRoot && hasErrorCode(error, "EINVAL"))) throw error;
      if (filesystemRoot) return null;
      candidate = dirname(candidate);
      continue;
    }
    if (marker.isDirectory()) return { root: candidate, linkedWorktree: false };
    if (marker.isFile()) {
      const match = /^gitdir:\s*(?<path>.+?)\s*$/u.exec(await readFile(join(candidate, ".git"), "utf8"));
      const gitDirectory = match?.groups?.path;
      if (gitDirectory === undefined) throw new Error("Git workspace marker is not a gitdir file");
      const canonicalGitDirectory = await realpath(resolve(candidate, gitDirectory));
      return {
        root: candidate,
        linkedWorktree: pointsToLinkedWorktree(canonicalGitDirectory, candidate),
      };
    }
    throw new Error("Git workspace marker must be a directory or regular file");
  }
}

export async function repositoryCollectionChildren(canonicalRoot: string): Promise<string[]> {
  const children = await readdir(canonicalRoot, { withFileTypes: true });
  const repositories: string[] = [];
  for (const child of children) {
    if (!child.isDirectory()) continue;
    try {
      const marker = await lstat(join(canonicalRoot, child.name, ".git"));
      if (marker.isDirectory() || marker.isFile()) repositories.push(join(canonicalRoot, child.name));
    } catch (error: unknown) {
      if (!isMissingPathError(error)) throw error;
    }
  }
  return repositories;
}

const WORKSPACE_FILE_MARKERS = new Set([
  "AGENTS.md",
  "CLAUDE.md",
  "package.json",
  "pyproject.toml",
  "Cargo.toml",
  "go.mod",
]);

function workspaceMarkerKind(name: string): "file" | "directory" | null {
  if (name === ".claude") return "directory";
  if (WORKSPACE_FILE_MARKERS.has(name) || name.endsWith(".code-workspace")) return "file";
  return null;
}

async function hasWorkspaceMarker(canonicalRoot: string): Promise<boolean> {
  const children = await readdir(canonicalRoot, { withFileTypes: true });
  for (const child of children) {
    const kind = workspaceMarkerKind(child.name);
    if (kind === null) continue;
    try {
      // Resolve before inspecting so a project marker symlinked into a child
      // repository is treated the same as a marker stored at this root.
      const marker = await lstat(await realpath(join(canonicalRoot, child.name)));
      if ((kind === "file" && marker.isFile()) || (kind === "directory" && marker.isDirectory())) return true;
    } catch (error: unknown) {
      if (!isMissingPathError(error)) throw error;
    }
  }
  return false;
}

// The collection heuristic is intentionally shallow: direct repository
// children show that trusting this directory would grant sibling authority,
// while recursively searching would misclassify an ordinary project that
// happens to contain nested fixtures or vendored repositories. A conventional
// marker at this exact root is an explicit project signal, even when the
// project deliberately composes several repositories.
export async function looksLikeRepositoryCollection(canonicalRoot: string): Promise<boolean> {
  if ((await repositoryCollectionChildren(canonicalRoot)).length <= 1) return false;
  return !await hasWorkspaceMarker(canonicalRoot);
}

async function workspaceTrustRecoveryMessage(
  canonicalRoot: string,
  environment: NodeJS.ProcessEnv = process.env,
): Promise<string> {
  const home = await realpath(homedir());
  if (canonicalRoot === parse(canonicalRoot).root) {
    return `Fabric bootstrap cannot proceed from ${shellQuote(canonicalRoot)}: the filesystem root can never be trusted because root-wide authority is forbidden by policy. Choose the exact repository root or current non-Git project directory instead`;
  }
  if (canonicalRoot === home) {
    return `Fabric bootstrap cannot proceed from ${shellQuote(canonicalRoot)}: this exact path can never be trusted because home-wide trust is forbidden by policy. Choose the exact repository root or current non-Git project directory instead`;
  }
  const workspace = await nearestGitWorkspace(canonicalRoot);
  if (workspace !== null && workspace.root === parse(workspace.root).root) {
    return `Fabric bootstrap cannot proceed from ${shellQuote(workspace.root)}: the filesystem root can never be trusted because root-wide authority is forbidden by policy. Choose the exact repository root or current non-Git project directory instead`;
  }
  if (workspace?.root === home) {
    return `Fabric bootstrap cannot proceed from ${shellQuote(workspace.root)}: this exact path can never be trusted because home-wide trust is forbidden by policy. Choose the exact repository root or current non-Git project directory instead`;
  }
  if (workspace?.linkedWorktree === true) {
    return `Fabric bootstrap cannot proceed from ${shellQuote(workspace.root)} because it is a linked worktree; granting a worktree-path trust exception is a user-only decision, and the agent must not run a trust command unprompted`;
  }
  if (workspace === null && await looksLikeRepositoryCollection(canonicalRoot)) {
    return `Fabric bootstrap cannot proceed from ${shellQuote(canonicalRoot)} because it looks like a parent collection of several repositories; policy forbids trusting a parent or collection directory. Run fabric_bootstrap again from inside the specific project it actually needs`;
  }
  if (workspace !== null && workspace.root !== canonicalRoot) {
    return `Fabric bootstrap requires the exact current project root to be trusted; run ${fabricCliCommand({ environment })} workspace trust ${shellQuote(workspace.root)}; then retry fabric_bootstrap from ${shellQuote(workspace.root)}`;
  }
  return `Fabric bootstrap requires the exact current project root to be trusted; run ${fabricCliCommand({ environment })} workspace trust ${shellQuote(workspace?.root ?? canonicalRoot)}; then retry fabric_bootstrap`;
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
  smokeDeadlineMs?: number;
}): Promise<InstalledBootstrapMcpSeat> {
  const seat = parseMcpSeat(input.environment.AGENT_FABRIC_SEAT ?? "");
  if (seat !== "claude" && seat !== "codex") {
    throw new Error(
      `MCP bootstrap creates only chair seats claude or codex; run ` +
      `${fabricCliCommand({ environment: input.environment })} mcp peer-provision --project ${shellQuote(input.cwd)} --seat ${seat} instead`,
    );
  }
  const canonicalRoot = await realpath(input.cwd);
  let identity: Awaited<ReturnType<typeof trustedWorkspaceIdentity>>;
  try {
    identity = await trustedWorkspaceIdentity({
      stateDirectory: input.paths.stateDirectory,
      canonicalRoot,
    });
  } catch (cause: unknown) {
    const message = await workspaceTrustRecoveryMessage(canonicalRoot, input.environment).catch(
      () => `Fabric bootstrap cannot safely determine the trust boundary for ${shellQuote(canonicalRoot)}, so no trust command is suggested. Inspect the workspace and retry from the exact repository root or current non-Git project directory`,
    );
    throw new McpBootstrapError(
      "WORKSPACE_NOT_TRUSTED",
      message,
      { cause },
    );
  }
  // Read before the daemon can rotate anything: comparing this pointer with the
  // generation the daemon replays is what distinguishes an installed roster
  // from a replayed one without inferring it from timing or file mtimes.
  const generationBefore = await readActiveSeatGeneration({
    stateDirectory: input.paths.stateDirectory,
    projectPath: identity.canonicalRoot,
  }).catch(() => null);
  let daemonHandle: Awaited<ReturnType<typeof startFabricDaemon>>;
  try {
    daemonHandle = await startFabricDaemon(
      defaultDaemonStartOptions(input.paths, { environment: input.environment }),
    );
  } catch (cause: unknown) {
    if (!isSchemaCutoverRefusal(cause)) throw cause;
    throw new McpBootstrapSchemaCutoverGateError(
      schemaCutoverGate(input.paths.databasePath, cause, input.environment),
      { cause },
    );
  }
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
    const selected = installed.find((candidate) => candidate.seat === seat);
    const selectedCredential = result.credentials.find((candidate) => candidate.seat === seat)?.capability;
    if (selected === undefined || selectedCredential === undefined) throw new Error("bootstrap did not install the caller seat");
    const smoke = await identityMailboxSmoke({
      socketPath: daemonHandle.address.path,
      credential: selectedCredential,
      deadlineMs: input.smokeDeadlineMs ?? IDENTITY_SMOKE_DEADLINE_MS,
    });
    const replayed = generationBefore?.generation === result.generation;
    const actions: LifecycleAction[] = [
      {
        action: "workspace-trust",
        outcome: "resolved",
        mutated: false,
        trustRecordDigest: identity.trustRecordDigest,
      },
      {
        action: "daemon",
        outcome: daemonHandle.ownsProcess ? "started" : "attached",
        mutated: daemonHandle.ownsProcess,
        pid: daemonHandle.pid,
        socketPath: daemonHandle.address.path,
      },
      {
        action: "seat-generation",
        outcome: replayed ? "replayed" : "installed",
        mutated: !replayed,
        generation: result.generation,
        previousGeneration: result.expectedPreviousGeneration,
      },
      ...(legacyBootstrapProvenanceRecorded
        ? [{
          action: "legacy-bootstrap-provenance" as const,
          outcome: "recorded" as const,
          mutated: false as const,
          generation: result.generation,
        }]
        : []),
      smoke,
    ];
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
  } finally {
    try {
      await daemon?.close();
    } finally {
      daemonHandle.release();
    }
  }
}
