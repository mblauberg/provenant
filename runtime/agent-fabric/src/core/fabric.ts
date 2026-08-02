import { createHash, createHmac, randomBytes } from "node:crypto";
import { existsSync, linkSync, mkdirSync, readFileSync, realpathSync, renameSync, statSync, unlinkSync, writeFileSync } from "node:fs";
import { basename, dirname, isAbsolute, join, normalize, posix, relative, resolve, sep } from "node:path";

import type Database from "better-sqlite3";
import { v7 as uuidv7 } from "uuid";
import {
  ACTIVITY_NARRATIVE_GROUPING_FEATURE, AGENT_TOPOLOGY_PROJECTION_FEATURE, GATE_SYSTEM_SUPERSESSION_FEATURE, WORK_FACTS_PROJECTION_FEATURE,
  DECLARED_RUN_PROGRESS_FEATURE, RUN_IDENTITY_PROJECTION_FEATURE, RUN_LIFECYCLE_FACTS_FEATURE,
  NATIVE_NOTIFICATION_PROJECTION_FEATURE, RUN_SESSION_PROJECTION_FEATURE,
  authorityEnvelopeV2Contained, parseAuthorityEnvelopeV2,
  parseEvidenceArtifactRegistration,
  type AgentCustodyResult,
  type EvidenceArtifactRegistration,
  type EvidencePublishRequest,
  type GateOperationTarget,
  type HerdrSteerDispatchResult,
  type LifecycleCurrentStateV1,
  type OperationInputMap,
  type ProjectSessionLaunchPacketPreparation,
  type ProjectSessionLaunchPacketPrepareRequest,
  type ProtocolOperation,
  type VerifiedProtocolCredential,
} from "@local/agent-fabric-protocol";

import { readStoredAuthority } from "../authority/stored-authority.js";
import { resolveFabricRoots } from "../domain/fabric-roots.js";

import type {
  AuthorityInput,
  DisclosurePolicy,
  DisclosureTarget,
  FabricOpenOptions,
  MessageInput,
  RecoveryEvidence,
} from "../domain/types.js";
import { isBudgetUnitKey } from "../domain/unit-keys.js";
import {
  FABRIC_OPERATIONS,
  OPERATION_REGISTRY,
  expandAuthorityActions,
  isReadFabricOperation,
  type FabricOperation,
} from "../domain/operations.js";
import { CommandJournal } from "../application/command-journal.js";
import { ProviderActionAdmissionCoordinator } from "../application/provider-action-admission.js";
import { ProviderSessionCoordinator } from "../application/provider-session-coordinator.js";
import { FabricError } from "../errors.js";
import { AdapterSupervisor } from "../adapters/supervisor.js";
import {
  parseAgentBridgeCapability,
  parseChairLaunchProviderResult,
  ProviderAdapterError,
  type AgentBridgeCapability,
} from "../adapters/providers/types.js";
import { projectFabricReceipt } from "../exports/projector.js";
import { assertFabricReceiptSchema } from "../exports/schema.js";
import { openFabricDatabase } from "../persistence/sqlite.js";
import { renderSafePreview } from "../visibility/safe-preview.js";
import {
  OperatorStore,
  type AuthenticatedOperatorCredential,
  type LocalOperatorConsoleCapabilityInput,
  type LocalOperatorConsoleCapabilityResult,
  type LocalOperatorConsoleSessionCapabilityResult,
  type LocalOperatorPrincipalRotationInput,
  type LocalOperatorPrincipalRotationResult,
  type LocalOperatorProvisioningInput,
  type LocalOperatorProvisioningResult,
  type LocalOperatorSessionCapabilityInput,
  type LocalOperatorSessionCapabilityResult,
  type LocalOperatorTakeoverCapabilityInput,
  type LocalOperatorTakeoverCapabilityResult,
} from "../operator/store.js";
import { OperatorProjectionStore } from "../operator/projection-store.js";
import {
  GitRepositoryReadService,
  type GitHostedChecksPort,
} from "../operator/git-repository-read.js";
import {
  HerdrDaemonIntegration,
  type HerdrDaemonActionRequest,
  type HerdrDaemonActionResult,
  type HerdrDaemonIntegrationConfiguration,
  type HerdrDirectSteerRequest,
  type HerdrDirectSteerResult,
} from "../integrations/herdr-daemon-integration.js";
import { ArtifactContentReadService } from "../operator/artifact-content-read.js";
import {
  ExternalEffectService,
  type ExternalEffectEvidencePort,
  type RegisteredEffectPort,
} from "../operator/external-effect-service.js";
import { FixedGitMutationPort, type GitMutationPort } from "../operator/fixed-git-mutation-port.js";
import { TypedGitService, type GitConflictInspectorPort } from "../operator/typed-git-service.js";
import {
  TrustedGitRegistry,
  type TrustedGitConfiguration,
  type TrustedRunGitAllowlist,
} from "../operator/trusted-git-registry.js";
import {
  OperatorActionStore,
  type OperatorActionEffectPort,
  type OperatorActionStatePort,
} from "../operator/action-store.js";
import { LifecycleRecoveryCustodyService } from "../operator/lifecycle-recovery-custody.js";
import {
  assertRunAcceptingWork,
  assertTaskOperationAdmitted,
  resolveTaskBindingForActiveWork,
} from "../operator/task-run-admission.js";
import {
  createProductionOperatorActionPorts,
  type ProductionDaemonStopPort,
} from "../operator/production-action-ports.js";
import { operatorOperationsForActions } from "../operator/protocol-credentials.js";
import type { PublicProtocolContext } from "./public-protocol-context.js";
import {
  attemptDrainedStop as attemptRuntimeDrainedStop,
  attemptIdleStop as attemptRuntimeIdleStop,
  markDaemonRuntimeRunning as markRuntimeEpochRunning,
  recoverDaemonRuntimeEpoch as recoverRuntimeEpoch,
  type IdleElectionPort,
  type IdleStopResult,
  type QuiesceToken,
} from "../lifecycle/global-liveness.js";
import { dispatchAgentProtocol } from "./agent-protocol-dispatch.js";
import { ProjectSessionStore } from "../project-session/store.js";
import { ProjectSessionMembershipStore } from "../project-session/membership-store.js";
import { MailboxCustodyService } from "../project-session/mailbox-custody.js";
import { dispatchProjectDeliveryOperation } from "../project-session/delivery-operation-dispatch.js";
import { CoordinatedWorkstreamStore } from "../project-session/workstream-store.js";
import { RunPlanStore } from "../project-session/run-plan-store.js";
import {
  LaunchCustodyService,
  normaliseLaunchChairAuthority,
  parseLaunchAdapterContract,
  type LaunchAdapterContract,
  type AgentBridgeContract,
  type AgentDispatchHandle,
  type LaunchDispatchHandle,
  type ChairRecoveryDispatchHandle,
  type RetainedChairBridge,
} from "../project-session/launch-custody.js";
import { ProviderAgentCustodyAdapter } from "../project-session/provider-agent-custody.js";
import { ProviderAgentCustodyRecoveryAdapter } from "../project-session/provider-agent-custody-recovery.js";
import {
  ChairLiveHandoffCustodyAdapter,
  type RetainedSuccessorBridgeProbe,
} from "../project-session/chair-live-handoff-custody.js";
import { ChairLiveHandoffCustodyRecoveryAdapter } from "../project-session/chair-live-handoff-custody-recovery.js";
import { ChairRecoveryCustodyService } from "../project-session/chair-recovery-custody.js";
import { LaunchService } from "../project-session/launch-service.js";
import { LaunchSettlement } from "../project-session/launch-settlement.js";
import type { Digest } from "../project-session/launch-contracts.js";
import { reconcileUnknownLaunchUsage as reconcileUnknownLaunchUsageOwner } from "../project-session/launch-usage-reconciliation.js";
import { IntakeStore } from "../project-session/intake-store.js";
import { assertSafeLaunchProviderInput } from "../project-session/provider-input-safety.js";
import {
  ProjectFabricCoreError,
  type AuthenticatedAgentContext,
} from "../project-session/contracts.js";
import type { ProviderActionDispatchRequest } from "../application/provider-action-dispatch-request.js";
import {
  ScopedGateStore,
  assertScopedBarrierAllowed,
  assertScopedOperationAllowed,
  assertScopedTaskReadinessAllowed,
} from "../gates/store.js";
import { HierarchicalAdmissionStore } from "../resources/store.js";
import {
  AtomicDeliveryStore,
  type ResultDeadlinePassInput,
  type ResultDeadlinePassResult,
} from "../results/store.js";
import { NotificationOutbox } from "../attention/outbox.js";
import { MacOsNativeDesktopAdapter } from "../attention/native-desktop.js";
import {
  NativeNotificationWorker,
  type NotificationWorkerPassResult,
} from "../attention/notification-worker.js";
import { FabricClient } from "./client.js";
import type {
  ArtifactResult,
  AuthorityResult,
  BarrierResult,
  BootstrapMcpSeatInput,
  BootstrapMcpSeatResult,
  BudgetDimensionResult,
  BudgetResult,
  CapabilityRotationResult,
  CurrentMcpSeatBindingInput,
  CurrentMcpSeatBindingResult,
  DiscussionGroupInput,
  EventsAfterResult,
  InterventionResult,
  LeaseResult,
  LifecycleCheckpoint,
  LifecycleResult,
  ProofResult,
  ProviderActionResult,
  ReceiptResult,
  RevocationResult,
  TaskResult,
  TeamCreateInput,
  TeamResult,
} from "./contracts.js";
import { currentMcpSeatGeneration } from "./mcp-seat-generation.js";
import { bootstrapCurrentMcpSeat as bootstrapMcpSeatCustody } from "./bootstrap-mcp-custody.js";
import { readCallerIdentity, type CallerIdentity } from "./caller-identity.js";
import { FabricReadPolicy } from "./read-policy.js";
import { ArtifactRegistry } from "../artifacts/registry.js";
import { resolveRunArtifactRoot } from "../artifacts/run-root.js";
import { LifecycleRotationRepository } from "../lifecycle/rotation-repository.js";
import { LifecycleCheckpointPolicy } from "../lifecycle/checkpoint-policy.js";
import { LifecycleAdmission, type LifecycleContinuationInput } from "../lifecycle/admission.js";
import { LifecycleReceiptRepository } from "../lifecycle/receipt-repository.js";
import { LifecycleFinalizer } from "../lifecycle/finalizer.js";
import { LifecycleContinuation } from "../lifecycle/continuation.js";
import { LifecycleRecovery } from "../lifecycle/recovery.js";
import { LifecycleService } from "../lifecycle/service.js";
import { GenerationLossRepository } from "../lifecycle/generation-loss-repository.js";
import { ProviderPayloadAuthority } from "../provider-action/payload-authority.js";
import { ProviderActionExecutor } from "../provider-action/executor.js";
import { ProviderActionState } from "../provider-action/state.js";
import { ProviderActionRecovery } from "../provider-action/recovery.js";
import { ProviderActionCoordinator } from "../provider-action/coordinator.js";
import type {
  LifecycleIntegrityReceiptAuthorityPort,
} from "../lifecycle/receipt-authority.js";
import {
  hasKnownLifecycleReceiptState,
  LifecycleReceiptRecoveryError,
  LifecycleReceiptRecoveryService,
} from "../lifecycle/receipt-recovery.js";
import {
  DAEMON_SHUTDOWN_FABRIC_CLOSE_TIMEOUT,
  DAEMON_SHUTDOWN_FABRIC_CLOSE_TIMEOUT_MS,
  waitWithShutdownDeadline,
} from "../lifecycle/shutdown-deadline.js";

export { FabricClient } from "./client.js";

export type FabricOperatorActionPorts = {
  statePort: OperatorActionStatePort;
  effectPort: OperatorActionEffectPort;
};

export type FabricRuntimeOpenOptions = FabricOpenOptions & {
  operatorActionPorts?: FabricOperatorActionPorts;
  daemonStopPort?: ProductionDaemonStopPort;
  fabricSocketPath?: string;
  gitHostedChecks?: GitHostedChecksPort;
  externalEffects?: Readonly<{
    registry: readonly RegisteredEffectPort[];
    evidence: ExternalEffectEvidencePort;
  }>;
  herdr?: HerdrDaemonIntegrationConfiguration;
  gitMutationPort?: GitMutationPort;
  gitConflictInspector?: GitConflictInspectorPort;
  trustedGitConfiguration?: TrustedGitConfiguration;
  lifecycleReceiptAuthority?: LifecycleIntegrityReceiptAuthorityPort;
  fault?: (label: string) => void;
};

type Row = Record<string, unknown>;
type TaskCreateInput = {
  taskId: string;
  authorityId: string;
  eligibleAgentIds: string[];
  proposedOwnerAgentId?: string;
  participantAgentIds?: string[];
  dependencies?: string[];
  expectedArtifacts?: string[];
  objectiveChecks?: string[];
  objective: string;
  baseRevision: string;
  commandId: string;
};
type StoredAuthority = AuthorityInput;

function isRow(value: unknown): value is Row {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stringField(row: Row, field: string): string {
  const value = row[field];
  if (typeof value !== "string") {
    throw new Error(`database field ${field} is not a string`);
  }
  return value;
}

function numberField(row: Row, field: string): number {
  const value = row[field];
  if (typeof value !== "number") {
    throw new Error(`database field ${field} is not a number`);
  }
  return value;
}

function rowOrNotFound(value: unknown, label: string): Row {
  if (!isRow(value)) {
    throw new FabricError("NOT_FOUND", `${label} was not found`);
  }
  return value;
}

function assertActiveMcpSeatGeneration(row: Row, database: Database.Database, tokenHash: string): void {
  const generation = row.mcp_seat_generation;
  if (generation === null || generation === undefined) return;
  if (typeof generation !== "string" || row.active_mcp_seat_generation !== generation) {
    // Lazily check chair lease status only on error
    const leaseStatus = database.prepare(`
      SELECT lease.status
        FROM mcp_seat_generation_members member
        JOIN mcp_seat_generations generation ON generation.generation=member.generation
        LEFT JOIN run_chair_leases lease ON lease.lease_id=generation.chair_lease_id
       WHERE member.token_hash=?
    `).get(tokenHash) as { status?: unknown } | undefined;
    
    if (leaseStatus?.status !== "active") {
      throw new FabricError("AUTHENTICATION_FAILED", "capability belongs to an MCP seat generation whose chair lease is not active");
    }
    throw new FabricError("AUTHENTICATION_FAILED", "capability belongs to an inactive MCP seat generation");
  }
}

function canonicalJson(value: unknown): string {
  if (value === null || typeof value === "boolean" || typeof value === "number" || typeof value === "string") {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) {
    return `[${value.map(canonicalJson).join(",")}]`;
  }
  if (isRow(value)) {
    return `{${Object.keys(value)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalJson(value[key])}`)
      .join(",")}}`;
  }
  throw new TypeError("value is not JSON-compatible");
}

function sha256(value: string): string {
  return createHash("sha256").update(value).digest("hex");
}

function sha256Digest(value: string): string {
  return `sha256:${sha256(value)}`;
}

function canonicalPath(path: string): string {
  if (
    path.length === 0 ||
    path.includes("\0") ||
    path.split(/[\\/]/u).includes("..") ||
    /[*?[\]{}]/u.test(path)
  ) {
    throw new FabricError("AUTHORITY_WIDENING", `unsafe path: ${path}`);
  }
  let cursor = resolve(path);
  const suffix: string[] = [];
  while (!existsSync(cursor)) {
    const parent = dirname(cursor);
    if (parent === cursor) {
      throw new FabricError("AUTHORITY_WIDENING", `path has no resolvable ancestor: ${path}`);
    }
    suffix.unshift(basename(cursor));
    cursor = parent;
  }
  const resolved = resolve(realpathSync(cursor), ...suffix);
  return normalize(resolved).replaceAll(sep, "/");
}

function pathContains(parent: string, child: string): boolean {
  const rel = posix.relative(parent, child);
  return rel === "" || (rel !== ".." && !rel.startsWith("../") && !posix.isAbsolute(rel));
}

function scopesOverlap(left: string, right: string): boolean {
  return pathContains(left, right) || pathContains(right, left);
}

function isAbsoluteOnAnyPlatform(path: string): boolean {
  return isAbsolute(path) || /^[A-Za-z]:[\\/]/u.test(path) || /^[\\/]{2}/u.test(path);
}

function canonicalWorkspaceRoot(path: string): string {
  if (!isAbsoluteOnAnyPlatform(path)) {
    throw new FabricError("AUTHORITY_WIDENING", "configured workspace root must be absolute");
  }
  return canonicalPath(path);
}

function canonicalAuthorityPath(workspaceRoot: string, path: string): string {
  if (
    path.length === 0 ||
    path.includes("\0") ||
    isAbsoluteOnAnyPlatform(path) ||
    path.split(/[\\/]/u).includes("..") ||
    /[*?[\]{}]/u.test(path)
  ) {
    throw new FabricError("AUTHORITY_WIDENING", `unsafe workspace-relative path: ${path}`);
  }
  let cursor = resolve(workspaceRoot, path);
  const suffix: string[] = [];
  while (!existsSync(cursor)) {
    const parent = dirname(cursor);
    if (parent === cursor) {
      throw new FabricError("AUTHORITY_WIDENING", `path has no resolvable ancestor: ${path}`);
    }
    suffix.unshift(basename(cursor));
    cursor = parent;
  }
  const resolved = resolve(realpathSync(cursor), ...suffix);
  const rel = relative(workspaceRoot, resolved);
  if (rel === ".." || rel.startsWith(`..${sep}`) || isAbsolute(rel)) {
    throw new FabricError("AUTHORITY_WIDENING", `workspace-relative path escapes configured root: ${path}`);
  }
  return rel === "" ? "." : normalize(rel).replaceAll(sep, "/");
}

const DISCLOSURE_TARGETS = ["local", "approved-provider", "external"] as const satisfies readonly DisclosureTarget[];
const disclosureTargets = new Set<string>(DISCLOSURE_TARGETS);

function isDisclosureTarget(value: string): value is DisclosureTarget {
  return disclosureTargets.has(value);
}

function normaliseDisclosure(value: AuthorityInput["disclosure"]): DisclosurePolicy {
  if (!isRow(value)) throw new FabricError("AUTHORITY_WIDENING", "authority disclosure policy is invalid");
  if (value.level === "allowed" && !("scopes" in value)) return { level: "allowed" };
  if (value.level === "forbidden" && !("scopes" in value)) return { level: "forbidden" };
  if (value.level === "scoped" && "scopes" in value && isStringArray(value.scopes)) {
    const scopes = [...new Set(value.scopes)];
    if (scopes.length === 0 || scopes.length === DISCLOSURE_TARGETS.length || scopes.some((scope) => !disclosureTargets.has(scope))) {
      throw new FabricError("AUTHORITY_WIDENING", "scoped disclosure requires a non-empty proper destination subset");
    }
    return { level: "scoped", scopes: scopes.filter(isDisclosureTarget).sort() };
  }
  throw new FabricError("AUTHORITY_WIDENING", "authority disclosure policy is invalid");
}

function validateIntegerBudget(budget: Record<string, number>): void {
  for (const [dimension, value] of Object.entries(budget)) {
    if (!isBudgetUnitKey(dimension)) {
      throw new FabricError("AUTHORITY_WIDENING", `budget unit key is invalid: ${dimension}`);
    }
    if (!Number.isFinite(value) || value < 0 || !Number.isInteger(value)) {
      throw new FabricError("AUTHORITY_WIDENING", `budget ${dimension} must be a non-negative integer`);
    }
  }
}

function validateBudgetUnitKeys(budget: Record<string, unknown>): void {
  const invalid = Object.keys(budget).find((unit) => !isBudgetUnitKey(unit));
  if (invalid !== undefined) throw new FabricError("BUDGET_EXCEEDED", `budget unit key is invalid: ${invalid}`);
}

export function normaliseAuthority(
  authority: AuthorityInput,
  workspaceRoot: string,
): StoredAuthority {
  let parsed: AuthorityInput;
  try {
    parsed = parseAuthorityEnvelopeV2(authority, "authority");
  } catch (cause: unknown) {
    throw new FabricError(
      "AUTHORITY_WIDENING",
      cause instanceof Error ? cause.message : "authority does not match AuthorityEnvelopeV2",
      { cause },
    );
  }
  validateIntegerBudget(parsed.budget);
  const expires = Date.parse(parsed.expiresAt);
  if (!Number.isFinite(expires)) {
    throw new FabricError("AUTHORITY_WIDENING", "authority expiry must be an ISO timestamp");
  }
  const actionExpansion = expandAuthorityActions(parsed.actions);
  if (!actionExpansion.ok) {
    throw new FabricError("AUTHORITY_WIDENING", `unknown authority actions: ${actionExpansion.unknownActions.join(", ")}`);
  }
  const deniedActionExpansion = expandAuthorityActions(parsed.deniedActions);
  if (!deniedActionExpansion.ok) {
    throw new FabricError("AUTHORITY_WIDENING", `unknown denied authority actions: ${deniedActionExpansion.unknownActions.join(", ")}`);
  }
  const canonicalisePath = (path: string): string => canonicalAuthorityPath(workspaceRoot, path);
  const workspaceRoots = [...new Set(parsed.workspaceRoots.map(canonicalisePath))].sort();
  const sourcePaths = [...new Set(parsed.sourcePaths.map(canonicalisePath))].sort();
  const artifactPaths = [...new Set(parsed.artifactPaths.map(canonicalisePath))].sort();
  if (workspaceRoots.length === 0 || sourcePaths.some((path) => !workspaceRoots.some((root) => pathContains(root, path))) || artifactPaths.some((path) => !workspaceRoots.some((root) => pathContains(root, path)))) {
    throw new FabricError("AUTHORITY_WIDENING", "source and artifact paths must be inside an authority workspace root");
  }
  return {
    schemaVersion: 2,
    approval: parsed.approval,
    workspaceRoots,
    sourcePaths,
    artifactPaths,
    actions: actionExpansion.operations,
    deniedPaths: [...new Set(parsed.deniedPaths.map(canonicalisePath))].sort(),
    deniedActions: [...new Set(deniedActionExpansion.operations)].sort(),
    prohibitedActions: [...new Set(parsed.prohibitedActions)].sort(),
    disclosure: normaliseDisclosure(parsed.disclosure),
    secrets: parsed.secrets.access === "none"
      ? parsed.secrets
      : { access: "use-without-disclosure", references: [...parsed.secrets.references].sort() },
    deployment: !parsed.deployment.allowed
      ? parsed.deployment
      : { allowed: true, targets: [...parsed.deployment.targets].sort() },
    irreversibleActions: !parsed.irreversibleActions.allowed
      ? parsed.irreversibleActions
      : { allowed: true, actionIds: [...parsed.irreversibleActions.actionIds].sort() },
    network: parsed.network.toolEgress === "none"
      ? parsed.network
      : { toolEgress: "allowlist", allowedHosts: [...parsed.network.allowedHosts].sort() },
    expiresAt: new Date(expires).toISOString(),
    budget: Object.fromEntries(Object.entries(parsed.budget).sort(([left], [right]) => left.localeCompare(right))),
  };
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function isNumberRecord(value: unknown): value is Record<string, number> {
  return isRow(value) && Object.values(value).every((item) => typeof item === "number");
}

function parseAuthority(record: Row): StoredAuthority {
  return readStoredAuthority(record);
}

function authorityContained(child: StoredAuthority, parent: StoredAuthority): boolean {
  return authorityEnvelopeV2Contained(child, parent);
}

function capabilityToken(key: string, runId: string, agentId: string, principalGeneration: number): string {
  return `afc_${createHmac("sha256", key).update(canonicalJson({ runId, agentId, principalGeneration })).digest("base64url")}`;
}

function isLeaseResult(value: unknown): value is LeaseResult {
  return (
    isRow(value) &&
    typeof value.leaseId === "string" &&
    typeof value.holderAgentId === "string" &&
    typeof value.generation === "number" &&
    (value.status === "active" || value.status === "quarantined") &&
    isStringArray(value.scope)
  );
}

function isAuthorityResult(value: unknown): value is AuthorityResult {
  return isRow(value) && typeof value.authorityId === "string";
}

function isTaskState(value: unknown): value is TaskResult["state"] {
  return value === "blocked" || value === "ready" || value === "active" || value === "complete" || value === "cancelled" || value === "degraded";
}

function isTaskResult(value: unknown): value is TaskResult {
  return (
    isRow(value) &&
    typeof value.taskId === "string" &&
    (typeof value.ownerAgentId === "string" || value.ownerAgentId === null) &&
    isTaskState(value.state) &&
    typeof value.revision === "number" &&
    typeof value.ownerLeaseGeneration === "number" &&
    (typeof value.proposedOwnerAgentId === "string" || value.proposedOwnerAgentId === null) &&
    isStringArray(value.dependencies)
  );
}

function taskResultFromRow(
  row: Row,
  proposedOwnerAgentId: string | null = null,
  dependencies: string[] = [],
): TaskResult {
  const ownerValue = row.owner_agent_id;
  const stateValue = row.state;
  if ((typeof ownerValue !== "string" && ownerValue !== null) || !isTaskState(stateValue)) {
    throw new Error("stored task is invalid");
  }
  return {
    taskId: stringField(row, "task_id"),
    ownerAgentId: ownerValue,
    state: stateValue,
    revision: numberField(row, "revision"),
    ownerLeaseGeneration: numberField(row, "owner_lease_generation"),
    proposedOwnerAgentId,
    dependencies,
  };
}

function isReceiptResult(value: unknown): value is ReceiptResult {
  return (
    isRow(value) &&
    typeof value.relativePath === "string" &&
    /^fabric-receipt-[0-9a-f]{64}\.json$/u.test(value.relativePath) &&
    (value.schemaVersion === 1 || value.schemaVersion === 2) &&
    typeof value.sha256 === "string"
  );
}

function isProofResult(value: unknown): value is ProofResult {
  return isRow(value) && typeof value.proofId === "string";
}

function isRevocationResult(value: unknown): value is RevocationResult {
  return isRow(value) && value.revoked === true;
}

function isArtifactResult(value: unknown): value is ArtifactResult {
  return (
    isRow(value) &&
    typeof value.artifactId === "string" &&
    typeof value.relativePath === "string" &&
    typeof value.sha256 === "string"
  );
}

function isEvidenceArtifactRegistration(value: unknown): value is EvidenceArtifactRegistration {
  try {
    parseEvidenceArtifactRegistration(value);
    return true;
  } catch {
    return false;
  }
}

function isBarrierResult(value: unknown): value is BarrierResult {
  return (
    isRow(value) &&
    (value.scope === "run" || value.scope === "stage") &&
    value.closed === true &&
    isReceiptResult(value.receipt)
  );
}

function assertAdapterOperation(capabilities: unknown, operation: string): void {
  if (
    !isRow(capabilities) || capabilities.actionJournal !== true ||
    !Array.isArray(capabilities.operations) ||
    !capabilities.operations.every((value) => typeof value === "string") ||
    (!capabilities.operations.includes(operation) &&
      !(operation !== "spawn" && operation !== "attach" && capabilities.operations.includes("dispatch")))
  ) {
    throw new FabricError("CAPABILITY_FORBIDDEN", `adapter does not advertise durable ${operation}`);
  }
}

function isInterventionResult(value: unknown): value is InterventionResult {
  return isRow(value) && typeof value.interventionId === "string";
}

function isTeamResult(value: unknown): value is TeamResult {
  return (
    isRow(value) &&
    typeof value.teamId === "string" &&
    (typeof value.parentTeamId === "string" || value.parentTeamId === null) &&
    typeof value.depth === "number" &&
    typeof value.leaderAgentId === "string" &&
    typeof value.rootTaskId === "string" &&
    isStringArray(value.ownedTaskIds) &&
    isStringArray(value.memberAgentIds) &&
    typeof value.budgetId === "string" &&
    (value.state === "active" || value.state === "frozen" || value.state === "barrier-closed") &&
    typeof value.generation === "number" &&
    (typeof value.successorAgentId === "string" || value.successorAgentId === null) &&
    (value.initialMembers === undefined || (
      Array.isArray(value.initialMembers) && value.initialMembers.every((member) =>
        isRow(member) && typeof member.agentId === "string" && typeof member.authorityId === "string")
    )) &&
    Array.isArray(value.discussionGroups) &&
    isNumberRecord(value.reservedBudget)
  );
}

function isBudgetResult(value: unknown): value is BudgetResult {
  return (
    isRow(value) &&
    typeof value.budgetId === "string" &&
    (typeof value.parentBudgetId === "string" || value.parentBudgetId === null) &&
    (value.state === "active" || value.state === "usage-unknown" || value.state === "released") &&
    isRow(value.dimensions) &&
    isNumberRecord(value.returned)
  );
}

function publicHerdrSteerResult(result: HerdrDirectSteerResult): HerdrSteerDispatchResult {
  if (result.status === "rejected" || result.status === "unavailable") return result;
  const base = { actionId: result.actionId, revision: result.revision };
  if (result.status === "prepared" || result.status === "dispatched") {
    return { ...base, status: result.status };
  }
  if (result.status === "ambiguous") {
    if (result.ambiguityReason === undefined) {
      throw new TypeError("ambiguous Herdr steering action has no bounded reason");
    }
    return { ...base, status: "ambiguous", reason: result.ambiguityReason };
  }
  if (
    result.receipt?.status !== "dispatched-unconfirmed" ||
    result.receipt.operation !== "steer.inject-fire-and-forget"
  ) throw new TypeError("terminal Herdr steering action has no direct-steer receipt");
  return { ...base, status: "terminal", receipt: result.receipt };
}

export class Fabric {
  readonly #database: Database.Database;
  readonly #workspaceRoots: string[];
  readonly #clock: () => number;
  readonly #productRoot: string;
  readonly #adapters: NonNullable<FabricOpenOptions["adapters"]>;
  readonly #readPolicy: FabricReadPolicy;
  readonly #commandJournal: CommandJournal;
  readonly #providerActionAdmission: ProviderActionAdmissionCoordinator;
  readonly #payloadAuthority: ProviderPayloadAuthority;
  readonly #lifecycleRotations: LifecycleRotationRepository;
  readonly #checkpointPolicy: LifecycleCheckpointPolicy;
  readonly #admission: LifecycleAdmission;
  readonly #finalizer: LifecycleFinalizer;
  readonly #continuation: LifecycleContinuation;
  readonly #recovery: LifecycleRecovery;
  readonly #service: LifecycleService;
  readonly #lifecycleReceipts: LifecycleReceiptRepository;
  readonly #generationLosses: GenerationLossRepository;
  readonly #lifecycleReceiptAuthority: LifecycleIntegrityReceiptAuthorityPort | undefined;
  readonly #lifecycleReceiptRecovery: LifecycleReceiptRecoveryService | undefined;
  readonly #capabilityKey: string;
  readonly #fabricSocketPath: string | undefined;
  readonly #adapterSupervisor: AdapterSupervisor;
  readonly #providerSessions: ProviderSessionCoordinator;
  readonly #maximumConcurrentProviderTurns: number;
  readonly #providerActionState: ProviderActionState;
  readonly #providerActionExecutor: ProviderActionExecutor;
  readonly #providerActionRecovery: ProviderActionRecovery;
  readonly #providerActionCoordinator: ProviderActionCoordinator;
  #closing = false;
  readonly #operatorStore: OperatorStore;
  readonly #operatorProjections: OperatorProjectionStore;
  readonly #gitRepositoryReads: GitRepositoryReadService;
  readonly #artifactContentReads: ArtifactContentReadService;
  readonly #operatorActions: OperatorActionStore;
  readonly #launchCustody: LaunchCustodyService | undefined;
  readonly #projectSessions: ProjectSessionStore;
  readonly #memberships: ProjectSessionMembershipStore;
  readonly #mailboxCustody: MailboxCustodyService;
  readonly #intakes: IntakeStore;
  readonly #gates: ScopedGateStore;
  readonly #resources: HierarchicalAdmissionStore;
  readonly #workstreams: CoordinatedWorkstreamStore;
  readonly #runPlans: RunPlanStore;
  readonly #results: AtomicDeliveryStore;
  readonly #notifications: NotificationOutbox;
  readonly #notificationWorker: NativeNotificationWorker;
  readonly #artifactRegistry: ArtifactRegistry;
  readonly #externalEffects: ExternalEffectService | undefined;
  readonly #herdr: HerdrDaemonIntegration;
  readonly #herdrConfigured: boolean;
  readonly #typedGit: TypedGitService;
  readonly #trustedGitRegistry: TrustedGitRegistry;
  readonly #trustedRunGitAllowlists: readonly TrustedRunGitAllowlist[];
  readonly #fault: (label: string) => void;

  constructor(options: FabricRuntimeOpenOptions) {
    const clock = options.clock ?? Date.now;
    this.#productRoot = resolveFabricRoots({ productRootFlag: options.productRoot }).productRoot;
    this.#clock = () => {
      const value = clock();
      return value instanceof Date ? value.getTime() : value;
    };
    this.#fault = options.fault ?? (() => undefined);
    this.#workspaceRoots = [...new Set(options.workspaceRoots.map(canonicalWorkspaceRoot))].sort();
    if (this.#workspaceRoots.length === 0) {
      throw new FabricError("AUTHORITY_WIDENING", "fabric requires at least one configured workspace root");
    }
    this.#database = openFabricDatabase(options.databasePath);
    this.#readPolicy = new FabricReadPolicy(this.#database);
    this.#commandJournal = new CommandJournal(this.#database, this.#clock);
    this.#providerActionAdmission = new ProviderActionAdmissionCoordinator({
      database: this.#database,
      clock: this.#clock,
      fault: this.#fault,
    });
    this.#lifecycleRotations = new LifecycleRotationRepository(this.#database);
    this.#checkpointPolicy = new LifecycleCheckpointPolicy({
      database: this.#database,
      clock: this.#clock,
      getTask: this.getTask.bind(this),
      getMailboxState: this.getMailboxState.bind(this),
      getAgentLifecycle: (runId, agentId) => this.#service.getAgentLifecycle(runId, agentId),
    });
    this.#lifecycleReceipts = new LifecycleReceiptRepository(
      this.#database,
      this.#lifecycleRotations,
      this.#clock,
    );
    this.#generationLosses = new GenerationLossRepository(this.#database);
    this.#lifecycleReceiptAuthority = options.lifecycleReceiptAuthority;
    this.#lifecycleReceiptRecovery = options.lifecycleReceiptAuthority === undefined
      ? undefined
      : new LifecycleReceiptRecoveryService(this.#database, options.lifecycleReceiptAuthority);
    this.#artifactRegistry = new ArtifactRegistry(this.#database, this.#clock);
    this.#adapters = options.adapters ?? {};
    this.#payloadAuthority = new ProviderPayloadAuthority({
      database: this.#database,
      clock: this.#clock,
      workspaceRootForRun: (runId) => this.#workspaceRootForRun(runId),
      adapterModelPolicy: (adapterId) => this.#adapter(adapterId).modelPolicy,
    });
    this.#adapterSupervisor = new AdapterSupervisor(this.#adapters);
    this.#maximumConcurrentProviderTurns = options.maximumConcurrentProviderTurns ?? 8;
    this.#providerSessions = new ProviderSessionCoordinator({
      database: this.#database,
      clock: this.#clock,
      maximumConcurrentTurns: this.#maximumConcurrentProviderTurns,
      providerActionAdmission: this.#providerActionAdmission,
    });
    this.#providerActionState = new ProviderActionState({
      database: this.#database,
      clock: this.#clock,
      fault: this.#fault,
      maximumConcurrentProviderTurns: this.#maximumConcurrentProviderTurns,
      isClosing: () => this.#closing,
      settleProviderTurn: (runId, adapterId, actionId, status) =>
        this.#providerSessions.settleTurn(runId, adapterId, actionId, status),
    });
    this.#providerActionExecutor = new ProviderActionExecutor({
      database: this.#database,
      clock: this.#clock,
      commandJournal: this.#commandJournal,
      providerActionAdmission: this.#providerActionAdmission,
      providerSessions: this.#providerSessions,
      requestAdapter: (adapterId, method, params) => this.#requestAdapter(adapterId, method, params),
      getProviderAction: (runId, adapterId, actionId) => this.#providerActionState.get(runId, adapterId, actionId),
      enqueueDeferred: (input) => this.#providerActionState.enqueueDeferred(input),
      persistProviderAction: (runId, adapterId, actionId, raw, result, expectedOwner) =>
        this.#providerActionState.persist(runId, adapterId, actionId, raw, result, expectedOwner),
    });
    this.#providerActionRecovery = new ProviderActionRecovery({
      database: this.#database,
      clock: this.#clock,
      commandJournal: this.#commandJournal,
      assertChair: (runId, actorAgentId) => this.#assertChair(runId, actorAgentId),
      event: (runId, type, actorAgentId, payload) => {
        this.#event(runId, type, actorAgentId, payload);
      },
      requestAdapter: (adapterId, method, params) => this.#requestAdapter(adapterId, method, params),
      payloadAuthority: this.#payloadAuthority,
      state: this.#providerActionState,
      executor: this.#providerActionExecutor,
    });
    this.#providerActionCoordinator = new ProviderActionCoordinator({
      database: this.#database,
      clock: this.#clock,
      fault: this.#fault,
      commandJournal: this.#commandJournal,
      providerActionAdmission: this.#providerActionAdmission,
      providerSessions: this.#providerSessions,
      payloadAuthority: this.#payloadAuthority,
      providerActionState: this.#providerActionState,
      providerActionExecutor: this.#providerActionExecutor,
      providerActionRecovery: this.#providerActionRecovery,
      assertChair: (runId, actorAgentId) => this.#assertChair(runId, actorAgentId),
      assertAdapterEnabled: (adapterId) => {
        this.#adapter(adapterId);
      },
      assertAdapterOperation: (capabilities, operation) => assertAdapterOperation(capabilities, operation),
      requestAdapter: (adapterId, method, params) => this.#requestAdapter(adapterId, method, params),
      isClosing: () => this.#closing,
    });
    this.#capabilityKey = options.capabilityKey ?? randomBytes(32).toString("base64url");
    this.#fabricSocketPath = options.fabricSocketPath;
    this.#admission = new LifecycleAdmission({
      database: this.#database,
      clock: this.#clock,
      fault: this.#fault,
      fabricSocketPath: this.#fabricSocketPath,
      commandJournal: this.#commandJournal,
      providerActionAdmission: this.#providerActionAdmission,
      lifecycleRotations: this.#lifecycleRotations,
      lifecycleReceiptAuthority: this.#lifecycleReceiptAuthority,
      scheduleLifecycleContinuation: (input: LifecycleContinuationInput) => {
        this.#continuation.schedule(input);
      },
    });
    this.#finalizer = new LifecycleFinalizer({
      database: this.#database,
      clock: this.#clock,
      fault: this.#fault,
      lifecycleReceiptAuthority: this.#lifecycleReceiptAuthority,
      lifecycleReceipts: this.#lifecycleReceipts,
      lifecycleRotations: this.#lifecycleRotations,
      admissionSourceVectorDigest: this.#admission.sourceVectorDigest.bind(this.#admission),
      checkpointPolicyRecordOperation: this.#checkpointPolicy.recordOperation.bind(this.#checkpointPolicy),
    });
    this.#continuation = new LifecycleContinuation({
      database: this.#database,
      clock: this.#clock,
      lifecycleRotations: this.#lifecycleRotations,
      adapterSupervisor: this.#adapterSupervisor,
      finalizer: this.#finalizer,
      fabricSocketPath: this.#fabricSocketPath,
      ensureReceiptScope: this.#admission.ensureReceiptScope.bind(this.#admission),
      getGenericPredecessor: (runId, adapterId, actionId) =>
        this.#providerActionState.getGenericPredecessor(runId, adapterId, actionId),
      isClosing: () => this.#closing,
      event: (runId, type, actorAgentId, payload) => {
        this.#event(runId, type, actorAgentId, payload);
      },
    });
    this.#recovery = new LifecycleRecovery({
      database: this.#database,
      clock: this.#clock,
      lifecycleRotations: this.#lifecycleRotations,
      lifecycleReceipts: this.#lifecycleReceipts,
      lifecycleReceiptAuthority: this.#lifecycleReceiptAuthority,
      admission: {
        ensureReceiptScope: this.#admission.ensureReceiptScope.bind(this.#admission),
        sourceVectorDigest: this.#admission.sourceVectorDigest.bind(this.#admission),
      },
      finalizer: this.#finalizer,
      requestAdapter: (adapterId, method, params) => this.#requestAdapter(adapterId, method, params),
      event: (runId, type, actorAgentId, payload) => {
        this.#event(runId, type, actorAgentId, payload);
      },
    });
    this.#service = new LifecycleService({
      database: this.#database,
      clock: this.#clock,
      commandJournal: this.#commandJournal,
      checkpointPolicy: this.#checkpointPolicy,
      admission: this.#admission,
      generationLosses: this.#generationLosses,
      providerActionAdmission: this.#providerActionAdmission,
      assertChair: (runId, actorAgentId) => this.#assertChair(runId, actorAgentId),
      adapterIdForAgent: (runId, agentId) => this.#adapterIdForAgent(runId, agentId),
      executeGenericRelease: (input) => this.#providerActionExecutor.executeGenericRelease(input),
      event: (runId, type, actorAgentId, payload) => {
        this.#event(runId, type, actorAgentId, payload);
      },
      isClosing: () => this.#closing,
    });
    this.#operatorStore = new OperatorStore({ database: this.#database, clock: this.#clock });
    this.#gates = new ScopedGateStore({
      database: this.#database,
      operatorStore: this.#operatorStore,
      clock: this.#clock,
    });
    this.#externalEffects = options.externalEffects === undefined
      ? undefined
      : new ExternalEffectService({
          database: this.#database,
          registry: options.externalEffects.registry,
          evidence: options.externalEffects.evidence,
          gates: this.#gates,
          clock: this.#clock,
        });
    this.#operatorProjections = new OperatorProjectionStore({
      database: this.#database,
      operatorStore: this.#operatorStore,
      clock: this.#clock,
    });
    this.#gitRepositoryReads = new GitRepositoryReadService({
      database: this.#database,
      operatorStore: this.#operatorStore,
      privateStateRoot: dirname(realpathSync(options.databasePath)),
      clock: this.#clock,
      ...(options.gitHostedChecks === undefined ? {} : { hostedChecks: options.gitHostedChecks }),
      artifactRegistry: this.#artifactRegistry,
    });
    this.#artifactContentReads = new ArtifactContentReadService({
      database: this.#database,
      operatorStore: this.#operatorStore,
      privateStateRoot: dirname(realpathSync(options.databasePath)),
    });
    const privateStateRoot = dirname(realpathSync(options.databasePath));
    this.#trustedGitRegistry = new TrustedGitRegistry(this.#database, this.#clock);
    this.#trustedRunGitAllowlists = options.trustedGitConfiguration?.runAllowlists ?? [];
    if (options.trustedGitConfiguration !== undefined) {
      this.#trustedGitRegistry.materialize({
        ...(options.trustedGitConfiguration.executionProfiles === undefined
          ? {}
          : { executionProfiles: options.trustedGitConfiguration.executionProfiles }),
        ...(options.trustedGitConfiguration.remoteRegistrations === undefined
          ? {}
          : { remoteRegistrations: options.trustedGitConfiguration.remoteRegistrations }),
      });
    }
    const gitMutationPort = options.gitMutationPort ?? new FixedGitMutationPort({
      privateStateRoot,
      clock: this.#clock,
    });
    this.#typedGit = new TypedGitService({
      database: this.#database,
      gitPort: gitMutationPort,
      ...(options.gitConflictInspector === undefined ? {} : { conflictInspector: options.gitConflictInspector }),
      materializeTrustedRunAllowlist: (identity) => {
        const matching = this.#trustedRunGitAllowlists.filter((allowlist) =>
          allowlist.projectSessionId === identity.projectSessionId &&
          allowlist.coordinationRunId === identity.coordinationRunId);
        if (matching.length > 0) this.#trustedGitRegistry.materialize({ runAllowlists: matching });
      },
      clock: this.#clock,
      daemonInstanceId: `git-owner-${randomBytes(16).toString("hex")}`,
    });
    const productionOperatorPorts = createProductionOperatorActionPorts({
      database: this.#database,
      clock: this.#clock,
      adapter: {
        capabilities: async (adapterId) => await this.#adapterSupervisor.request(adapterId, "capabilities", {}),
        dispatch: async (adapterId, input) => await this.#adapterSupervisor.request(adapterId, "dispatch", input),
        lookup: async (adapterId, actionId) => await this.#adapterSupervisor.request(adapterId, "lookup_action", { actionId }),
      },
      providerActionAdmission: this.#providerActionAdmission,
      ...(options.daemonStopPort === undefined ? {} : { daemonStop: options.daemonStopPort }),
      ...(this.#externalEffects === undefined ? {} : { externalEffects: this.#externalEffects }),
      typedGit: this.#typedGit,
      retireVolatileProjectSession: (projectSessionId) => {
        this.#adapterSupervisor.retireProjectSessionBridges(projectSessionId);
      },
    });
    // Plan §1 "What dissolve means, minimally" (issue #354, S4e2): each of the four custody
    // families is constructed here with only its own narrow effects/options; `LaunchCustodyService`
    // is a thin facade over the already-built instances (launch-custody.ts), and the operator ports
    // below receive the narrow family adapters directly rather than the combined facade.
    const launchCustodyFamilies = options.fabricSocketPath === undefined
      ? undefined
      : (() => {
          const database = this.#database;
          const clock = this.#clock;
          const fabricSocketPath = options.fabricSocketPath;
          const daemonInstanceGeneration = () => this.#currentDaemonInstanceGeneration();
          const adapterContracts = {
            inspect: async (adapterId: string) => await this.#inspectLaunchAdapterContract(adapterId),
          };
          const adapterEffects = {
            dispatch: async (handle: LaunchDispatchHandle) => await this.#dispatchLaunchAdapter(handle),
            lookup: async (input: Readonly<{ providerAdapterId: string; providerActionId: string; providerContractDigest: Digest; attestationChallengeDigest: Digest }>) => await this.#lookupLaunchAdapter(input),
            hasRetainedChairBridge: (entry: RetainedChairBridge) => this.#adapterSupervisor.hasRetainedChairBridge(entry),
            recoverChair: async (handle: ChairRecoveryDispatchHandle) => await this.#dispatchChairRecoveryAdapter(handle),
            lookupChairRecovery: async (input: Readonly<{ adapterId: string; actionId: string }>) => await this.#requestAdapter(
              input.adapterId,
              "lookup_action",
              { actionId: input.actionId },
            ),
            lookupRetainedSuccessorBridge: async (input: RetainedSuccessorBridgeProbe) => (
              await this.#adapterSupervisor.lookupRetainedSuccessorBridge(input)
            ),
            promoteRetainedSuccessorBridge: async (input: RetainedSuccessorBridgeProbe) => (
              await this.#adapterSupervisor.promoteRetainedChildBridgeToChair(input)
            ),
          };
          const agentEffects = {
            dispatch: async (handle: AgentDispatchHandle) => await this.#dispatchAgentAdapter(handle),
            attachWithoutBridge: async (handle: AgentDispatchHandle) => await this.#attachWithoutBridge(handle),
            lookup: async (input: Readonly<{ adapterId: string; actionId: string }>) => await this.#requestAdapter(input.adapterId, "lookup_action", { actionId: input.actionId }),
            hasRetainedBridge: (result: AgentCustodyResult, handle: AgentDispatchHandle) => this.#adapterSupervisor.hasRetainedChildBridge({
              runId: handle.runId,
              agentId: result.agentId,
              adapterId: result.adapterId,
              actionId: result.actionId,
              providerSessionRef: result.providerSessionRef,
              providerSessionGeneration: result.providerSessionGeneration,
              bridgeGeneration: result.bridgeGeneration,
            }),
          };
          const retireVolatileProjectSession = (projectSessionId: string) => {
            this.#adapterSupervisor.retireProjectSessionBridges(projectSessionId);
          };
          const retireVolatileChairBridge = (entry: RetainedChairBridge) => {
            this.#adapterSupervisor.retireChairBridge(entry);
          };
          const randomCapability = () => `afc_${randomBytes(32).toString("base64url")}`;
          const providerAgentCustody = new ProviderAgentCustodyAdapter({
            database,
            providerActionAdmission: this.#providerActionAdmission,
            clock,
            fault: this.#fault,
            randomCapability,
            fabricSocketPath,
            agentEffects,
            daemonInstanceGeneration,
          });
          const providerAgentCustodyRecovery = new ProviderAgentCustodyRecoveryAdapter({
            database,
            agentEffects,
            custody: providerAgentCustody.recoveryPort(),
          });
          const chairLiveHandoffCustody = new ChairLiveHandoffCustodyAdapter({
            database,
            providerActionAdmission: this.#providerActionAdmission,
            clock,
            fault: this.#fault,
            adapterContracts,
            adapterEffects,
            retireVolatileChairBridge,
          });
          const chairLiveHandoffCustodyRecovery = new ChairLiveHandoffCustodyRecoveryAdapter({
            database,
            custody: chairLiveHandoffCustody.recoveryPort(),
          });
          const chairRecoveryCustody = new ChairRecoveryCustodyService({
            database,
            providerActionAdmission: this.#providerActionAdmission,
            clock,
            fault: this.#fault,
            randomCapability,
            randomAttestationChallenge: () => randomBytes(32).toString("hex"),
            fabricSocketPath,
            adapterContracts,
            adapterEffects,
            daemonInstanceGeneration,
            retireVolatileProjectSession,
            reconcileUnknownLaunchUsage: (input) => reconcileUnknownLaunchUsageOwner(database, clock, input),
          });
          const launchSettlement = new LaunchSettlement({
            database,
            clock,
            adapterContracts,
            adapterEffects,
            chairLoss: {
              observeChairBridgeLoss: (input) => chairRecoveryCustody.observeChairBridgeLoss(input),
            },
          });
          const launchService = new LaunchService({
            database,
            providerActionAdmission: this.#providerActionAdmission,
            clock,
            fault: this.#fault,
            randomCapability,
            randomAttestationChallenge: () => randomBytes(32).toString("hex"),
            fabricSocketPath,
            adapterContracts,
            adapterEffects,
            settlement: launchSettlement,
          });
          return {
            providerAgentCustody,
            providerAgentCustodyRecovery,
            chairLiveHandoffCustody,
            chairLiveHandoffCustodyRecovery,
            chairRecoveryCustody,
            launchService,
            launchSettlement,
          };
        })();
    this.#launchCustody = launchCustodyFamilies === undefined
      ? undefined
      : new LaunchCustodyService({
          database: this.#database,
          clock: this.#clock,
          ...launchCustodyFamilies,
        });
    if (this.#launchCustody !== undefined) {
      this.#adapterSupervisor.setChairBridgeLossHandler((entry, reason) => {
        this.#launchCustody?.observeChairBridgeLoss({ ...entry, reason });
      });
      this.#adapterSupervisor.setChildBridgeLossHandler((entry, reason) => {
        this.#launchCustody?.observeChildBridgeLoss({ ...entry, reason });
      });
    }
    this.#operatorActions = new OperatorActionStore({
      database: this.#database,
      operatorStore: this.#operatorStore,
      statePort: options.operatorActionPorts?.statePort ?? productionOperatorPorts.statePort,
      effectPort: options.operatorActionPorts?.effectPort ?? productionOperatorPorts.effectPort,
      ...(this.#launchCustody === undefined ? {} : { launchCustody: this.#launchCustody }),
      ...(this.#launchCustody === undefined ? {} : { chairRecoveryCustody: this.#launchCustody }),
      ...(this.#launchCustody === undefined ? {} : { chairLiveHandoffCustody: this.#launchCustody }),
      ...(this.#lifecycleReceiptAuthority === undefined ? {} : {
        lifecycleRecoveryCustody: new LifecycleRecoveryCustodyService({
          database: this.#database,
          receipts: this.#lifecycleReceipts,
          authority: this.#lifecycleReceiptAuthority,
          clock: this.#clock,
        }),
      }),
      clock: this.#clock,
    });
    this.#projectSessions = new ProjectSessionStore({
      database: this.#database,
      operatorStore: this.#operatorStore,
      commandJournal: this.#commandJournal,
      clock: this.#clock,
      retireVolatileProjectSession: (projectSessionId) => {
        this.#adapterSupervisor.retireProjectSessionBridges(projectSessionId);
      },
    });
    this.#memberships = new ProjectSessionMembershipStore({
      database: this.#database,
      clock: this.#clock,
    });
    this.#mailboxCustody = new MailboxCustodyService({
      database: this.#database,
      clock: this.#clock,
      commandJournal: this.#commandJournal,
      memberships: this.#memberships,
      host: {
        assertChair: (runId, actorAgentId) => this.#assertChair(runId, actorAgentId),
        event: (runId, type, actorAgentId, payload) => {
          this.#event(runId, type, actorAgentId, payload);
        },
      },
    });
    this.#results = new AtomicDeliveryStore({
      database: this.#database,
      clock: this.#clock,
      artifactRegistry: this.#artifactRegistry,
      memberships: this.#memberships,
    });
    this.#intakes = new IntakeStore({
      database: this.#database,
      operatorStore: this.#operatorStore,
      clock: this.#clock,
      requestCommitter: {
        commitTaskRequest: (request) => {
          const run = rowOrNotFound(this.#database.prepare(`
            SELECT chair_agent_id FROM runs
             WHERE run_id=? AND project_session_id=?
          `).get(request.coordinationRunId, request.projectSessionId), "intake coordination run");
          return this.#results.request(
            this.#agentContext(request.coordinationRunId, stringField(run, "chair_agent_id")),
            request,
          );
        },
      },
    });
    this.#resources = new HierarchicalAdmissionStore({ database: this.#database, clock: this.#clock });
    this.#workstreams = new CoordinatedWorkstreamStore({
      database: this.#database,
      clock: this.#clock,
      commandJournal: this.#commandJournal,
      resources: this.#resources,
      createTeam: (runId, actorAgentId, input) => this.createTeam(runId, actorAgentId, input),
    });
    this.#runPlans = new RunPlanStore({ database: this.#database, clock: this.#clock });
    this.#notifications = new NotificationOutbox({ database: this.#database, clock: this.#clock });
    this.#notificationWorker = new NativeNotificationWorker({
      outbox: this.#notifications,
      adapter: new MacOsNativeDesktopAdapter(),
      workerInstanceId: `native-notification-${randomBytes(16).toString("hex")}`,
      integrationId: "native-desktop",
      clock: this.#clock,
    });
    this.#herdr = new HerdrDaemonIntegration({
      database: this.#database,
      providerActionAdmission: this.#providerActionAdmission,
      configuration: options.herdr ?? { mode: "disabled" },
      clock: this.#clock,
    });
    this.#herdrConfigured = options.herdr !== undefined;
    this.#recoverLaunchPreparations();
  }

  /** Trusted in-process daemon composition only; no protocol operation dispatches here. */
  materializeTrustedGitConfiguration(configuration: TrustedGitConfiguration): {
    profiles: number;
    remotes: number;
    runAllowlists: number;
  } {
    return this.#trustedGitRegistry.materialize(configuration);
  }

  async executeHerdrAction(request: HerdrDaemonActionRequest): Promise<HerdrDaemonActionResult> {
    return await this.#herdr.executeAction(request);
  }

  async executeHerdrDirectSteer(request: HerdrDirectSteerRequest): Promise<HerdrDirectSteerResult> {
    return await this.#herdr.executeDirectSteer(request);
  }

  async runHerdrPresencePass(): Promise<import("../integrations/herdr-daemon-integration.js").HerdrPresencePassResult> {
    return await this.#herdr.runPresencePass();
  }

  recoverDaemonRuntimeEpoch(input: {
    instanceGeneration: number;
    instanceId: string;
  }): ReturnType<typeof recoverRuntimeEpoch> {
    return recoverRuntimeEpoch(this.#database, {
      ...input,
      now: this.#clock(),
    });
  }

  markDaemonRuntimeRunning(instanceGeneration: number): ReturnType<typeof markRuntimeEpochRunning> {
    return markRuntimeEpochRunning(this.#database, {
      instanceGeneration,
      now: this.#clock(),
    });
  }

  async attemptIdleStop(input: {
    actionId: string;
    daemonInstanceGeneration: number;
    election: IdleElectionPort;
    beforeElectionRelease?: () => Promise<void>;
    closeSocket(): Promise<void>;
    reopenSocket(): Promise<void>;
  }): Promise<IdleStopResult> {
    return await attemptRuntimeIdleStop({
      ...input,
      database: this.#database,
      clock: this.#clock,
    });
  }

  async runNativeNotificationPass(): Promise<NotificationWorkerPassResult> {
    return await this.#notificationWorker.runOnce();
  }

  runResultDeadlinePass(input: ResultDeadlinePassInput): ResultDeadlinePassResult {
    return this.#results.sweepDeadlines(input);
  }

  async attemptDrainedStop(input: {
    actionId: string;
    token: QuiesceToken;
    excludeOperatorEffectCustodyId?: string;
    election: IdleElectionPort;
    beforeElectionRelease?: () => Promise<void>;
    closeSocket(): Promise<void>;
    reopenSocket(): Promise<void>;
  }): Promise<IdleStopResult> {
    return await attemptRuntimeDrainedStop({
      ...input,
      database: this.#database,
      clock: this.#clock,
    });
  }

  recordDaemonStopCustodyResult(input: {
    custodyId: string;
    resultCorrelationDigest: string;
    operatorId: string;
    projectId: string;
    projectSessionId: string;
    principalGeneration: number;
    commandId: string;
    daemonInstanceGeneration: number;
    state: "stopped" | "failed" | "rejected";
    result: unknown;
  }): void {
    const persist = this.#database.transaction(() => {
      const resultJson = canonicalJson(input.result);
      const changed = this.#database.prepare(`
        UPDATE operator_daemon_stop_custody SET state=?, result_json=?, updated_at=?
         WHERE custody_id=? AND result_correlation_digest=?
           AND operator_id=? AND project_id=? AND project_session_id=? AND command_id=?
           AND principal_generation=? AND daemon_instance_generation=? AND operation='daemon-stop'
           AND state IN ('prepared','scheduled')
      `).run(
        input.state,
        resultJson,
        this.#clock(),
        input.custodyId,
        input.resultCorrelationDigest,
        input.operatorId,
        input.projectId,
        input.projectSessionId,
        input.commandId,
        input.principalGeneration,
        input.daemonInstanceGeneration,
      );
      if (changed.changes !== 1) {
        const existing = this.#database.prepare(`
          SELECT state, result_json FROM operator_daemon_stop_custody
           WHERE custody_id=? AND result_correlation_digest=?
             AND operator_id=? AND project_id=? AND project_session_id=?
             AND principal_generation=? AND command_id=?
             AND daemon_instance_generation=? AND operation='daemon-stop'
        `).get(
          input.custodyId,
          input.resultCorrelationDigest,
          input.operatorId,
          input.projectId,
          input.projectSessionId,
          input.principalGeneration,
          input.commandId,
          input.daemonInstanceGeneration,
        );
        if (!isRow(existing) || existing.state !== input.state || existing.result_json !== resultJson) {
          throw new ProjectFabricCoreError("STALE_GENERATION", "daemon stop custody result correlation changed");
        }
      }

      const terminalOutcome = input.state === "stopped"
        ? canonicalJson({ status: "committed", afterState: { lifecycleState: "stopped" } })
        : input.state === "rejected"
          ? canonicalJson({ status: "rejected", code: "state-changed", evidenceRefs: [] })
          : null;
      const effectState = input.state === "stopped"
        ? "terminal"
        : input.state;
      const effectChanged = terminalOutcome === null
        ? this.#database.prepare(`
            UPDATE operator_effect_custody SET state='failed', updated_at=?
             WHERE custody_id=? AND operator_id=? AND project_id=? AND project_session_id=?
               AND principal_generation=? AND command_id=? AND operation='stop' AND state='dispatching'
          `).run(
            this.#clock(),
            input.custodyId,
            input.operatorId,
            input.projectId,
            input.projectSessionId,
            input.principalGeneration,
            input.commandId,
          )
        : this.#database.prepare(`
            UPDATE operator_effect_custody SET state=?, outcome_json=?, updated_at=?
             WHERE custody_id=? AND operator_id=? AND project_id=? AND project_session_id=?
               AND principal_generation=? AND command_id=? AND operation='stop' AND state='dispatching'
          `).run(
            effectState,
            terminalOutcome,
            this.#clock(),
            input.custodyId,
            input.operatorId,
            input.projectId,
            input.projectSessionId,
            input.principalGeneration,
            input.commandId,
          );
      if (effectChanged.changes === 1) return;
      const existingEffect = this.#database.prepare(`
        SELECT state, outcome_json FROM operator_effect_custody
         WHERE custody_id=? AND operator_id=? AND project_id=? AND project_session_id=?
           AND principal_generation=? AND command_id=? AND operation='stop'
      `).get(
        input.custodyId,
        input.operatorId,
        input.projectId,
        input.projectSessionId,
        input.principalGeneration,
        input.commandId,
      );
      if (
        !isRow(existingEffect) || existingEffect.state !== effectState ||
        (terminalOutcome !== null && existingEffect.outcome_json !== terminalOutcome)
      ) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "daemon stop effect custody finalization changed");
      }
    });
    persist();
  }

  provisionLocalOperator(input: LocalOperatorProvisioningInput): LocalOperatorProvisioningResult {
    return this.#operatorStore.provisionLocalOperator(input);
  }

  openLocalOperatorConsoleCapability(
    input: LocalOperatorConsoleCapabilityInput,
  ): LocalOperatorConsoleCapabilityResult {
    return this.#operatorStore.openLocalOperatorConsoleCapability(input);
  }

  issueLocalOperatorSessionCapability(
    input: LocalOperatorSessionCapabilityInput,
  ): LocalOperatorSessionCapabilityResult {
    return this.#operatorStore.issueLocalOperatorSessionCapability(input);
  }

  openLocalOperatorConsoleSessionCapability(
    input: Omit<LocalOperatorSessionCapabilityInput, "fresh">,
  ): LocalOperatorConsoleSessionCapabilityResult {
    return this.#operatorStore.openLocalOperatorConsoleSessionCapability(input);
  }

  openLocalOperatorConsoleTakeoverCapability(
    input: LocalOperatorTakeoverCapabilityInput,
  ): LocalOperatorTakeoverCapabilityResult {
    return this.#operatorStore.openLocalOperatorConsoleTakeoverCapability(input);
  }

  rotateLocalOperatorPrincipal(
    input: LocalOperatorPrincipalRotationInput,
  ): LocalOperatorPrincipalRotationResult {
    return this.#operatorStore.rotatePrincipal(input);
  }

  #workspaceRootForRun(runId: string): string {
    const run = rowOrNotFound(this.#database.prepare("SELECT workspace_root FROM runs WHERE run_id = ?").get(runId), "run");
    return stringField(run, "workspace_root");
  }

  #agentContext(runId: string, agentId: string): AuthenticatedAgentContext {
    const identity = rowOrNotFound(this.#database.prepare(`
      SELECT r.project_session_id, c.principal_generation
        FROM runs r
        JOIN capabilities c ON c.run_id=r.run_id AND c.agent_id=?
       WHERE r.run_id=? AND c.revoked_at IS NULL
       ORDER BY c.principal_generation DESC
       LIMIT 1
    `).get(agentId, runId), "authenticated agent context");
    return {
      agentId: agentId as never,
      projectSessionId: stringField(identity, "project_session_id") as never,
      coordinationRunId: runId as never,
      principalGeneration: numberField(identity, "principal_generation"),
    };
  }

  async close(): Promise<void> {
    this.#closing = true;
    this.#providerActionState.clearScheduledPump();
    this.#providerActionState.abandonDeferred();
    let drainFailure: unknown;
    try {
      await waitWithShutdownDeadline(
        (async () => {
          while (
            this.#service.size > 0 ||
            this.#providerActionState.size > 0 ||
            this.#continuation.size > 0
          ) {
            await Promise.allSettled([
              ...this.#service.pending(),
              ...this.#providerActionState.pending(),
              ...this.#continuation.pending(),
            ]);
            this.#providerActionState.abandonDeferred();
          }
        })(),
        DAEMON_SHUTDOWN_FABRIC_CLOSE_TIMEOUT_MS,
        DAEMON_SHUTDOWN_FABRIC_CLOSE_TIMEOUT,
        `fabric operations did not close within ${String(DAEMON_SHUTDOWN_FABRIC_CLOSE_TIMEOUT_MS)}ms`,
      );
    } catch (error: unknown) {
      drainFailure = error;
    }
    try {
      await this.#adapterSupervisor.close();
    } finally {
      if (this.#database.open) {
        try {
          this.#database.pragma("wal_checkpoint(TRUNCATE)");
        } finally {
          if (this.#database.open) this.#database.close();
        }
      }
    }
    if (drainFailure !== undefined) throw drainFailure;
  }

  async #requestAdapter(adapterId: string, method: string, params: Record<string, unknown>): Promise<unknown> {
    this.#adapter(adapterId);
    return await this.#adapterSupervisor.request(adapterId, method, params);
  }

  async #inspectLaunchAdapterContract(adapterId: string): Promise<LaunchAdapterContract> {
    const capabilities = await this.#requestAdapter(adapterId, "capabilities", {});
    assertAdapterOperation(capabilities, "launch_chair");
    if (!isRow(capabilities) || !Object.hasOwn(capabilities, "chairLaunch")) {
      throw new FabricError("CAPABILITY_FORBIDDEN", "adapter does not advertise a chair launch contract");
    }
    return parseLaunchAdapterContract(capabilities.chairLaunch);
  }

  async #inspectAgentBridgeContract(
    adapterId: string,
    operation: "spawn" | "attach",
  ): Promise<AgentBridgeCapability | undefined> {
    const capabilities = await this.#requestAdapter(adapterId, "capabilities", {});
    assertAdapterOperation(capabilities, operation);
    if (!isRow(capabilities) || capabilities.agentBridge === undefined) return undefined;
    return parseAgentBridgeCapability(capabilities.agentBridge);
  }

  async #dispatchAgentAdapter(handle: AgentDispatchHandle): Promise<unknown> {
    if (handle.capability === undefined || handle.socketPath === undefined || handle.expectedPrincipal === undefined) {
      throw new FabricError("CAPABILITY_UNAVAILABLE", "agent bridge private handoff is unavailable");
    }
    return await this.#adapterSupervisor.provisionAgent(
      handle.adapterId,
      {
        schemaVersion: 1,
        runId: handle.runId,
        operation: handle.operation,
        actionId: handle.actionId,
        targetAgentId: handle.targetAgentId,
        authorityId: handle.authorityId,
        bridgeGeneration: handle.bridgeGeneration,
        bridgeContractDigest: handle.bridgeContractDigest,
        payload: handle.publicPayload,
        ...(handle.requestedProviderSessionRef === undefined
          ? {}
          : { providerSessionRef: handle.requestedProviderSessionRef }),
      },
      {
        capability: handle.capability,
        socketPath: handle.socketPath,
        expectedPrincipal: handle.expectedPrincipal,
      },
    );
  }

  async #attachWithoutBridge(handle: AgentDispatchHandle): Promise<unknown> {
    if (handle.operation !== "attach" || handle.requestedProviderSessionRef === undefined) {
      throw new FabricError("CAPABILITY_UNAVAILABLE", "bridge-less custody is attach-only");
    }
    const result = await this.#requestAdapter(handle.adapterId, "attach", {
      actionId: handle.actionId,
      resumeReference: handle.requestedProviderSessionRef,
      ...handle.publicPayload,
    });
    if (!isRow(result)) throw new FabricError("LIFECYCLE_PRECONDITION_FAILED", "adapter attach returned no typed result");
    return {
      providerSessionRef: typeof result.resumeReference === "string"
        ? result.resumeReference
        : handle.requestedProviderSessionRef,
      providerSessionGeneration: typeof result.providerSessionGeneration === "number"
        ? result.providerSessionGeneration
        : 1,
    };
  }

  #currentDaemonInstanceGeneration(): number {
    const current = this.#database.prepare(`
      SELECT instance_generation FROM daemon_runtime_epochs
       WHERE state IN ('starting','running','quiescing')
       ORDER BY instance_generation DESC LIMIT 1
    `).get();
    return isRow(current) && typeof current.instance_generation === "number"
      ? current.instance_generation
      : 1;
  }

  #launchResourceUsage(
    providerAdapterId: string,
    providerActionId: string,
  ): Record<string, number | "unknown"> {
    const reservation = rowOrNotFound(this.#database.prepare(`
      SELECT r.amounts_json
        FROM project_session_launch_custody c
        JOIN resource_reservations r ON r.reservation_id=c.reservation_id
       WHERE c.provider_adapter_id=? AND c.provider_action_id=?
    `).get(providerAdapterId, providerActionId), "launch resource reservation");
    const amounts: unknown = JSON.parse(stringField(reservation, "amounts_json"));
    if (!isRow(amounts) || Object.values(amounts).some((value) => !Number.isSafeInteger(value) || Number(value) < 0)) {
      throw new Error("launch reservation amounts are invalid");
    }
    return Object.fromEntries(Object.keys(amounts).sort().map((unit) => [
      unit,
      unit === "provider_calls" ? 1
        : unit === "concurrent_turns" ? 0
          : "unknown",
    ]));
  }

  #terminalLaunchOutcome(
    handle: Pick<LaunchDispatchHandle, "providerAdapterId" | "providerActionId" | "providerContractDigest" | "attestationChallengeDigest">,
    raw: unknown,
    observationKind: "dispatch-return" | "lookup",
  ): unknown {
    const result = parseChairLaunchProviderResult(raw, {
      providerAdapterId: handle.providerAdapterId,
      providerActionId: handle.providerActionId,
      providerContractDigest: handle.providerContractDigest,
      challengeDigest: handle.attestationChallengeDigest,
    });
    const effectEvidence = {
      schemaVersion: 1,
      providerAdapterId: handle.providerAdapterId,
      providerActionId: handle.providerActionId,
      providerContractDigest: handle.providerContractDigest,
      resumeReference: result.resumeReference,
      providerSessionGeneration: result.providerSessionGeneration,
      fabricContinuity: result.fabricContinuity,
    };
    return {
      schemaVersion: 1,
      providerAdapterId: handle.providerAdapterId,
      providerActionId: handle.providerActionId,
      providerContractDigest: handle.providerContractDigest,
      observationKind,
      observedAt: new Date(this.#clock()).toISOString(),
      outcome: {
        kind: "terminal-success",
        providerSessionRef: result.resumeReference,
        providerSessionGeneration: result.providerSessionGeneration,
        effectDigest: sha256Digest(canonicalJson(effectEvidence)),
        resourceUsage: this.#launchResourceUsage(handle.providerAdapterId, handle.providerActionId),
      },
    };
  }

  #ambiguousLaunchOutcome(
    input: Pick<LaunchDispatchHandle, "providerAdapterId" | "providerActionId" | "providerContractDigest">,
    reasonCode: "absent" | "transport-error" | "adapter-error" | "malformed" | "incomplete" | "conflict" | "missing-resume-reference",
    evidence: unknown,
  ): unknown {
    return {
      schemaVersion: 1,
      providerAdapterId: input.providerAdapterId,
      providerActionId: input.providerActionId,
      providerContractDigest: input.providerContractDigest,
      observationKind: "lookup",
      observedAt: new Date(this.#clock()).toISOString(),
      outcome: {
        kind: "ambiguous",
        reasonCode,
        evidenceDigest: evidence === null ? null : sha256Digest(canonicalJson(evidence)),
      },
    };
  }

  async #dispatchLaunchAdapter(handle: LaunchDispatchHandle): Promise<unknown> {
    const result = await this.#adapterSupervisor.launchChair(
      handle.providerAdapterId,
      {
        schemaVersion: 1,
        actionId: handle.providerActionId,
        providerContractDigest: handle.providerContractDigest,
        payload: handle.publicPayload,
      },
      {
        capability: handle.capability,
        socketPath: handle.socketPath,
        attestationChallenge: handle.attestationChallenge,
        expectedPrincipal: handle.expectedPrincipal,
      },
    );
    if (!this.#adapterSupervisor.hasRetainedChairBridge({
      ...handle.expectedPrincipal,
      adapterId: handle.providerAdapterId,
      actionId: handle.providerActionId,
      providerSessionRef: result.resumeReference,
      providerSessionGeneration: result.providerSessionGeneration,
      bridgeGeneration: 1,
    })) {
      throw new ProviderAdapterError("CHAIR_BRIDGE_LOST", "chair bridge closed before launch activation");
    }
    return this.#terminalLaunchOutcome(handle, result, "dispatch-return");
  }

  async #dispatchChairRecoveryAdapter(handle: ChairRecoveryDispatchHandle): Promise<unknown> {
    if (
      handle.intent.path !== "rebind" || handle.capability === undefined ||
      handle.attestationChallenge === undefined || handle.socketPath === undefined
    ) throw new FabricError("CAPABILITY_UNAVAILABLE", "chair recovery private handoff is unavailable");
    const loss = rowOrNotFound(this.#database.prepare(`
      SELECT loss.*, action.payload_json
        FROM chair_bridge_losses loss
        JOIN provider_actions action
          ON action.adapter_id=loss.provider_adapter_id AND action.action_id=loss.provider_action_id
       WHERE loss.loss_id=?
    `).get(handle.intent.lossId), "chair recovery loss");
    const oldPayload: unknown = JSON.parse(stringField(loss, "payload_json"));
    const providerPayload = isRow(oldPayload) && isRow(oldPayload.input) ? oldPayload.input : {};
    const result = await this.#adapterSupervisor.recoverChair(
      handle.intent.providerAdapterId,
      {
        schemaVersion: 1,
        recoveryId: handle.recoveryId,
        lossId: handle.intent.lossId,
        actionId: handle.intent.providerActionId,
        providerContractDigest: handle.intent.providerContractDigest,
        resumeReference: stringField(loss, "provider_session_ref"),
        expectedProviderSessionGeneration: handle.intent.expectedProviderSessionGeneration,
        nextProviderSessionGeneration: handle.intent.expectedProviderSessionGeneration + 1,
        bridgeGeneration: handle.intent.expectedLostBridgeGeneration + 1,
        payload: providerPayload,
      },
      {
        capability: handle.capability,
        socketPath: handle.socketPath,
        attestationChallenge: handle.attestationChallenge,
        expectedPrincipal: {
          agentId: stringField(loss, "chair_agent_id"),
          projectSessionId: handle.intent.projectSessionId,
          runId: handle.intent.coordinationRunId,
          principalGeneration: handle.intent.expectedPrincipalGeneration + 1,
        },
      },
    );
    return {
      schemaVersion: 1,
      recoveryId: handle.recoveryId,
      providerAdapterId: handle.intent.providerAdapterId,
      providerActionId: handle.intent.providerActionId,
      providerContractDigest: handle.intent.providerContractDigest,
      providerSessionRef: result.resumeReference,
      providerSessionGeneration: result.providerSessionGeneration,
      activationEvidenceDigest: sha256Digest(canonicalJson(result.fabricContinuity)),
    };
  }

  async #lookupLaunchAdapter(input: Pick<
    LaunchDispatchHandle,
    "providerAdapterId" | "providerActionId" | "providerContractDigest" | "attestationChallengeDigest"
  >): Promise<unknown> {
    let record: unknown;
    try {
      record = await this.#requestAdapter(input.providerAdapterId, "lookup_action", {
        actionId: input.providerActionId,
      });
    } catch (error: unknown) {
      if (error instanceof Error && error.name === "ACTION_NOT_FOUND") {
        return this.#ambiguousLaunchOutcome(input, "absent", null);
      }
      throw error;
    }
    if (!isRow(record)) return this.#ambiguousLaunchOutcome(input, "malformed", record);
    if (record.status === "terminal") {
      try {
        return this.#terminalLaunchOutcome(input, record.result, "lookup");
      } catch {
        return this.#ambiguousLaunchOutcome(input, "malformed", record);
      }
    }
    if (record.status === "ambiguous" || record.status === "accepted" || record.status === "dispatched" || record.status === "prepared") {
      return this.#ambiguousLaunchOutcome(input, "incomplete", record);
    }
    return this.#ambiguousLaunchOutcome(input, "conflict", record);
  }
  async recoverStartupState(): Promise<{
    actionsReconciled: number; actionsQuarantined: number;
    leasesQuarantined: number; sessionsDegraded: number; deliveriesReleased: number;
  }> {
    if (this.#lifecycleReceiptRecovery === undefined) {
      if (hasKnownLifecycleReceiptState(this.#database)) {
        throw new LifecycleReceiptRecoveryError(
          "RECOVERY_PENDING",
          "lifecycle receipt authority recovery is pending",
        );
      }
    } else {
      await this.#lifecycleReceiptRecovery.hydrateKnownProjects();
    }
    this.#results.recover();
    this.#notifications.recover();
    await this.#recovery.recoverRotations();
    await this.#launchCustody?.recover();
    await this.#externalEffects?.recover();
    await this.#herdr.recover();
    if (this.#herdrConfigured) await this.#herdr.runPresencePass();
    await this.#typedGit.recover();
    const now = this.#clock();
    const deliveriesReleased = this.#database
      .prepare("UPDATE deliveries SET state = 'ready', claim_deadline = NULL WHERE state = 'claimed' AND claim_deadline <= ?")
      .run(now).changes;
    const expiredLeases = this.#database
      .prepare("SELECT run_id, lease_id, holder_agent_id FROM leases WHERE kind = 'write' AND status = 'active' AND expires_at <= ?")
      .all(now);
    this.#database.transaction(() => {
      for (const value of expiredLeases) {
        const lease = rowOrNotFound(value, "startup lease");
        this.#database.prepare("UPDATE leases SET status = 'quarantined', updated_at = ? WHERE run_id = ? AND lease_id = ?").run(now, stringField(lease, "run_id"), stringField(lease, "lease_id"));
        this.#event(stringField(lease, "run_id"), "startup-write-lease-quarantined", null, { leaseId: stringField(lease, "lease_id"), predecessorAgentId: stringField(lease, "holder_agent_id") });
      }
    })();
    const { actionsReconciled, actionsQuarantined } = await this.#providerActionRecovery.recoverStartupProviderActions(now);
    let sessionsDegraded = 0;
    const sessions = this.#database.prepare(`
      SELECT a.run_id,a.agent_id,a.provider_session_ref,b.adapter_id,
             run.project_session_id,bridge.action_id,bridge.bridge_generation,
             bridge.revision AS bridge_revision,bridge.capability_hash,
             custody.bridge_contract_digest,capability.principal_generation,
             provider.provider_session_generation,
             CAST(provider.context_revision AS TEXT) AS context_revision
        FROM agents a
        JOIN runs run ON run.run_id=a.run_id
        JOIN agent_adapter_bindings b ON b.run_id=a.run_id AND b.agent_id=a.agent_id
        JOIN agent_bridge_state bridge
          ON bridge.run_id=a.run_id AND bridge.agent_id=a.agent_id
         AND bridge.adapter_id=b.adapter_id AND bridge.bridge_state='active'
         AND bridge.provider_session_ref=a.provider_session_ref
        JOIN provider_agent_custody custody
          ON custody.run_id=bridge.run_id AND custody.adapter_id=bridge.adapter_id
         AND custody.action_id=bridge.action_id AND custody.target_agent_id=bridge.agent_id
         AND custody.capability_hash=bridge.capability_hash
        JOIN capabilities capability
          ON capability.token_hash=bridge.capability_hash AND capability.run_id=a.run_id
         AND capability.agent_id=a.agent_id AND capability.revoked_at IS NULL
        JOIN provider_state provider ON provider.run_id=a.run_id AND provider.agent_id=a.agent_id
       WHERE a.provider_session_ref IS NOT NULL AND a.lifecycle NOT IN ('archived', 'suspended')
         AND NOT EXISTS (
           SELECT 1 FROM project_session_launch_custody launch
            WHERE launch.coordination_run_id=a.run_id AND launch.chair_agent_id=a.agent_id
         )
         AND NOT EXISTS (
           SELECT 1 FROM lifecycle_rotation_custody_heads rotation
            WHERE rotation.run_id=a.run_id AND rotation.agent_id=a.agent_id AND rotation.terminal=0
         )
         AND NOT EXISTS (
           SELECT 1 FROM lifecycle_generation_loss_heads loss
            WHERE loss.run_id=a.run_id AND loss.agent_id=a.agent_id AND loss.terminal=0
         )
    `).all();
    for (const value of sessions) {
      const session = rowOrNotFound(value, "startup provider session");
      const runId = stringField(session, "run_id");
      const agentId = stringField(session, "agent_id");
      try {
        const status = await this.#requestAdapter(stringField(session, "adapter_id"), "status", {
          agentId,
          providerSessionRef: stringField(session, "provider_session_ref"),
        });
        if (!isRow(status)) throw new Error("adapter returned an invalid provider session status");
        if (status.healthy !== true) throw new Error("adapter did not prove the persisted provider session healthy");
        if (status.matches !== undefined && typeof status.matches !== "boolean") {
          throw new Error("adapter returned an invalid provider session match value");
        }
        if (status.matches === false) {
          throw new Error("adapter no longer manages the persisted provider session");
        }
      } catch (error: unknown) {
        const reason = error instanceof Error ? error.message : String(error);
        const storedContextRevision = session.context_revision;
        const currentContextRevision = storedContextRevision === null
          ? 0
          : typeof storedContextRevision === "bigint"
          ? Number(storedContextRevision)
          : typeof storedContextRevision === "number"
            ? storedContextRevision
            : typeof storedContextRevision === "string"
              ? Number(storedContextRevision)
              : Number.NaN;
        if (!Number.isSafeInteger(currentContextRevision) || currentContextRevision < 0) {
          throw new Error("startup provider context revision is invalid");
        }
        const contextRevision = currentContextRevision + 1;
        const evidence = {
          schemaVersion: 1,
          kind: "startup-provider-session-loss",
          runId,
          agentId,
          adapterId: stringField(session, "adapter_id"),
          providerSessionRef: stringField(session, "provider_session_ref"),
          providerSessionGeneration: numberField(session, "provider_session_generation"),
          contextRevision,
          reason,
        };
        const evidenceDigest = sha256Digest(canonicalJson(evidence));
        this.#database.transaction(() => {
          this.#generationLosses.recordObservationInCurrentTransaction({
            sourceEventId: `startup-provider-session-loss:${runId}:${agentId}:${contextRevision}`,
            projectSessionId: stringField(session, "project_session_id"),
            runId,
            agentId,
            providerGeneration: numberField(session, "provider_session_generation"),
            contextRevision,
            evidenceDigest,
            observedAt: this.#clock(),
            lossSource: {
              oldProviderSessionRef: stringField(session, "provider_session_ref"),
              newProviderSessionRef: stringField(session, "provider_session_ref"),
              sourceActionRef: {
                adapterId: stringField(session, "adapter_id"),
                actionId: stringField(session, "action_id"),
              },
              sourceAdapterContractDigest: stringField(session, "bridge_contract_digest"),
              sourcePrincipalGeneration: numberField(session, "principal_generation"),
              sourceBridgeGeneration: numberField(session, "bridge_generation"),
              bridgeOwnerKind: "child",
              sourceBridgeRowId: `${runId}:${agentId}`,
              sourceBridgeRevision: numberField(session, "bridge_revision"),
              sourceCapabilityHash: stringField(session, "capability_hash"),
              sourceProjectSessionGeneration: null,
              sourceRunGeneration: null,
              sourceChairLeaseGeneration: null,
              checkpoint: { state: "absent", ref: null, digest: null },
            },
          });
          const provider = this.#database.prepare(`
            UPDATE provider_state SET context_revision=?,reconciled_checkpoint_sha256=NULL
             WHERE run_id=? AND agent_id=? AND provider_session_generation=?
               AND COALESCE(CAST(context_revision AS INTEGER),0)=?
          `).run(
            contextRevision, runId, agentId,
            numberField(session, "provider_session_generation"), contextRevision - 1,
          );
          if (provider.changes !== 1) throw new Error("startup provider context changed before generation-loss commit");
          const agent = this.#database.prepare(`
            UPDATE agents SET lifecycle='context-unreconciled'
             WHERE run_id=? AND agent_id=? AND lifecycle NOT IN ('archived','suspended','context-unreconciled')
          `).run(runId, agentId);
          if (agent.changes !== 1) throw new Error("startup provider agent changed before generation-loss commit");
          this.#database.prepare(`
            INSERT INTO delivery_freezes(run_id,agent_id,reason,created_at)
            VALUES (?,?,'context-unreconciled',?)
            ON CONFLICT(run_id,agent_id) DO UPDATE SET reason=excluded.reason,created_at=excluded.created_at
          `).run(runId, agentId, this.#clock());
          this.#database.prepare(`
            UPDATE leases SET status='quarantined',updated_at=?
             WHERE run_id=? AND holder_agent_id=? AND status='active'
          `).run(this.#clock(), runId, agentId);
          this.#database.prepare(`
            UPDATE provider_session_turn_leases SET status='quarantined',updated_at=?
             WHERE run_id=? AND agent_id=? AND status='active'
          `).run(this.#clock(), runId, agentId);
        }).immediate();
        this.#event(runId, "startup-provider-session-degraded", null, { agentId, reason, evidenceDigest });
        sessionsDegraded += 1;
      }
    }
    return { actionsReconciled, actionsQuarantined, leasesQuarantined: expiredLeases.length, sessionsDegraded, deliveriesReleased };
  }

  bindCurrentMcpSeats(input: CurrentMcpSeatBindingInput): CurrentMcpSeatBindingResult {
    const canonicalRoot = canonicalWorkspaceRoot(input.canonicalRoot);
    if (canonicalRoot !== input.canonicalRoot) {
      throw new FabricError("AUTHORITY_WIDENING", "MCP binding requires the exact canonical project root");
    }
    if (!this.#workspaceRoots.some((root) => pathContains(root, canonicalRoot))) {
      throw new FabricError("AUTHORITY_WIDENING", "MCP binding project root is not configured");
    }
    const expiresAt = Date.parse(input.expiresAt);
    if (!Number.isFinite(expiresAt) || new Date(expiresAt).toISOString() !== input.expiresAt || expiresAt <= this.#clock()) {
      throw new FabricError("AUTHENTICATION_FAILED", "MCP seat credential expiry is invalid or elapsed");
    }
    if (input.bindings.length === 0) throw new FabricError("DEDUPE_CONFLICT", "MCP seat binding roster is empty");
    const seatIds = input.bindings.map(({ seat }) => seat);
    const agentIds = input.bindings.map(({ agentId }) => agentId);
    if (new Set(seatIds).size !== seatIds.length || new Set(agentIds).size !== agentIds.length) {
      throw new FabricError("DEDUPE_CONFLICT", "MCP seat bindings must name distinct seats and agents");
    }
    const chairBinding = input.bindings.find(({ agentId }) => agentId === input.chairAgentId);
    if (chairBinding === undefined) {
      throw new FabricError("DEDUPE_CONFLICT", "MCP seat bindings do not contain the exact current chair");
    }
    const derivedGeneration = currentMcpSeatGeneration({
      canonicalRoot,
      projectSessionId: input.projectSessionId,
      sessionRevision: input.expectedSessionRevision,
      sessionGeneration: input.expectedSessionGeneration,
      runId: input.runId,
      runRevision: input.expectedRunRevision,
      chairAgentId: input.chairAgentId,
      chairGeneration: input.expectedChairGeneration,
      chairLeaseId: input.chairLeaseId,
      expiresAt: input.expiresAt,
      bindings: input.bindings,
    });
    if (input.generation !== derivedGeneration.generation) {
      throw new FabricError("DEDUPE_CONFLICT", "MCP seat generation does not match its immutable binding");
    }

    return this.#database.transaction((): CurrentMcpSeatBindingResult => {
      const identity = rowOrNotFound(this.#database.prepare(`
        SELECT project.project_id, project.canonical_root, session.state AS session_state,
               session.revision AS session_revision, session.generation AS session_generation,
               session.origin_kind, session.origin_operator_id,
               run.lifecycle_state AS run_state, run.revision AS run_revision,
               run.chair_agent_id, run.chair_generation, run.chair_lease_id,
               lease.holder_agent_id, lease.generation AS lease_generation, lease.status AS lease_status
          FROM project_sessions session
          JOIN projects project ON project.project_id=session.project_id
          JOIN runs run ON run.project_session_id=session.project_session_id
          JOIN run_chair_leases lease
            ON lease.project_session_id=run.project_session_id
           AND lease.run_id=run.run_id
           AND lease.lease_id=run.chair_lease_id
         WHERE session.project_session_id=? AND run.run_id=?
      `).get(input.projectSessionId, input.runId), "current MCP project-session/run identity");
      const currentStates = new Set(["active", "visibility_degraded"]);
      if (
        stringField(identity, "canonical_root") !== canonicalRoot ||
        numberField(identity, "session_revision") !== input.expectedSessionRevision ||
        numberField(identity, "session_generation") !== input.expectedSessionGeneration ||
        numberField(identity, "run_revision") !== input.expectedRunRevision ||
        stringField(identity, "chair_agent_id") !== input.chairAgentId ||
        numberField(identity, "chair_generation") !== input.expectedChairGeneration ||
        stringField(identity, "chair_lease_id") !== input.chairLeaseId ||
        stringField(identity, "holder_agent_id") !== input.chairAgentId ||
        numberField(identity, "lease_generation") !== input.expectedChairGeneration
      ) {
        throw new FabricError("DEDUPE_CONFLICT", "MCP binding identity is stale or crossed");
      }
      if (
        stringField(identity, "origin_kind") !== "operator-launch" ||
        typeof identity.origin_operator_id !== "string" ||
        !currentStates.has(stringField(identity, "session_state")) ||
        !currentStates.has(stringField(identity, "run_state")) ||
        stringField(identity, "lease_status") !== "active"
      ) {
        throw new FabricError("LIFECYCLE_PRECONDITION_FAILED", "MCP binding target is not a current active operator-launched run");
      }

      const projectId = stringField(identity, "project_id");
      const activeValue = this.#database.prepare(`
        SELECT generation FROM mcp_active_seat_generations WHERE project_id=?
      `).get(projectId);
      const activeGeneration = activeValue === undefined
        ? null
        : stringField(rowOrNotFound(activeValue, "active MCP seat generation"), "generation");
      const storedValue = this.#database.prepare(`
        SELECT project_id,project_session_id,session_revision,session_generation,
               run_id,run_revision,chair_agent_id,chair_generation,chair_lease_id,
               previous_generation,binding_json,expires_at
          FROM mcp_seat_generations WHERE generation=?
      `).get(input.generation);
      const replay = storedValue !== undefined;
      if (replay) {
        const stored = rowOrNotFound(storedValue, "stored MCP seat generation");
        if (
          activeGeneration !== input.generation ||
          stored.project_id !== projectId ||
          stored.project_session_id !== input.projectSessionId ||
          stored.session_revision !== input.expectedSessionRevision ||
          stored.session_generation !== input.expectedSessionGeneration ||
          stored.run_id !== input.runId ||
          stored.run_revision !== input.expectedRunRevision ||
          stored.chair_agent_id !== input.chairAgentId ||
          stored.chair_generation !== input.expectedChairGeneration ||
          stored.chair_lease_id !== input.chairLeaseId ||
          stored.previous_generation !== input.expectedPreviousGeneration ||
          stored.binding_json !== derivedGeneration.bindingJson ||
          stored.expires_at !== expiresAt
        ) {
          throw new FabricError("DEDUPE_CONFLICT", "MCP seat generation replay is stale, crossed or changed");
        }
      } else {
        if (activeGeneration !== input.expectedPreviousGeneration) {
          throw new FabricError("DEDUPE_CONFLICT", "active MCP seat generation changed");
        }
        if (input.expectedPreviousGeneration === input.generation) {
          throw new FabricError("DEDUPE_CONFLICT", "MCP seat generation cannot replace itself");
        }
        this.#database.prepare(`
          INSERT INTO mcp_seat_generations(
            generation,project_id,project_session_id,session_revision,session_generation,
            run_id,run_revision,chair_agent_id,chair_generation,chair_lease_id,previous_generation,
            binding_json,binding_digest,expires_at,created_at
          ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
        `).run(
          input.generation,
          projectId,
          input.projectSessionId,
          input.expectedSessionRevision,
          input.expectedSessionGeneration,
          input.runId,
          input.expectedRunRevision,
          input.chairAgentId,
          input.expectedChairGeneration,
          input.chairLeaseId,
          input.expectedPreviousGeneration,
          derivedGeneration.bindingJson,
          `sha256:${input.generation}`,
          expiresAt,
          this.#clock(),
        );
      }

      const credentials = input.bindings
        .slice()
        .sort((left, right) => left.seat.localeCompare(right.seat))
        .map((binding) => {
          const agent = rowOrNotFound(this.#database.prepare(`
            SELECT agent.lifecycle, authority.authority_json, authority.authority_hash,
                   MAX(capability.principal_generation) AS principal_generation
              FROM agents agent
              JOIN authorities authority ON authority.authority_id=agent.authority_id
              JOIN capabilities capability
                ON capability.run_id=agent.run_id AND capability.agent_id=agent.agent_id
             WHERE agent.run_id=? AND agent.agent_id=?
               AND capability.revoked_at IS NULL AND capability.expires_at>?
             GROUP BY agent.lifecycle, authority.authority_json, authority.authority_hash
          `).get(input.runId, binding.agentId, this.#clock()), `current MCP agent ${binding.agentId}`);
          if (agent.lifecycle === "archived" || agent.lifecycle === "suspended") {
            throw new FabricError("LIFECYCLE_PRECONDITION_FAILED", `MCP agent ${binding.agentId} is not active`);
          }
          const principalGeneration = numberField(agent, "principal_generation");
          if (principalGeneration !== binding.expectedPrincipalGeneration) {
            throw new FabricError("STALE_PRINCIPAL_GENERATION", `MCP agent ${binding.agentId} principal generation changed`);
          }
          const authority = parseAuthority(agent);
          if (Date.parse(authority.expiresAt) < expiresAt) {
            throw new FabricError("AUTHORITY_WIDENING", `MCP credential for ${binding.agentId} outlives its authority`);
          }
          const capability = `afc_${createHmac("sha256", this.#capabilityKey)
            .update(canonicalJson({
              kind: "current-mcp-seat",
              canonicalRoot,
              projectSessionId: input.projectSessionId,
              sessionRevision: input.expectedSessionRevision,
              sessionGeneration: input.expectedSessionGeneration,
              runId: input.runId,
              runRevision: input.expectedRunRevision,
              chairAgentId: input.chairAgentId,
              chairGeneration: input.expectedChairGeneration,
              chairLeaseId: input.chairLeaseId,
              generation: input.generation,
              expiresAt: input.expiresAt,
              ...binding,
            }))
            .digest("base64url")}`;
          const tokenHash = sha256(capability);
          const existing = this.#database.prepare(`
            SELECT run_id, agent_id, principal_generation, expires_at, revoked_at
              FROM capabilities WHERE token_hash=?
          `).get(tokenHash);
          if (existing === undefined) {
            if (replay) {
              throw new FabricError("DEDUPE_CONFLICT", `MCP credential replay is missing for ${binding.agentId}`);
            }
            this.#database.prepare(`
              INSERT INTO capabilities(token_hash, run_id, agent_id, principal_generation, expires_at)
              VALUES (?, ?, ?, ?, ?)
            `).run(tokenHash, input.runId, binding.agentId, principalGeneration, expiresAt);
          } else if (
            !isRow(existing) ||
            existing.run_id !== input.runId ||
            existing.agent_id !== binding.agentId ||
            existing.principal_generation !== principalGeneration ||
            existing.expires_at !== expiresAt ||
            existing.revoked_at !== null
          ) {
            throw new FabricError("DEDUPE_CONFLICT", `MCP credential replay changed for ${binding.agentId}`);
          }
          const member = this.#database.prepare(`
            SELECT run_id,agent_id,principal_generation,token_hash,expires_at
              FROM mcp_seat_generation_members WHERE generation=? AND seat=?
          `).get(input.generation, binding.seat);
          if (replay) {
            if (
              !isRow(member) ||
              member.run_id !== input.runId ||
              member.agent_id !== binding.agentId ||
              member.principal_generation !== principalGeneration ||
              member.token_hash !== tokenHash ||
              member.expires_at !== expiresAt
            ) {
              throw new FabricError("DEDUPE_CONFLICT", `MCP seat generation replay changed for ${binding.seat}`);
            }
          } else {
            this.#database.prepare(`
              INSERT INTO mcp_seat_generation_members(
                generation,seat,run_id,agent_id,principal_generation,token_hash,expires_at
              ) VALUES (?,?,?,?,?,?,?)
            `).run(
              input.generation,
              binding.seat,
              input.runId,
              binding.agentId,
              principalGeneration,
              tokenHash,
              expiresAt,
            );
          }
          return { ...binding, capability };
        });
      if (!replay) {
        if (input.expectedPreviousGeneration !== null) {
          this.#database.prepare(`
            UPDATE capabilities SET revoked_at=?
             WHERE revoked_at IS NULL AND token_hash IN (
               SELECT token_hash FROM mcp_seat_generation_members WHERE generation=?
             )
          `).run(this.#clock(), input.expectedPreviousGeneration);
          const updated = this.#database.prepare(`
            UPDATE mcp_active_seat_generations
               SET generation=?,activated_at=?
             WHERE project_id=? AND generation=?
          `).run(input.generation, this.#clock(), projectId, input.expectedPreviousGeneration);
          if (updated.changes !== 1) {
            throw new FabricError("DEDUPE_CONFLICT", "active MCP seat generation changed during rotation");
          }
        } else {
          this.#database.prepare(`
            INSERT INTO mcp_active_seat_generations(project_id,generation,activated_at)
            VALUES (?,?,?)
          `).run(projectId, input.generation, this.#clock());
        }
      }
      return {
        expectedPreviousGeneration: input.expectedPreviousGeneration,
        generation: input.generation,
        projectSessionId: input.projectSessionId,
        sessionRevision: input.expectedSessionRevision,
        sessionGeneration: input.expectedSessionGeneration,
        runId: input.runId,
        runRevision: input.expectedRunRevision,
        chairAgentId: input.chairAgentId,
        chairGeneration: input.expectedChairGeneration,
        chairLeaseId: input.chairLeaseId,
        expiresAt: input.expiresAt,
        credentials,
      };
    })();
  }

  bootstrapTrustedCurrentMcpSeat(
    input: BootstrapMcpSeatInput,
    revalidatedWorkspace: { canonicalRoot: string; trustRecordDigest: string },
  ): BootstrapMcpSeatResult {
    if (
      canonicalWorkspaceRoot(revalidatedWorkspace.canonicalRoot) !== input.canonicalRoot ||
      revalidatedWorkspace.canonicalRoot !== input.canonicalRoot ||
      revalidatedWorkspace.trustRecordDigest !== input.trustRecordDigest
    ) {
      throw new FabricError("AUTHENTICATION_FAILED", "revalidated MCP workspace binding changed");
    }
    if (!this.#workspaceRoots.includes(revalidatedWorkspace.canonicalRoot)) {
      this.#workspaceRoots.push(revalidatedWorkspace.canonicalRoot);
      this.#workspaceRoots.sort();
    }
    return this.bootstrapCurrentMcpSeat(input);
  }

  bootstrapCurrentMcpSeat(input: BootstrapMcpSeatInput): BootstrapMcpSeatResult {
    return bootstrapMcpSeatCustody({
      database: this.#database,
      clock: this.#clock,
      productRoot: this.#productRoot,
      workspaceRoots: this.#workspaceRoots,
      capabilityKey: this.#capabilityKey,
      canonicalWorkspaceRoot,
      normaliseAuthority,
      bindCurrentMcpSeats: (binding) => this.bindCurrentMcpSeats(binding),
    }, input);
  }

  connect(token: string): FabricClient {
    const row = rowOrNotFound(
      this.#database
        .prepare(`
          SELECT capability.run_id,capability.agent_id,capability.expires_at,capability.revoked_at,
                 member.generation AS mcp_seat_generation,
                 active.generation AS active_mcp_seat_generation
            FROM capabilities capability
            LEFT JOIN mcp_seat_generation_members member ON member.token_hash=capability.token_hash
            LEFT JOIN current_mcp_seat_generation_members active ON active.token_hash=capability.token_hash
           WHERE capability.token_hash=?
        `)
        .get(sha256(token)),
      "capability",
    );
    if (row.revoked_at !== null || numberField(row, "expires_at") <= this.#clock()) {
      throw new FabricError("AUTHENTICATION_FAILED", "capability is expired or revoked");
    }
    assertActiveMcpSeatGeneration(row, this.#database, sha256(token));
    return new FabricClient(this, stringField(row, "run_id"), stringField(row, "agent_id"), sha256(token));
  }

  verifyProtocolCredential(token: string): VerifiedProtocolCredential {
    const authenticated = this.#database.prepare(`
      SELECT c.run_id, c.agent_id, c.principal_generation, c.expires_at, c.revoked_at,
             a.authority_json, a.authority_hash, r.project_session_id,
             member.generation AS mcp_seat_generation,
             active.generation AS active_mcp_seat_generation
        FROM capabilities c
        JOIN agents g ON g.run_id=c.run_id AND g.agent_id=c.agent_id
        JOIN authorities a ON a.authority_id=g.authority_id
        JOIN runs r ON r.run_id=c.run_id
        LEFT JOIN mcp_seat_generation_members member ON member.token_hash=c.token_hash
        LEFT JOIN current_mcp_seat_generation_members active ON active.token_hash=c.token_hash
       WHERE c.token_hash=?
    `).get(sha256(token));
    if (!isRow(authenticated)) {
      const operator = this.#operatorStore.authenticateCredential(token);
      return {
        principal: {
          kind: "operator",
          operatorId: operator.context.operatorId,
          projectId: operator.context.projectId,
          projectAuthorityGeneration: operator.context.projectAuthorityGeneration,
          principalGeneration: operator.context.principalGeneration,
        },
        grantedOperations: operatorOperationsForActions(operator.actions),
      };
    }
    if (authenticated.revoked_at !== null || numberField(authenticated, "expires_at") <= this.#clock()) {
      throw new FabricError("AUTHENTICATION_FAILED", "protocol credential is expired or revoked");
    }
    assertActiveMcpSeatGeneration(authenticated, this.#database, sha256(token));
    const authority = parseAuthority(authenticated);
    const denied = new Set(authority.deniedActions);
    return {
      principal: {
        kind: "agent",
        agentId: stringField(authenticated, "agent_id") as never,
        projectSessionId: stringField(authenticated, "project_session_id") as never,
        runId: stringField(authenticated, "run_id"),
        principalGeneration: numberField(authenticated, "principal_generation"),
      },
      grantedOperations: authority.actions.filter((operation) => !denied.has(operation) && (operation !== FABRIC_OPERATIONS.whoami || authenticated.active_mcp_seat_generation !== null)),
    };
  }

  #writeClosedLaunchArtifact(
    root: string,
    relativePath: string,
    content: string,
  ): Readonly<{ device: string; inode: string }> {
    const destination = resolve(root, canonicalAuthorityPath(root, relativePath));
    mkdirSync(dirname(destination), { recursive: true, mode: 0o700 });
    try {
      writeFileSync(destination, content, { encoding: "utf8", mode: 0o600, flag: "wx" });
    } catch (error: unknown) {
      if (typeof error === "object" && error !== null && "code" in error && error.code === "EEXIST") {
        throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "launch staging path is already occupied");
      }
      throw error;
    }
    const identity = statSync(destination, { bigint: true });
    return { device: identity.dev.toString(), inode: identity.ino.toString() };
  }

  #removeLaunchPreparationFile(
    root: string,
    relativePath: unknown,
    expectedDigest: unknown,
    expectedDevice: unknown,
    expectedInode: unknown,
  ): boolean {
    if (typeof relativePath !== "string") return true;
    const destination = this.#quarantineLaunchPreparationFile(
      root,
      relativePath,
      "launch-preparation:before-committed-stage-quarantine",
    );
    if (destination === null) return true;
    if (
      typeof expectedDigest !== "string" || typeof expectedDevice !== "string" ||
      typeof expectedInode !== "string"
    ) return true;
    const identity = statSync(destination, { bigint: true });
    if (identity.dev.toString() !== expectedDevice || identity.ino.toString() !== expectedInode) return true;
    if (sha256Digest(readFileSync(destination, "utf8")) !== expectedDigest) return true;
    unlinkSync(destination);
    return true;
  }

  #quarantineLaunchPreparationFile(root: string, relativePath: string, faultLabel: string): string | null {
    const source = resolve(root, canonicalAuthorityPath(root, relativePath));
    const quarantineDirectory = resolve(root, canonicalAuthorityPath(root, ".agent-run/.fabric-custody"));
    mkdirSync(quarantineDirectory, { recursive: true, mode: 0o700 });
    const destination = resolve(
      quarantineDirectory,
      `custody-${randomBytes(16).toString("hex")}`,
    );
    this.#fault(faultLabel);
    try {
      renameSync(source, destination);
      return destination;
    } catch (error: unknown) {
      if (typeof error === "object" && error !== null && "code" in error && error.code === "ENOENT") return null;
      throw error;
    }
  }

  #compensateLaunchPreparationPair(
    root: string,
    stagedPath: unknown,
    publishedPath: unknown,
    expectedDigest: unknown,
    expectedDevice: unknown,
    expectedInode: unknown,
  ): boolean {
    if (typeof publishedPath !== "string" || typeof expectedDigest !== "string") {
      return stagedPath === null && publishedPath === null;
    }
    if (typeof stagedPath !== "string") {
      const published = resolve(root, canonicalAuthorityPath(root, publishedPath));
      return !existsSync(published);
    }
    const published = resolve(root, canonicalAuthorityPath(root, publishedPath));
    const staged = this.#quarantineLaunchPreparationFile(
      root,
      stagedPath,
      "launch-preparation:before-compensation-stage-quarantine",
    );
    if (typeof expectedDevice !== "string" || typeof expectedInode !== "string") {
      return !existsSync(published);
    }
    if (staged === null) return !existsSync(published);
    const stagedIdentity = statSync(staged, { bigint: true });
    if (
      stagedIdentity.dev.toString() !== expectedDevice ||
      stagedIdentity.ino.toString() !== expectedInode
    ) return false;
    if (sha256Digest(readFileSync(staged, "utf8")) !== expectedDigest) return false;
    const quarantinedPublished = this.#quarantineLaunchPreparationFile(
      root,
      publishedPath,
      "launch-preparation:before-compensation-published-quarantine",
    );
    if (quarantinedPublished !== null) {
      const publishedIdentity = statSync(quarantinedPublished, { bigint: true });
      if (
        stagedIdentity.dev !== publishedIdentity.dev || stagedIdentity.ino !== publishedIdentity.ino ||
        sha256Digest(readFileSync(quarantinedPublished, "utf8")) !== expectedDigest
      ) return false;
      unlinkSync(quarantinedPublished);
    }
    unlinkSync(staged);
    return true;
  }

  #cleanCommittedLaunchPreparationStages(row: Row): void {
    this.#database.transaction(() => {
      const current = rowOrNotFound(this.#database.prepare(`
        SELECT * FROM project_session_launch_preparations
         WHERE operator_id=? AND command_id=?
      `).get(stringField(row, "operator_id"), stringField(row, "command_id")), "committed launch preparation");
      if (stringField(current, "status") !== "committed") {
        throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "launch preparation custody changed during committed cleanup");
      }
      if (current.staged_launch_packet_path === null && current.staged_resource_plan_path === null) return;
      const project = rowOrNotFound(this.#database.prepare(`
        SELECT canonical_root FROM projects WHERE project_id=?
      `).get(stringField(current, "project_id")), "project");
      const root = realpathSync(stringField(project, "canonical_root"));
      const removed = [
        this.#removeLaunchPreparationFile(
          root,
          current.staged_launch_packet_path,
          current.launch_packet_digest,
          current.staged_launch_packet_device,
          current.staged_launch_packet_inode,
        ),
        this.#removeLaunchPreparationFile(
          root,
          current.staged_resource_plan_path,
          current.resource_plan_digest,
          current.staged_resource_plan_device,
          current.staged_resource_plan_inode,
        ),
      ];
      if (removed.includes(false)) {
        throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "committed launch preparation staging bytes changed");
      }
      const cleared = this.#database.prepare(`
        UPDATE project_session_launch_preparations
           SET staged_launch_packet_path=NULL, staged_resource_plan_path=NULL,
               staged_launch_packet_device=NULL, staged_launch_packet_inode=NULL,
               staged_resource_plan_device=NULL, staged_resource_plan_inode=NULL,
               updated_at=?
         WHERE operator_id=? AND command_id=? AND status='committed'
           AND staged_launch_packet_path IS ? AND staged_resource_plan_path IS ?
           AND staged_launch_packet_device IS ? AND staged_launch_packet_inode IS ?
           AND staged_resource_plan_device IS ? AND staged_resource_plan_inode IS ?
      `).run(
        this.#clock(),
        stringField(current, "operator_id"),
        stringField(current, "command_id"),
        current.staged_launch_packet_path,
        current.staged_resource_plan_path,
        current.staged_launch_packet_device,
        current.staged_launch_packet_inode,
        current.staged_resource_plan_device,
        current.staged_resource_plan_inode,
      );
      if (cleared.changes !== 1) {
        throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "launch preparation custody changed during committed cleanup");
      }
    })();
  }

  #compensateLaunchPreparation(row: Row): void {
    const project = rowOrNotFound(this.#database.prepare(`
      SELECT canonical_root FROM projects WHERE project_id=?
    `).get(stringField(row, "project_id")), "project");
    const root = realpathSync(stringField(project, "canonical_root"));
    const removed = [
      this.#compensateLaunchPreparationPair(
        root,
        row.staged_launch_packet_path,
        row.launch_packet_path,
        row.launch_packet_digest,
        row.staged_launch_packet_device,
        row.staged_launch_packet_inode,
      ),
      this.#compensateLaunchPreparationPair(
        root,
        row.staged_resource_plan_path,
        row.resource_plan_path,
        row.resource_plan_digest,
        row.staged_resource_plan_device,
        row.staged_resource_plan_inode,
      ),
    ];
    if (removed.includes(false)) {
      throw new ProjectFabricCoreError(
        "DEDUPE_CONFLICT",
        "interrupted launch preparation artifact changed before custody recovery",
      );
    }
    this.#database.prepare(`
      UPDATE project_session_launch_preparations
         SET status='claimed', launch_packet_path=NULL, launch_packet_digest=NULL,
             resource_plan_path=NULL, resource_plan_digest=NULL,
             staged_launch_packet_path=NULL, staged_resource_plan_path=NULL,
             staged_launch_packet_device=NULL, staged_launch_packet_inode=NULL,
             staged_resource_plan_device=NULL, staged_resource_plan_inode=NULL,
             updated_at=?
       WHERE operator_id=? AND command_id=? AND status='staged'
    `).run(this.#clock(), stringField(row, "operator_id"), stringField(row, "command_id"));
  }

  #recoverLaunchPreparations(): void {
    const staged = this.#database.prepare(`
      SELECT * FROM project_session_launch_preparations WHERE status='staged'
    `).all() as Row[];
    for (const row of staged) this.#compensateLaunchPreparation(row);
    const committed = this.#database.prepare(`
      SELECT * FROM project_session_launch_preparations WHERE status='committed'
    `).all() as Row[];
    for (const row of committed) this.#cleanCommittedLaunchPreparationStages(row);
  }

  #claimLaunchPreparation(
    credential: AuthenticatedOperatorCredential,
    request: ProjectSessionLaunchPacketPrepareRequest,
  ): ProjectSessionLaunchPacketPreparation | undefined {
    const commandPayload = {
      ...request,
      command: {
        ...request.command,
        credential: { capabilityId: request.command.credential.capabilityId },
      },
    };
    const payloadHash = sha256Digest(canonicalJson(commandPayload));
    this.#operatorStore.authenticateCommand(credential.context, request.command, {
      projectId: request.projectId,
      projectSessionId: request.projectSessionId,
      sessionGeneration: request.expectedSessionGeneration,
      requiredAction: "launch",
      commandPayload,
    });
    const claim = this.#database.transaction((): Row => {
      const existing = this.#database.prepare(`
        SELECT * FROM project_session_launch_preparations
         WHERE operator_id=? AND command_id=?
      `).get(credential.context.operatorId, request.command.commandId);
      if (isRow(existing)) {
        if (stringField(existing, "payload_hash") !== payloadHash) {
          throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "Implement command ID was reused with changed input");
        }
        return existing;
      }
      this.#database.prepare(`
        INSERT INTO project_session_launch_preparations(
          operator_id, command_id, capability_id, project_id, project_session_id,
          session_generation, payload_hash, status, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, 'claimed', ?, ?)
      `).run(
        credential.context.operatorId,
        request.command.commandId,
        request.command.credential.capabilityId,
        request.projectId,
        request.projectSessionId,
        request.expectedSessionGeneration,
        payloadHash,
        this.#clock(),
        this.#clock(),
      );
      return rowOrNotFound(this.#database.prepare(`
        SELECT * FROM project_session_launch_preparations
         WHERE operator_id=? AND command_id=?
      `).get(credential.context.operatorId, request.command.commandId), "launch preparation claim");
    })();
    if (stringField(claim, "status") === "committed") {
      this.#cleanCommittedLaunchPreparationStages(claim);
      return JSON.parse(stringField(claim, "result_json")) as ProjectSessionLaunchPacketPreparation;
    }
    if (stringField(claim, "status") === "staged") this.#compensateLaunchPreparation(claim);
    return undefined;
  }

  #prepareProjectSessionImplementation(
    credential: AuthenticatedOperatorCredential,
    request: ProjectSessionLaunchPacketPrepareRequest,
  ): ProjectSessionLaunchPacketPreparation {
    const context = credential.context;
    assertSafeLaunchProviderInput(request.launchPacket.provider.input);
    const replay = this.#claimLaunchPreparation(credential, request);
    if (replay !== undefined) return replay;
    const session = rowOrNotFound(this.#database.prepare(`
      SELECT * FROM project_sessions WHERE project_session_id=? AND project_id=?
    `).get(request.projectSessionId, request.projectId), "project session");
    if (
      request.projectId !== context.projectId ||
      credential.projectSessionId !== request.projectSessionId ||
      credential.sessionGeneration !== request.expectedSessionGeneration ||
      numberField(session, "generation") !== request.expectedSessionGeneration ||
      numberField(session, "revision") !== request.command.expectedRevision
    ) {
      throw new ProjectFabricCoreError("STALE_REVISION", "Implement requires the exact current project-session binding");
    }
    if (stringField(session, "state") !== "draft") {
      throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "Implement requires a draft project session");
    }
    const accepted = this.#database.prepare(`
      SELECT 1 FROM intakes intake
      JOIN artifacts artifact ON artifact.artifact_id=intake.accepted_scope_artifact_id
       WHERE intake.intake_id=? AND intake.project_id=?
         AND intake.state='accepted' AND intake.accepted_scope_state='bound'
         AND artifact.registry_state='active'
         AND artifact.relative_path=? AND artifact.sha256=?
    `).get(
      request.intakeId,
      request.projectId,
      request.acceptedScopeRef.path,
      request.acceptedScopeRef.digest,
    );
    if (accepted === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "Implement requires the exact current accepted scope artifact");
    }
    const packet = request.launchPacket;
    const plan = request.resourcePlan;
    if (
      packet.projectId !== request.projectId || packet.projectSessionId !== request.projectSessionId ||
      plan.projectId !== request.projectId || plan.projectSessionId !== request.projectSessionId ||
      packet.runId !== plan.runId || packet.topologyMode !== stringField(session, "mode") ||
      packet.budgetRef !== stringField(session, "budget_ref") || plan.budgetRef !== packet.budgetRef
    ) {
      throw new ProjectFabricCoreError("STALE_REVISION", "launch packet, resource plan and project session identities differ");
    }
    if (packet.chairAuthority.approval.evidenceDigest !== request.acceptedScopeRef.digest) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "chair authority is not bound to the accepted scope digest");
    }
    if (Date.parse(packet.chairAuthority.expiresAt) <= this.#clock()) {
      throw new ProjectFabricCoreError("CAPABILITY_EXPIRED", "chair authority expired before Implement confirmation");
    }
    if (canonicalJson(packet.chairAuthority.budget) !== canonicalJson(plan.scopes.coordinationRun.limits)) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "chair authority budget must equal coordination-run limits");
    }
    const resourcePlanText = canonicalJson(plan);
    const resourcePlanDigest = sha256Digest(resourcePlanText);
    if (
      packet.resourcePlanRef.path !== request.resourcePlanRef.path ||
      packet.resourcePlanRef.digest !== request.resourcePlanRef.digest ||
      request.resourcePlanRef.digest !== resourcePlanDigest
    ) {
      throw new ProjectFabricCoreError("STALE_REVISION", "resource plan digest changed since review");
    }
    const launchPacketText = canonicalJson(packet);
    const launchPacketDigest = sha256Digest(launchPacketText);
    if (request.launchPacketRef.digest !== launchPacketDigest) {
      throw new ProjectFabricCoreError("STALE_REVISION", "launch packet digest changed since review");
    }
    if (request.launchPacketRef.path === request.resourcePlanRef.path) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "launch packet and resource plan require distinct artifact paths");
    }
    const project = rowOrNotFound(this.#database.prepare(`
      SELECT canonical_root FROM projects WHERE project_id=?
    `).get(request.projectId), "project");
    const root = realpathSync(stringField(project, "canonical_root"));
    const authority = normaliseLaunchChairAuthority(packet.chairAuthority, root);
    const projectRunDirectory = canonicalAuthorityPath(root, packet.projectRunDirectory);
    const launchPacketPath = canonicalAuthorityPath(root, request.launchPacketRef.path);
    const resourcePlanPath = canonicalAuthorityPath(root, request.resourcePlanRef.path);
    const artifactPaths = [launchPacketPath, resourcePlanPath];
    if (
      artifactPaths.some((path) => !pathContains(projectRunDirectory, path)) ||
      artifactPaths.some((path) => !authority.artifactPaths.some((scope) => pathContains(scope, path))) ||
      artifactPaths.some((path) => authority.deniedPaths.some((scope) => pathContains(scope, path)))
    ) {
      throw new ProjectFabricCoreError(
        "CAPABILITY_FORBIDDEN",
        "launch artifacts must remain inside the reviewed run directory and chair artifact scope",
      );
    }
    const stageSuffix = randomBytes(12).toString("hex");
    const stagedLaunchPacketPath = `${request.launchPacketRef.path}.prepare-${stageSuffix}`;
    const stagedResourcePlanPath = `${request.resourcePlanRef.path}.prepare-${stageSuffix}`;
    this.#database.prepare(`
      UPDATE project_session_launch_preparations
         SET status='staged', launch_packet_path=?, launch_packet_digest=?,
             resource_plan_path=?, resource_plan_digest=?, staged_launch_packet_path=?,
             staged_resource_plan_path=?, updated_at=?
       WHERE operator_id=? AND command_id=? AND status='claimed'
    `).run(
      launchPacketPath,
      request.launchPacketRef.digest,
      resourcePlanPath,
      request.resourcePlanRef.digest,
      stagedLaunchPacketPath,
      stagedResourcePlanPath,
      this.#clock(),
      context.operatorId,
      request.command.commandId,
    );
    try {
      this.#fault("launch-preparation:before-stage-write");
      const stagedResourcePlanIdentity = this.#writeClosedLaunchArtifact(root, stagedResourcePlanPath, resourcePlanText);
      this.#fault("launch-preparation:after-resource-stage-write");
      this.#database.prepare(`
        UPDATE project_session_launch_preparations
           SET staged_resource_plan_device=?, staged_resource_plan_inode=?, updated_at=?
         WHERE operator_id=? AND command_id=? AND status='staged'
           AND staged_resource_plan_path=?
      `).run(
        stagedResourcePlanIdentity.device,
        stagedResourcePlanIdentity.inode,
        this.#clock(),
        context.operatorId,
        request.command.commandId,
        stagedResourcePlanPath,
      );
      const stagedLaunchPacketIdentity = this.#writeClosedLaunchArtifact(root, stagedLaunchPacketPath, launchPacketText);
      this.#fault("launch-preparation:after-launch-stage-write");
      this.#database.prepare(`
        UPDATE project_session_launch_preparations
           SET staged_launch_packet_device=?, staged_launch_packet_inode=?, updated_at=?
         WHERE operator_id=? AND command_id=? AND status='staged'
           AND staged_launch_packet_path=?
      `).run(
        stagedLaunchPacketIdentity.device,
        stagedLaunchPacketIdentity.inode,
        this.#clock(),
        context.operatorId,
        request.command.commandId,
        stagedLaunchPacketPath,
      );
      linkSync(resolve(root, stagedResourcePlanPath), resolve(root, resourcePlanPath));
      this.#fault("launch-preparation:after-resource-publish");
      linkSync(resolve(root, stagedLaunchPacketPath), resolve(root, launchPacketPath));
      this.#fault("launch-preparation:after-launch-publish");
      this.#fault("launch-preparation:before-transition");
      const result = this.#database.transaction((): ProjectSessionLaunchPacketPreparation => {
        const projectSession = this.#projectSessions.transitionProjectSession(context, {
          command: request.command,
          projectSessionId: request.projectSessionId,
          expectedGeneration: request.expectedSessionGeneration,
          transition: {
            to: "awaiting_launch",
            reason: `accepted evidence ${request.acceptedScopeRef.digest}`,
            launchPacketRef: request.launchPacketRef,
          },
        }, "launch");
        const prepared = {
          projectSession,
          launchPacketRef: request.launchPacketRef,
          resourcePlanRef: request.resourcePlanRef,
          acceptedScopeRef: request.acceptedScopeRef,
        };
        this.#database.prepare(`
          UPDATE project_session_launch_preparations
             SET status='committed', result_json=?, updated_at=?
           WHERE operator_id=? AND command_id=? AND status='staged'
        `).run(canonicalJson(prepared), this.#clock(), context.operatorId, request.command.commandId);
        return prepared;
      })();
      const committed = rowOrNotFound(this.#database.prepare(`
        SELECT * FROM project_session_launch_preparations
         WHERE operator_id=? AND command_id=?
      `).get(context.operatorId, request.command.commandId), "committed launch preparation");
      this.#fault("launch-preparation:before-committed-stage-cleanup");
      this.#cleanCommittedLaunchPreparationStages(committed);
      this.#fault("launch-preparation:after-commit");
      return result;
    } catch (error: unknown) {
      const stored = rowOrNotFound(this.#database.prepare(`
        SELECT * FROM project_session_launch_preparations
         WHERE operator_id=? AND command_id=?
      `).get(context.operatorId, request.command.commandId), "launch preparation custody");
      if (stringField(stored, "status") === "staged") this.#compensateLaunchPreparation(stored);
      throw error;
    }
  }

  async dispatchPublicProtocol(
    context: PublicProtocolContext,
    operation: ProtocolOperation,
    input: OperationInputMap[ProtocolOperation],
  ): Promise<unknown> {
    if (!context.allowedOperations.has(operation)) {
      throw new FabricError("CAPABILITY_FORBIDDEN", `connection does not permit ${operation}`);
    }
    if (context.principal.kind === "agent") {
      const definition = OPERATION_REGISTRY[operation];
      if (!definition.principals.includes("agent")) {
        throw new FabricError("CAPABILITY_FORBIDDEN", "operation is not available to agent principals");
      }
      this.assertCapability(
        context.principal.runId,
        context.principal.agentId,
        context.credentialHash,
        operation,
      );
      if (definition.gateOwner !== "scoped-gate") {
        assertScopedOperationAllowed(
          this.#database,
          context.principal.runId,
          operation,
          this.#agentOperationGateTarget(
            context.principal.runId,
            context.principal.agentId,
            operation,
            input,
          ),
        );
      }
      if (definition.kind === "baseline") {
        return dispatchAgentProtocol(
          new FabricClient(
            this,
            context.principal.runId,
            context.principal.agentId,
            context.credentialHash,
          ),
          operation as never,
          input as never,
        );
      }
      const agent = {
        agentId: context.principal.agentId,
        projectSessionId: context.principal.projectSessionId,
        coordinationRunId: context.principal.runId as never,
        principalGeneration: context.principal.principalGeneration,
      };
      switch (operation) {
        case FABRIC_OPERATIONS.membershipBind: {
          const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.membershipBind];
          if (request.origin !== "chair") {
            throw new FabricError("CAPABILITY_FORBIDDEN", "agent membership binding requires chair origin");
          }
          return this.#projectSessions.bindMembership(agent, request);
        }
        case FABRIC_OPERATIONS.intakeRevise: {
          const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.intakeRevise];
          if (request.origin !== "chair") {
            throw new FabricError("CAPABILITY_FORBIDDEN", "agent intake revision requires chair origin");
          }
          return this.#intakes.revise(agent, request);
        }
        case FABRIC_OPERATIONS.scopedGateCreate: {
          const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.scopedGateCreate];
          if (request.origin !== "chair") {
            throw new FabricError("CAPABILITY_FORBIDDEN", "agent gate creation requires chair origin");
          }
          const existing = this.#gates.getGateByDedupe(
            agent,
            request.intent.projectSessionId,
            request.intent.coordinationRunId,
            request.intent.dedupeKey,
          );
          if (existing?.status === "superseded" && existing.resolution.kind === "system-supersession" &&
            !context.features.includes(GATE_SYSTEM_SUPERSESSION_FEATURE)) {
            throw new ProjectFabricCoreError("FEATURE_UNAVAILABLE", "gate system-supersession result shape was not negotiated");
          }
          return this.#gates.createGate(agent, request);
        }
        case FABRIC_OPERATIONS.scopedGateCheck:
          return this.#gates.check(
            agent,
            input as OperationInputMap[typeof FABRIC_OPERATIONS.scopedGateCheck],
          );
        case FABRIC_OPERATIONS.resourceReserve:
          return this.#resources.reserve(
            agent,
            input as OperationInputMap[typeof FABRIC_OPERATIONS.resourceReserve],
          );
        case FABRIC_OPERATIONS.resourceRelease:
          return this.#resources.release(
            agent,
            input as OperationInputMap[typeof FABRIC_OPERATIONS.resourceRelease],
          );
        case FABRIC_OPERATIONS.resourceReconcile:
          return this.#resources.reconcile(
            agent,
            input as OperationInputMap[typeof FABRIC_OPERATIONS.resourceReconcile],
          );
        case FABRIC_OPERATIONS.workstreamCreate:
        case FABRIC_OPERATIONS.workstreamSettle:
        case FABRIC_OPERATIONS.runPlanDeclare:
          return dispatchProjectDeliveryOperation(operation, agent, input, this.#workstreams, this.#runPlans);
        case FABRIC_OPERATIONS.taskRequest:
          return this.#results.request(
            agent,
            input as OperationInputMap[typeof FABRIC_OPERATIONS.taskRequest],
          );
        case FABRIC_OPERATIONS.taskCompleteWithReply:
          return this.#results.completeWithReply(
            agent,
            input as OperationInputMap[typeof FABRIC_OPERATIONS.taskCompleteWithReply],
          );
        case FABRIC_OPERATIONS.resultDeliveryClaim:
          return this.#results.claim(
            agent,
            input as OperationInputMap[typeof FABRIC_OPERATIONS.resultDeliveryClaim],
          );
        case FABRIC_OPERATIONS.resultDeliveryConsume:
          return this.#results.consume(
            agent,
            input as OperationInputMap[typeof FABRIC_OPERATIONS.resultDeliveryConsume],
          );
        case FABRIC_OPERATIONS.resultDeliveryRetry:
          return this.#results.retry(
            agent,
            input as OperationInputMap[typeof FABRIC_OPERATIONS.resultDeliveryRetry],
          );
        case FABRIC_OPERATIONS.resultDeliveryReassign:
          return this.#results.reassign(
            agent,
            input as OperationInputMap[typeof FABRIC_OPERATIONS.resultDeliveryReassign],
          );
        case FABRIC_OPERATIONS.resultDeliveryAbandon:
          return this.#results.abandon(
            agent,
            input as OperationInputMap[typeof FABRIC_OPERATIONS.resultDeliveryAbandon],
          );
        case FABRIC_OPERATIONS.evidencePublish:
          return this.publishEvidence(
            context.principal.runId,
            context.principal.agentId,
            input as OperationInputMap[typeof FABRIC_OPERATIONS.evidencePublish],
          );
        case FABRIC_OPERATIONS.herdrSteerDispatch: {
          const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.herdrSteerDispatch];
          const run = rowOrNotFound(this.#database.prepare(`
            SELECT session.project_id, coordination.project_session_id
              FROM runs coordination
              JOIN project_sessions session
                ON session.project_session_id=coordination.project_session_id
             WHERE coordination.run_id=?
          `).get(context.principal.runId), "Herdr steering coordination run");
          const result = await this.#herdr.executeDirectSteer({
            ...request,
            reference: {
              ...request.reference,
              projectId: stringField(run, "project_id") as never,
              projectSessionId: context.principal.projectSessionId,
              coordinationRunId: context.principal.runId as never,
            },
          });
          return publicHerdrSteerResult(result);
        }
        default:
          throw Object.assign(new Error(`agent protocol operation is not wired: ${operation}`), {
            code: "PROTOCOL_UNSUPPORTED",
          });
      }
    }
    const operatorCredential = (): AuthenticatedOperatorCredential => {
      if (context.principal.kind !== "operator") {
        throw new FabricError("CAPABILITY_FORBIDDEN", "operation requires an operator principal");
      }
      const credential = this.#operatorStore.authenticateCredentialHash(context.credentialHash);
      if (
        credential.context.operatorId !== context.principal.operatorId ||
        credential.context.projectId !== context.principal.projectId ||
        credential.context.projectAuthorityGeneration !== context.principal.projectAuthorityGeneration ||
        credential.context.principalGeneration !== context.principal.principalGeneration
      ) {
        throw new FabricError("AUTHENTICATION_FAILED", "operator connection principal changed");
      }
      return credential;
    };
    const operatorCommand = (
      credential: AuthenticatedOperatorCredential,
      command: { credential: { capabilityId: string; token: string } },
    ): void => {
      if (
        command.credential.capabilityId !== credential.capabilityId ||
        sha256(command.credential.token) !== context.credentialHash
      ) {
        throw new FabricError("AUTHENTICATION_FAILED", "operator command credential differs from its connection");
      }
    };

    switch (operation) {
      case FABRIC_OPERATIONS.projectSessionCreate: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.projectSessionCreate];
        const credential = operatorCredential();
        operatorCommand(credential, request.command);
        if (request.projectId !== credential.context.projectId) throw new ProjectFabricCoreError("WRONG_PROJECT", "session is outside the operator project");
        return this.#projectSessions.createProjectSession(credential.context, request);
      }
      case FABRIC_OPERATIONS.projectSessionGet: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.projectSessionGet];
        const credential = operatorCredential();
        if (request.projectId !== credential.context.projectId) throw new ProjectFabricCoreError("WRONG_PROJECT", "session is outside the operator project");
        return this.#projectSessions.getProjectSession(request);
      }
      case FABRIC_OPERATIONS.projectSessionTransition: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.projectSessionTransition];
        const credential = operatorCredential();
        operatorCommand(credential, request.command);
        return this.#projectSessions.transitionProjectSession(credential.context, request);
      }
      case FABRIC_OPERATIONS.projectSessionClose: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.projectSessionClose];
        const credential = operatorCredential();
        operatorCommand(credential, request.command);
        return this.#projectSessions.closeProjectSession(credential.context, request);
      }
      case FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.projectSessionLaunchPacketPrepare];
        const credential = operatorCredential();
        operatorCommand(credential, request.command);
        return this.#prepareProjectSessionImplementation(credential, request);
      }
      case FABRIC_OPERATIONS.projectSessionLaunchPrepare: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.projectSessionLaunchPrepare];
        const credential = operatorCredential();
        operatorCommand(credential, request.command);
        if (
          request.projectId !== credential.context.projectId ||
          credential.projectSessionId !== request.projectSessionId ||
          credential.sessionGeneration !== request.expectedSessionGeneration
        ) {
          throw new ProjectFabricCoreError(
            "CAPABILITY_FORBIDDEN",
            "launch preparation requires the exact session-bound operator capability",
          );
        }
        if (this.#launchCustody === undefined) {
          throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "launch custody runtime is unavailable");
        }
        const replayed = this.#operatorActions.replayLaunchPreview(credential.context, request);
        if (replayed !== undefined) return replayed;
        const intent = await this.#launchCustody.prepareLaunchIntent(request);
        return await this.#operatorActions.preview(
          credential.context,
          {
            command: request.command,
            projectId: request.projectId,
            intent,
          },
          { allowLaunchIntent: true },
        );
      }
      case FABRIC_OPERATIONS.membershipBind: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.membershipBind];
        if (request.origin !== "operator") throw new FabricError("CAPABILITY_FORBIDDEN", "agent membership binding uses the chair dispatcher");
        const credential = operatorCredential();
        operatorCommand(credential, request.command);
        return this.#projectSessions.bindMembership(credential.context, request);
      }
      case FABRIC_OPERATIONS.operatorAttach: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.operatorAttach];
        const credential = operatorCredential();
        operatorCommand(credential, request.command);
        return this.#operatorStore.attach(credential.context, request, context.daemonInstanceGeneration);
      }
      case FABRIC_OPERATIONS.operatorDetach: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.operatorDetach];
        const credential = operatorCredential();
        operatorCommand(credential, request.command);
        return this.#operatorStore.detach(credential.context, request);
      }
      case FABRIC_OPERATIONS.operatorHeartbeat: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.operatorHeartbeat];
        const credential = operatorCredential();
        operatorCommand(credential, request.command);
        return this.#operatorStore.heartbeat(credential.context, request);
      }
      case FABRIC_OPERATIONS.intakeDraftCreate: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.intakeDraftCreate];
        const credential = operatorCredential();
        operatorCommand(credential, request.command);
        return this.#intakes.createDraft(credential.context, request);
      }
      case FABRIC_OPERATIONS.intakeRead: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.intakeRead];
        const credential = operatorCredential();
        if (
          request.credential.capabilityId !== credential.capabilityId ||
          sha256(request.credential.token) !== context.credentialHash
        ) throw new FabricError("AUTHENTICATION_FAILED", "intake read credential differs from its connection");
        const intake = this.#intakes.get(request.intakeId);
        if (intake.projectId !== credential.context.projectId) throw new ProjectFabricCoreError("WRONG_PROJECT", "intake is outside the operator project");
        return intake;
      }
      case FABRIC_OPERATIONS.intakeSubmit: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.intakeSubmit];
        const credential = operatorCredential();
        operatorCommand(credential, request.command);
        return this.#intakes.submit(credential.context, request);
      }
      case FABRIC_OPERATIONS.intakeRevise: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.intakeRevise];
        if (request.origin !== "operator") {
          throw new FabricError("CAPABILITY_FORBIDDEN", "agent intake revision uses the chair dispatcher");
        }
        const credential = operatorCredential();
        operatorCommand(credential, request.command);
        return this.#intakes.revise(credential.context, request);
      }
      case FABRIC_OPERATIONS.scopedGateCreate: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.scopedGateCreate];
        if (request.origin !== "operator") {
          throw new FabricError("CAPABILITY_FORBIDDEN", "operator gate creation requires operator origin");
        }
        const credential = operatorCredential();
        operatorCommand(credential, request.command);
        const existing = this.#gates.getGateByDedupe(
          credential.context,
          request.intent.projectSessionId,
          request.intent.coordinationRunId,
          request.intent.dedupeKey,
        );
        if (existing?.status === "superseded" && existing.resolution.kind === "system-supersession" &&
          !context.features.includes(GATE_SYSTEM_SUPERSESSION_FEATURE)) {
          throw new ProjectFabricCoreError("FEATURE_UNAVAILABLE", "gate system-supersession result shape was not negotiated");
        }
        const gate = this.#gates.createGate(credential.context, request);
        if (gate.status === "superseded" && gate.resolution.kind === "system-supersession" &&
          !context.features.includes(GATE_SYSTEM_SUPERSESSION_FEATURE)) {
          throw new ProjectFabricCoreError("FEATURE_UNAVAILABLE", "gate system-supersession result shape was not negotiated");
        }
        return gate;
      }
      case FABRIC_OPERATIONS.scopedGateResolve: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.scopedGateResolve];
        const credential = operatorCredential();
        operatorCommand(credential, request.command);
        return this.#gates.resolveGate(credential.context, request);
      }
      case FABRIC_OPERATIONS.scopedGateRead: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.scopedGateRead];
        const credential = operatorCredential();
        if (
          request.credential.capabilityId !== credential.capabilityId ||
          sha256(request.credential.token) !== context.credentialHash
        ) {
          throw new FabricError("AUTHENTICATION_FAILED", "gate read credential differs from its connection");
        }
        if (request.projectId !== credential.context.projectId) {
          throw new ProjectFabricCoreError("WRONG_PROJECT", "gate is outside the operator project");
        }
        const gate = this.#gates.getGate(request.gateId);
        if (gate.projectSessionId !== request.projectSessionId) {
          throw new ProjectFabricCoreError("WRONG_PROJECT", "gate is outside the requested session");
        }
        if (gate.status === "superseded" && gate.resolution.kind === "system-supersession" &&
          !context.features.includes(GATE_SYSTEM_SUPERSESSION_FEATURE)) {
          throw new ProjectFabricCoreError("FEATURE_UNAVAILABLE", "gate system-supersession result shape was not negotiated");
        }
        const stateDigest = sha256Digest(canonicalJson(gate)) as never;
        const readTransactionId = `read_${sha256(`${context.connectionNonce}\0${gate.gateId}\0${String(gate.revision)}`).slice(0, 24)}`;
        if (request.expectedRevision !== undefined && request.expectedRevision !== gate.revision) {
          return {
            status: "changed",
            expectedRevision: request.expectedRevision,
            gate,
            readTransactionId,
            stateDigest,
          };
        }
        return { status: "current", gate, readTransactionId, stateDigest };
      }
      case FABRIC_OPERATIONS.projectDiscover: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.projectDiscover];
        const credential = operatorCredential();
        operatorCommand(credential, { credential: request.credential });
        return this.#operatorProjections.discover(request);
      }
      case FABRIC_OPERATIONS.projectionSnapshot: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.projectionSnapshot];
        const credential = operatorCredential();
        operatorCommand(credential, { credential: request.credential });
        return this.#operatorProjections.snapshot(
          request,
          context.features.includes(NATIVE_NOTIFICATION_PROJECTION_FEATURE) ? "include" : "omit",
          context.features.includes(RUN_SESSION_PROJECTION_FEATURE) ? "include" : "omit",
        );
      }
      case FABRIC_OPERATIONS.projectionPage: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.projectionPage];
        const credential = operatorCredential();
        operatorCommand(credential, { credential: request.credential });
        return this.#operatorProjections.page(
          request,
          context.features.includes(NATIVE_NOTIFICATION_PROJECTION_FEATURE) ? "include" : "omit",
          context.features.includes(RUN_SESSION_PROJECTION_FEATURE) ? "include" : "omit", context.features.includes(ACTIVITY_NARRATIVE_GROUPING_FEATURE) ? "include" : "omit",
        );
      }
      case FABRIC_OPERATIONS.projectionEvents: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.projectionEvents];
        const credential = operatorCredential();
        operatorCommand(credential, { credential: request.credential });
        return this.#operatorProjections.events(request);
      }
      case FABRIC_OPERATIONS.projectionViewPage: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.projectionViewPage];
        const credential = operatorCredential();
        operatorCommand(credential, { credential: request.credential });
        return this.#operatorProjections.viewPage(
          request,
          context.features.includes(NATIVE_NOTIFICATION_PROJECTION_FEATURE) ? "include" : "omit",
          context.features.includes(RUN_SESSION_PROJECTION_FEATURE) ? "include" : "omit",
          context.features.includes(DECLARED_RUN_PROGRESS_FEATURE) ? "include" : "omit",
          context.features.includes(RUN_IDENTITY_PROJECTION_FEATURE) ? "include" : "omit", context.features.includes(AGENT_TOPOLOGY_PROJECTION_FEATURE) ? "include" : "omit", context.features.includes(WORK_FACTS_PROJECTION_FEATURE) ? "include" : "omit", context.features.includes(ACTIVITY_NARRATIVE_GROUPING_FEATURE) ? "include" : "omit",
        );
      }
      case FABRIC_OPERATIONS.projectionRunPage: operatorCommand(operatorCredential(), { credential: (input as OperationInputMap[typeof FABRIC_OPERATIONS.projectionRunPage]).credential }); return this.#operatorProjections.runPage(input as OperationInputMap[typeof FABRIC_OPERATIONS.projectionRunPage], context.features.includes(ACTIVITY_NARRATIVE_GROUPING_FEATURE) ? "include" : "omit", context.features.includes(RUN_LIFECYCLE_FACTS_FEATURE) ? "include" : "omit");
      case FABRIC_OPERATIONS.projectionDetailRead: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.projectionDetailRead];
        const credential = operatorCredential();
        operatorCommand(credential, { credential: request.credential });
        return this.#operatorProjections.detail(
          request,
          context.features.includes(RUN_SESSION_PROJECTION_FEATURE) ? "include" : "omit",
          context.features.includes(DECLARED_RUN_PROGRESS_FEATURE) ? "include" : "omit",
          context.features.includes(RUN_IDENTITY_PROJECTION_FEATURE) ? "include" : "omit", context.features.includes(AGENT_TOPOLOGY_PROJECTION_FEATURE) ? "include" : "omit", context.features.includes(WORK_FACTS_PROJECTION_FEATURE) ? "include" : "omit", context.features.includes(ACTIVITY_NARRATIVE_GROUPING_FEATURE) ? "include" : "omit",
        );
      }
      case FABRIC_OPERATIONS.messageBodyRead: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.messageBodyRead];
        const credential = operatorCredential();
        operatorCommand(credential, { credential: request.credential });
        return this.#operatorProjections.messageBody(request);
      }
      case FABRIC_OPERATIONS.operatorRepositoryRead: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.operatorRepositoryRead];
        const credential = operatorCredential();
        operatorCommand(credential, { credential: request.credential });
        return this.#gitRepositoryReads.read(request);
      }
      case FABRIC_OPERATIONS.operatorArtifactContentRead: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.operatorArtifactContentRead];
        const credential = operatorCredential();
        operatorCommand(credential, { credential: request.credential });
        return this.#artifactContentReads.read(request);
      }
      case FABRIC_OPERATIONS.operatorActionPreview: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.operatorActionPreview];
        if (request.intent.kind === "project-session-launch") {
          throw new FabricError(
            "CAPABILITY_FORBIDDEN",
            "project-session-launch previews are server-authored only; use projectSessionLaunchPrepare",
          );
        }
        const credential = operatorCredential();
        operatorCommand(credential, request.command);
        this.#assertChairLiveHandoffFeature(context, operation, request);
        return this.#operatorActions.preview(credential.context, request);
      }
      case FABRIC_OPERATIONS.operatorActionCommit: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.operatorActionCommit];
        const credential = operatorCredential();
        operatorCommand(credential, request.command);
        this.#assertChairLiveHandoffFeature(context, operation, request);
        return this.#operatorActions.commit(credential.context, request);
      }
      case FABRIC_OPERATIONS.operatorActionStatus: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.operatorActionStatus];
        const credential = operatorCredential();
        operatorCommand(credential, { credential: request.credential });
        this.#assertChairLiveHandoffFeature(context, operation, request);
        return this.#operatorActions.status(request);
      }
      case FABRIC_OPERATIONS.operatorActionReconcile: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.operatorActionReconcile];
        const credential = operatorCredential();
        operatorCommand(credential, request.command);
        this.#assertChairLiveHandoffFeature(context, operation, request);
        return this.#operatorActions.reconcile(credential.context, request);
      }
      case FABRIC_OPERATIONS.chairTakeover: {
        const request = input as OperationInputMap[typeof FABRIC_OPERATIONS.chairTakeover];
        const credential = operatorCredential();
        operatorCommand(credential, request.command);
        return this.#projectSessions.takeoverChair(credential.context, request);
      }
      default:
        throw Object.assign(new Error(`public protocol operation is not wired: ${operation}`), {
          code: "PROTOCOL_UNSUPPORTED",
        });
    }
  }

  #assertChairLiveHandoffFeature(
    context: PublicProtocolContext,
    operation: ProtocolOperation,
    input: unknown,
  ): void {
    if (!this.#operatorActionTargetsChairLiveHandoff(operation, input)) return;
    if (!context.features.includes("chair-live-handoff.v1")) {
      throw new ProjectFabricCoreError(
        "FEATURE_UNAVAILABLE",
        "chair live handoff requires chair-live-handoff.v1 negotiation",
      );
    }
  }

  #operatorActionTargetsChairLiveHandoff(operation: ProtocolOperation, input: unknown): boolean {
    if (!isRow(input)) return false;
    if (operation === FABRIC_OPERATIONS.operatorActionPreview) {
      return isRow(input.intent) && input.intent.kind === "chair-live-handoff";
    }
    let preview: unknown;
    if (operation === FABRIC_OPERATIONS.operatorActionCommit && typeof input.previewId === "string") {
      preview = this.#database.prepare("SELECT preview_json FROM operator_previews WHERE preview_id=?")
        .get(input.previewId);
    } else {
      const commandId = operation === FABRIC_OPERATIONS.operatorActionStatus
        ? input.commandId
        : operation === FABRIC_OPERATIONS.operatorActionReconcile
          ? input.targetCommandId
          : undefined;
      if (typeof commandId !== "string") return false;
      preview = this.#database.prepare(`
        SELECT preview_json FROM operator_previews WHERE confirmed_command_id=?
      `).get(commandId);
    }
    if (!isRow(preview) || typeof preview.preview_json !== "string") return false;
    try {
      const envelope: unknown = JSON.parse(preview.preview_json);
      return isRow(envelope) && isRow(envelope.preview) &&
        isRow(envelope.preview.intent) && envelope.preview.intent.kind === "chair-live-handoff";
    } catch {
      return false;
    }
  }

  #agentOperationGateTarget(
    runId: string,
    actorAgentId: string,
    operation: FabricOperation,
    input: unknown,
  ): GateOperationTarget {
    const request = rowOrNotFound(input, "gate operation input");
    const directTaskOperations = new Set<FabricOperation>([
      FABRIC_OPERATIONS.claimTask,
      FABRIC_OPERATIONS.refreshTaskReadiness,
      FABRIC_OPERATIONS.recordObjectiveCheck,
      FABRIC_OPERATIONS.acknowledgeTaskHandoff,
      FABRIC_OPERATIONS.getTask,
      FABRIC_OPERATIONS.updateTask,
      FABRIC_OPERATIONS.recordTaskOwnerRecoveryProof,
      FABRIC_OPERATIONS.recoverTaskOwner,
      FABRIC_OPERATIONS.requestLifecycle,
      FABRIC_OPERATIONS.taskCompleteWithReply,
    ]);
    if (directTaskOperations.has(operation)) {
      const taskId = request.taskId;
      if (typeof taskId !== "string") throw new ProjectFabricCoreError("PROTOCOL_INVALID", "task-owned operation lacks an exact task ID");
      return { kind: "task", taskId: taskId as never };
    }
    if (operation === FABRIC_OPERATIONS.sendMessage) {
      const audience = request.audience;
      if (isRow(audience) && audience.kind === "task") {
        if (typeof audience.taskId !== "string") throw new ProjectFabricCoreError("PROTOCOL_INVALID", "task audience lacks an exact task ID");
        return { kind: "task", taskId: audience.taskId as never };
      }
      return { kind: "run" };
    }
    if (
      operation === FABRIC_OPERATIONS.acquireWriteLease ||
      operation === FABRIC_OPERATIONS.publishArtifact ||
      operation === FABRIC_OPERATIONS.evidencePublish ||
      operation === FABRIC_OPERATIONS.resourceReserve
    ) {
      const requestedTaskId = request.taskId;
      if (requestedTaskId !== undefined && typeof requestedTaskId !== "string") {
        throw new ProjectFabricCoreError("PROTOCOL_INVALID", "optional task target is invalid");
      }
      const taskId = resolveTaskBindingForActiveWork(
        this.#database,
        runId,
        actorAgentId,
        requestedTaskId,
      );
      return taskId === undefined ? { kind: "run" } : { kind: "task", taskId: taskId as never };
    }
    if (operation === FABRIC_OPERATIONS.dispatchProviderAction) {
      const providerOperation = request.operation;
      if (providerOperation !== "send_turn" && providerOperation !== "steer") return { kind: "run" };
      const payload = rowOrNotFound(request.payload, "provider action payload");
      const requestedTaskId = payload.taskId;
      if (requestedTaskId !== undefined && typeof requestedTaskId !== "string") {
        throw new ProjectFabricCoreError("PROTOCOL_INVALID", "provider task target is invalid");
      }
      const targetAgentId = typeof payload.agentId === "string" ? payload.agentId : actorAgentId;
      const taskId = resolveTaskBindingForActiveWork(
        this.#database,
        runId,
        targetAgentId,
        requestedTaskId,
      );
      if (taskId === undefined) throw new ProjectFabricCoreError("PROTOCOL_INVALID", "task-owned provider action lacks an exact task target");
      return { kind: "task", taskId: taskId as never };
    }
    if (
      operation === FABRIC_OPERATIONS.recoverWriteLease ||
      operation === FABRIC_OPERATIONS.renewWriteLease ||
      operation === FABRIC_OPERATIONS.getWriteLease ||
      operation === FABRIC_OPERATIONS.releaseWriteLease
    ) {
      const leaseId = request.leaseId;
      if (typeof leaseId !== "string") throw new ProjectFabricCoreError("PROTOCOL_INVALID", "write-lease operation lacks a lease ID");
      const binding = this.#database.prepare(`
        SELECT task_id FROM task_obligation_bindings
         WHERE coordination_run_id=? AND obligation_kind='write-lease' AND obligation_id=?
      `).get(runId, leaseId);
      return isRow(binding) && typeof binding.task_id === "string"
        ? { kind: "task", taskId: binding.task_id as never }
        : { kind: "run" };
    }
    if (
      operation === FABRIC_OPERATIONS.resultDeliveryClaim ||
      operation === FABRIC_OPERATIONS.resultDeliveryConsume ||
      operation === FABRIC_OPERATIONS.resultDeliveryRetry ||
      operation === FABRIC_OPERATIONS.resultDeliveryReassign ||
      operation === FABRIC_OPERATIONS.resultDeliveryAbandon
    ) {
      const resultDeliveryId = request.resultDeliveryId;
      if (typeof resultDeliveryId !== "string") throw new ProjectFabricCoreError("PROTOCOL_INVALID", "result operation lacks a delivery ID");
      const delivery = rowOrNotFound(this.#database.prepare(`
        SELECT task_id FROM result_deliveries WHERE run_id=? AND result_delivery_id=?
      `).get(runId, resultDeliveryId), "result delivery gate target");
      return { kind: "task", taskId: stringField(delivery, "task_id") as never };
    }
    if (operation === FABRIC_OPERATIONS.resourceRelease || operation === FABRIC_OPERATIONS.resourceReconcile) {
      const reservationId = request.reservationId;
      if (typeof reservationId !== "string") throw new ProjectFabricCoreError("PROTOCOL_INVALID", "resource operation lacks a reservation ID");
      const reservation = rowOrNotFound(this.#database.prepare(`
        SELECT operation_id FROM resource_reservations
         WHERE coordination_run_id=? AND reservation_id=?
      `).get(runId, reservationId), "resource reservation gate target");
      const taskId = reservation.operation_id;
      return typeof taskId === "string" && this.#database.prepare(`
        SELECT 1 FROM tasks WHERE run_id=? AND task_id=?
      `).get(runId, taskId) !== undefined
        ? { kind: "task", taskId: taskId as never }
        : { kind: "run" };
    }
    return { kind: "run" };
  }

  assertCapability(runId: string, agentId: string, tokenHash: string, requiredOperation: FabricOperation, allowSuspended = false): void {
    const row = this.#database
      .prepare(`
      SELECT c.expires_at,c.revoked_at,a.authority_json,a.authority_hash,g.lifecycle,
             member.generation AS mcp_seat_generation,
             active.generation AS active_mcp_seat_generation
          FROM capabilities c
          JOIN agents g ON g.run_id=c.run_id AND g.agent_id=c.agent_id
          JOIN authorities a ON a.authority_id=g.authority_id
          LEFT JOIN mcp_seat_generation_members member ON member.token_hash=c.token_hash
          LEFT JOIN current_mcp_seat_generation_members active ON active.token_hash=c.token_hash
         WHERE c.token_hash=? AND c.run_id=? AND c.agent_id=?
      `)
      .get(tokenHash, runId, agentId);
    if (
      !isRow(row) ||
      row.revoked_at !== null ||
      numberField(row, "expires_at") <= this.#clock()
    ) {
      throw new FabricError("AUTHENTICATION_FAILED", "capability is expired, revoked or unknown");
    }
    assertActiveMcpSeatGeneration(row, this.#database, tokenHash);
    const authority = parseAuthority(row);
    if (authority.deniedActions.includes(requiredOperation) || !authority.actions.includes(requiredOperation)) {
      throw new FabricError("CAPABILITY_FORBIDDEN", `authority does not permit ${requiredOperation}`);
    }
    if (stringField(row, "lifecycle") === "archived") {
      throw new FabricError("AUTHENTICATION_FAILED", "archived agent capability is no longer active");
    }
    const lifecycle = stringField(row, "lifecycle");
    const lifecycleRecovery = requiredOperation === FABRIC_OPERATIONS.requestLifecycle;
    if (
      !allowSuspended &&
      (lifecycle === "suspended" || lifecycle === "context-unreconciled") &&
      !isReadFabricOperation(requiredOperation) &&
      !lifecycleRecovery
    ) {
      throw new FabricError("CONTEXT_UNRECONCILED", "unreconciled agent may only read until explicit lifecycle recovery");
    }
  }

  #assertChair(runId: string, actorAgentId: string): void {
    const run = rowOrNotFound(
      this.#database.prepare("SELECT chair_agent_id FROM runs WHERE run_id = ?").get(runId),
      "run",
    );
    if (stringField(run, "chair_agent_id") !== actorAgentId) {
      throw new FabricError("CAPABILITY_FORBIDDEN", "operation requires the run chair");
    }
  }

  assertTaskReadable(runId: string, actorAgentId: string, taskId: string): void {
    if (!this.#readPolicy.canReadTask(runId, actorAgentId, taskId)) throw new FabricError("CAPABILITY_FORBIDDEN", "task is outside the caller read scope");
  }

  assertAgentReadable(runId: string, actorAgentId: string, targetAgentId: string): void {
    if (!this.#readPolicy.canReadAgent(runId, actorAgentId, targetAgentId)) throw new FabricError("CAPABILITY_FORBIDDEN", "agent is outside the caller read scope");
  }

  assertWriteLeaseReadable(runId: string, actorAgentId: string, leaseId: string): void {
    if (!this.#readPolicy.canReadWriteLease(runId, actorAgentId, leaseId)) throw new FabricError("CAPABILITY_FORBIDDEN", "write lease is outside the caller read scope");
  }

  assertProviderActionReadable(runId: string, actorAgentId: string): void {
    if (!this.#readPolicy.isChair(runId, actorAgentId)) throw new FabricError("CAPABILITY_FORBIDDEN", "provider action reads are chair-only");
  }

  assertTeamReadable(runId: string, actorAgentId: string, teamId: string): void {
    if (!this.#readPolicy.canReadTeam(runId, actorAgentId, teamId)) throw new FabricError("CAPABILITY_FORBIDDEN", "team is outside the caller read scope");
  }

  assertBudgetReadable(runId: string, actorAgentId: string, budgetId: string): void {
    if (!this.#readPolicy.canReadBudget(runId, actorAgentId, budgetId)) throw new FabricError("CAPABILITY_FORBIDDEN", "budget is outside the caller read scope");
  }

  #event(runId: string, type: string, actorAgentId: string | null, payload: unknown): void {
    this.#database.transaction(() => {
      const eventId = uuidv7();
      this.#database
        .prepare("INSERT INTO events(event_id, run_id, type, actor_agent_id, payload_json, created_at) VALUES (?, ?, ?, ?, ?, ?)")
        .run(eventId, runId, type, actorAgentId, canonicalJson(payload), this.#clock());
      this.#database.prepare("INSERT INTO observer_event_sequence(event_id) VALUES (?)").run(eventId);
    })();
  }

  delegateAuthority(
    runId: string,
    actorAgentId: string,
    input: { parentAuthorityId: string; authority: AuthorityInput; commandId?: string },
  ): AuthorityResult {
    const commandId = input.commandId ?? `authority:${uuidv7()}`;
    return this.#commandJournal.execute(runId, actorAgentId, commandId, input, isAuthorityResult, () => {
      assertRunAcceptingWork(this.#database, runId);
      const parentRow = rowOrNotFound(
        this.#database
          .prepare("SELECT authority_json, authority_hash FROM authorities WHERE authority_id = ? AND run_id = ?")
          .get(input.parentAuthorityId, runId),
        "parent authority",
      );
      const actorRow = rowOrNotFound(
        this.#database.prepare("SELECT authority_id FROM agents WHERE run_id = ? AND agent_id = ?").get(runId, actorAgentId),
        "agent",
      );
      if (stringField(actorRow, "authority_id") !== input.parentAuthorityId) {
        throw new FabricError("CAPABILITY_FORBIDDEN", "actor does not hold the parent authority");
      }
      const parent = parseAuthority(parentRow);
      const child = normaliseAuthority(input.authority, this.#workspaceRootForRun(runId));
      if (!authorityContained(child, parent)) {
        throw new FabricError("AUTHORITY_WIDENING", "child authority exceeds its parent");
      }
      for (const [unitKey, requested] of Object.entries(child.budget)) {
        const row = rowOrNotFound(
          this.#database
            .prepare("SELECT granted, reserved, consumed, usage_unknown FROM authority_budget WHERE authority_id = ? AND unit_key = ?")
            .get(input.parentAuthorityId, unitKey),
          `budget ${unitKey}`,
        );
        if (
          numberField(row, "usage_unknown") !== 0 ||
          numberField(row, "granted") - numberField(row, "reserved") - numberField(row, "consumed") < requested
        ) {
          throw new FabricError("BUDGET_EXCEEDED", `insufficient available budget for ${unitKey}`);
        }
      }
      const authorityId = uuidv7();
      const now = this.#clock();
      this.#database
        .prepare(
          "INSERT INTO authorities(authority_id, run_id, parent_authority_id, authority_json, authority_hash, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .run(authorityId, runId, input.parentAuthorityId, canonicalJson(child), sha256(canonicalJson(child)), now);
      for (const [unitKey, granted] of Object.entries(child.budget)) {
        this.#database
          .prepare("UPDATE authority_budget SET reserved = reserved + ? WHERE authority_id = ? AND unit_key = ?")
          .run(granted, input.parentAuthorityId, unitKey);
        this.#database
          .prepare("INSERT INTO authority_budget(authority_id, unit_key, granted) VALUES (?, ?, ?)")
          .run(authorityId, unitKey, granted);
      }
      this.#event(runId, "authority-delegated", actorAgentId, { authorityId, parentAuthorityId: input.parentAuthorityId });
      return { authorityId };
    });
  }

  registerAgent(
    runId: string,
    actorAgentId: string,
    input: { agentId: string; authorityId: string; providerSessionRef?: string; adapterId?: string },
  ): { capability: string } {
    assertRunAcceptingWork(this.#database, runId);
    const authorityRow = rowOrNotFound(
      this.#database
        .prepare("SELECT authority_json, authority_hash, parent_authority_id FROM authorities WHERE authority_id = ? AND run_id = ?")
        .get(input.authorityId, runId),
      "authority",
    );
    const actorRow = rowOrNotFound(
      this.#database.prepare("SELECT authority_id FROM agents WHERE run_id = ? AND agent_id = ?").get(runId, actorAgentId),
      "agent",
    );
    if (authorityRow.parent_authority_id !== stringField(actorRow, "authority_id")) {
      throw new FabricError("CAPABILITY_FORBIDDEN", "actor cannot register an agent for this authority");
    }
    const authority = parseAuthority(authorityRow);
    const existing = this.#database.prepare(`
      SELECT g.parent_agent_id, g.authority_id, g.provider_session_ref, c.principal_generation, c.revoked_at, b.adapter_id
        FROM agents g LEFT JOIN capabilities c ON c.run_id = g.run_id AND c.agent_id = g.agent_id
        LEFT JOIN agent_adapter_bindings b ON b.run_id = g.run_id AND b.agent_id = g.agent_id
       WHERE g.run_id = ? AND g.agent_id = ? ORDER BY c.principal_generation DESC LIMIT 1
    `).get(runId, input.agentId);
    if (isRow(existing)) {
      const same = existing.parent_agent_id === actorAgentId && existing.authority_id === input.authorityId && existing.provider_session_ref === (input.providerSessionRef ?? null) && existing.adapter_id === (input.adapterId ?? null);
      if (!same) throw new FabricError("DEDUPE_CONFLICT", "agent ID was reused with changed registration input");
      if (existing.principal_generation === null) {
        const token = capabilityToken(this.#capabilityKey, runId, input.agentId, 1);
        this.#database.prepare(
          "INSERT INTO capabilities(token_hash, run_id, agent_id, principal_generation, expires_at) VALUES (?, ?, ?, 1, ?)",
        ).run(sha256(token), runId, input.agentId, Date.parse(authority.expiresAt));
        this.#event(runId, "agent-capability-issued", actorAgentId, { agentId: input.agentId, principalGeneration: 1 });
        return { capability: token };
      }
      if (existing.revoked_at !== null) throw new FabricError("AUTHENTICATION_FAILED", "agent capability was revoked");
      return { capability: capabilityToken(this.#capabilityKey, runId, input.agentId, numberField(existing, "principal_generation")) };
    }
    const token = capabilityToken(this.#capabilityKey, runId, input.agentId, 1);
    this.#database.transaction(() => {
      this.#database
        .prepare(
          "INSERT INTO agents(run_id, agent_id, parent_agent_id, authority_id, provider_session_ref, classification) VALUES (?, ?, ?, ?, ?, 'worker')",
        )
        .run(runId, input.agentId, actorAgentId, input.authorityId, input.providerSessionRef ?? null);
      this.#database.prepare("INSERT INTO mailbox_state(run_id, recipient_id) VALUES (?, ?)").run(runId, input.agentId);
      this.#database
        .prepare("INSERT INTO capabilities(token_hash, run_id, agent_id, expires_at) VALUES (?, ?, ?, ?)")
        .run(sha256(token), runId, input.agentId, Date.parse(authority.expiresAt));
      if (input.adapterId !== undefined) {
        if (input.providerSessionRef === undefined) {
          throw new FabricError("CAPABILITY_FORBIDDEN", "adapter binding requires a provider session reference");
        }
        this.#database
          .prepare("INSERT INTO agent_adapter_bindings(run_id, agent_id, adapter_id, bound_at) VALUES (?, ?, ?, ?)")
          .run(runId, input.agentId, input.adapterId, this.#clock());
      }
      this.#event(runId, "agent-registered", actorAgentId, { agentId: input.agentId });
    })();
    return { capability: token };
  }

  #registerAgentIdentity(
    runId: string,
    actorAgentId: string,
    input: { agentId: string; authorityId: string },
  ): void {
    assertRunAcceptingWork(this.#database, runId);
    const authority = rowOrNotFound(
      this.#database.prepare(
        "SELECT parent_authority_id FROM authorities WHERE authority_id = ? AND run_id = ?",
      ).get(input.authorityId, runId),
      "authority",
    );
    const actor = rowOrNotFound(
      this.#database.prepare("SELECT authority_id FROM agents WHERE run_id = ? AND agent_id = ?")
        .get(runId, actorAgentId),
      "agent",
    );
    if (authority.parent_authority_id !== stringField(actor, "authority_id")) {
      throw new FabricError("CAPABILITY_FORBIDDEN", "actor cannot register an identity for this authority");
    }
    const existing = this.#database.prepare(`
      SELECT parent_agent_id, authority_id, provider_session_ref
        FROM agents WHERE run_id=? AND agent_id=?
    `).get(runId, input.agentId);
    if (isRow(existing)) {
      if (
        existing.parent_agent_id !== actorAgentId ||
        existing.authority_id !== input.authorityId ||
        existing.provider_session_ref !== null
      ) throw new FabricError("DEDUPE_CONFLICT", "agent identity was reused with changed registration input");
      return;
    }
    this.#database.prepare(
      "INSERT INTO agents(run_id, agent_id, parent_agent_id, authority_id, provider_session_ref, classification) VALUES (?, ?, ?, ?, NULL, 'worker')",
    ).run(runId, input.agentId, actorAgentId, input.authorityId);
    this.#database.prepare("INSERT INTO mailbox_state(run_id, recipient_id) VALUES (?, ?)")
      .run(runId, input.agentId);
    this.#event(runId, "agent-identity-registered", actorAgentId, { agentId: input.agentId });
  }

  async spawnAgent(
    runId: string,
    actorAgentId: string,
    input: {
      agentId: string;
      authorityId: string;
      adapterId: string;
      actionId: string;
      payload: Record<string, unknown>;
    },
  ): Promise<AgentCustodyResult> {
    assertRunAcceptingWork(this.#database, runId);
    this.#adapter(input.adapterId);
    this.#providerSessions.preflightRegistration({
      runId,
      actorAgentId,
      agentId: input.agentId,
      authorityId: input.authorityId,
      adapterId: input.adapterId,
    });
    const providerPayload = this.#payloadAuthority.admitProviderPayload(
      runId,
      input.authorityId,
      input.payload,
      true,
      { actorAgentId, taskId: typeof input.payload.taskId === "string" ? input.payload.taskId : undefined },
    );
    this.#payloadAuthority.assertAdapterModel(input.adapterId, providerPayload);
    const bridgeContract = await this.#inspectAgentBridgeContract(input.adapterId, "spawn");
    if (this.#launchCustody === undefined) {
      throw new FabricError("CAPABILITY_UNAVAILABLE", "agent custody requires an elected daemon socket");
    }
    return await this.#launchCustody.provisionAgent({
      runId,
      actorAgentId,
      operation: "spawn",
      agentId: input.agentId,
      authorityId: input.authorityId,
      adapterId: input.adapterId,
      actionId: input.actionId,
      payload: { ...providerPayload, agentId: input.agentId },
      ...(bridgeContract === undefined ? {} : { bridgeContract: bridgeContract as AgentBridgeContract }),
    });
  }

  async attachAgent(
    runId: string,
    actorAgentId: string,
    input: {
      agentId: string;
      authorityId: string;
      adapterId: string;
      actionId: string;
      providerSessionRef: string;
    },
  ): Promise<AgentCustodyResult> {
    assertRunAcceptingWork(this.#database, runId);
    this.#adapter(input.adapterId);
    this.#providerSessions.preflightRegistration({
      runId,
      actorAgentId,
      agentId: input.agentId,
      authorityId: input.authorityId,
      adapterId: input.adapterId,
      providerSessionRef: input.providerSessionRef,
    });
    const providerPayload = this.#payloadAuthority.admitProviderPayload(runId, input.authorityId, {});
    const bridgeContract = await this.#inspectAgentBridgeContract(input.adapterId, "attach");
    if (this.#launchCustody === undefined) {
      throw new FabricError("CAPABILITY_UNAVAILABLE", "agent custody requires an elected daemon socket");
    }
    return await this.#launchCustody.provisionAgent({
      runId,
      actorAgentId,
      operation: "attach",
      agentId: input.agentId,
      authorityId: input.authorityId,
      adapterId: input.adapterId,
      actionId: input.actionId,
      payload: { ...providerPayload, agentId: input.agentId },
      providerSessionRef: input.providerSessionRef,
      ...(bridgeContract === undefined ? {} : { bridgeContract: bridgeContract as AgentBridgeContract }),
    });
  }

  sendMessage(runId: string, senderId: string, input: MessageInput): { messageId: string } {
    return this.#mailboxCustody.sendMessage(runId, senderId, input);
  }

  createDiscussionGroup(
    runId: string,
    actorAgentId: string,
    input: { groupId: string; memberAgentIds: string[]; teamId?: string; commandId: string },
  ): { groupId: string; memberAgentIds: string[] } {
    return this.#mailboxCustody.createDiscussionGroup(runId, actorAgentId, input);
  }

  receiveMessages(
    runId: string,
    recipientId: string,
    input: { limit: number; visibilityTimeoutMs: number },
  ): Array<{
    deliveryId: string;
    messageId: string;
    sequence: number;
    body: string;
    attempt: number;
    senderId: string;
    kind: MessageInput["kind"];
    requiresAck: boolean;
  }> {
    return this.#mailboxCustody.receiveMessages(runId, recipientId, input);
  }

  acknowledgeDelivery(runId: string, recipientId: string, deliveryId: string): void {
    this.#mailboxCustody.acknowledgeDelivery(runId, recipientId, deliveryId);
  }

  abandonDelivery(
    runId: string,
    actorAgentId: string,
    input: { deliveryId: string; reason: string; commandId: string },
  ): { deliveryId: string; status: "abandoned"; reason: string } {
    return this.#mailboxCustody.abandonDelivery(runId, actorAgentId, input);
  }

  getMailboxState(runId: string, recipientId: string): {
    contiguousWatermark: number;
    acknowledgedAboveWatermark: number[];
  } {
    return this.#mailboxCustody.getMailboxState(runId, recipientId);
  }

  createTask(runId: string, actorAgentId: string, input: TaskCreateInput): TaskResult {
    return this.#createTask(runId, actorAgentId, input, true);
  }

  #createTask(
    runId: string,
    actorAgentId: string,
    input: TaskCreateInput,
    bindToLedTeam: boolean,
  ): TaskResult {
    const journalPayload = {
      ...input,
      teamOwnershipBinding: bindToLedTeam ? "active-led-team" : "atomic-team-root",
    } as const;
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, journalPayload, isTaskResult, () => {
      assertRunAcceptingWork(this.#database, runId);
      const agentContext = this.#agentContext(runId, actorAgentId);
      const dependencyRevision = numberField(
        rowOrNotFound(
          this.#database.prepare("SELECT dependency_revision FROM runs WHERE run_id=?").get(runId),
          "coordination run",
        ),
        "dependency_revision",
      );
      const actor = rowOrNotFound(
        this.#database.prepare("SELECT authority_id FROM agents WHERE run_id = ? AND agent_id = ?").get(runId, actorAgentId),
        "agent",
      );
      const authority = rowOrNotFound(
        this.#database
          .prepare("SELECT parent_authority_id FROM authorities WHERE run_id = ? AND authority_id = ?")
          .get(runId, input.authorityId),
        "task authority",
      );
      const actorAuthorityId = stringField(actor, "authority_id");
      if (input.authorityId !== actorAuthorityId && authority.parent_authority_id !== actorAuthorityId) {
        throw new FabricError("CAPABILITY_FORBIDDEN", "actor cannot assign this task authority");
      }
      const eligibleAgentIds = [...new Set(input.eligibleAgentIds)].sort();
      if (eligibleAgentIds.length === 0) {
        throw new FabricError("NOT_FOUND", "task has no eligible agents");
      }
      const participantAgentIds = [...new Set(input.participantAgentIds ?? [actorAgentId, ...eligibleAgentIds])].sort();
      if (participantAgentIds.length > 256) throw new FabricError("CAPABILITY_FORBIDDEN", "task participants exceed the protocol limit; provide at most 256 participantAgentIds");
      for (const agentId of eligibleAgentIds) {
        rowOrNotFound(
          this.#database.prepare("SELECT 1 FROM agents WHERE run_id = ? AND agent_id = ?").get(runId, agentId),
          `eligible agent ${agentId}`,
        );
      }
      const proposedOwnerAgentId = input.proposedOwnerAgentId ?? (eligibleAgentIds.length === 1 ? eligibleAgentIds[0] ?? null : null);
      if (proposedOwnerAgentId !== null && !eligibleAgentIds.includes(proposedOwnerAgentId)) {
        throw new FabricError("CAPABILITY_FORBIDDEN", "proposed owner is not eligible for the task");
      }
      for (const agentId of participantAgentIds) {
        rowOrNotFound(
          this.#database.prepare("SELECT 1 FROM agents WHERE run_id = ? AND agent_id = ?").get(runId, agentId),
          `task participant ${agentId}`,
        );
      }
      const dependencies = [...new Set(input.dependencies ?? [])].sort();
      let blocked = false;
      for (const dependencyTaskId of dependencies) {
        const dependency = rowOrNotFound(
          this.#database.prepare("SELECT state FROM tasks WHERE run_id = ? AND task_id = ?").get(runId, dependencyTaskId),
          `dependency ${dependencyTaskId}`,
        );
        if (!["complete", "cancelled", "degraded"].includes(stringField(dependency, "state"))) blocked = true;
      }
      this.#database
        .prepare(
          "INSERT INTO tasks(run_id, task_id, authority_id, objective, base_revision, state, owner_agent_id, revision, owner_lease_generation, created_by) VALUES (?, ?, ?, ?, ?, ?, NULL, 1, 0, ?)",
        )
        .run(runId, input.taskId, input.authorityId, input.objective, input.baseRevision, blocked ? "blocked" : "ready", actorAgentId);
      for (const agentId of eligibleAgentIds) {
        this.#database
          .prepare("INSERT INTO task_eligible_agents(run_id, task_id, agent_id) VALUES (?, ?, ?)")
          .run(runId, input.taskId, agentId);
      }
      this.#database
        .prepare("INSERT INTO task_proposals(run_id, task_id, proposed_owner_agent_id) VALUES (?, ?, ?)")
        .run(runId, input.taskId, proposedOwnerAgentId);
      for (const agentId of participantAgentIds) {
        this.#database.prepare("INSERT INTO task_participants(run_id, task_id, agent_id) VALUES (?, ?, ?)").run(
          runId,
          input.taskId,
          agentId,
        );
      }
      for (const relativePath of [...new Set(input.expectedArtifacts ?? [])].sort()) {
        if (relativePath.length === 0 || isAbsolute(relativePath) || relativePath.split(/[\\/]/u).includes("..")) {
          throw new FabricError("ARTIFACT_PATH_FORBIDDEN", "expected artifact path must be relative and traversal-free");
        }
        this.#database
          .prepare("INSERT INTO task_expected_artifacts(run_id, task_id, relative_path) VALUES (?, ?, ?)")
          .run(runId, input.taskId, relativePath);
      }
      for (const checkId of [...new Set(input.objectiveChecks ?? [])].sort()) {
        this.#database
          .prepare("INSERT INTO task_objective_checks(run_id, task_id, check_id) VALUES (?, ?, ?)")
          .run(runId, input.taskId, checkId);
      }
      if (dependencies.length > 0) {
        this.#gates.setTaskDependencies(agentContext, {
            commandId: `${input.commandId}:dependencies`,
            expectedRevision: dependencyRevision,
            taskId: input.taskId,
            dependencyTaskIds: dependencies,
          });
      }
      if (bindToLedTeam) {
        const ledTeam = this.#database.prepare(`
          SELECT team_id FROM teams
           WHERE run_id=? AND leader_agent_id=? AND state='active'
           ORDER BY depth DESC, team_id
        `).all(runId, actorAgentId) as Array<{ team_id: string }>;
        if (ledTeam.length > 1) {
          throw new FabricError("TASK_SUBTREE_CONFLICT", "task creator leads more than one active team");
        }
        const ownedTeam = ledTeam[0];
        if (ownedTeam !== undefined) {
          this.#database.prepare(
            "INSERT INTO team_owned_tasks(run_id, team_id, task_id) VALUES (?, ?, ?)",
          ).run(runId, ownedTeam.team_id, input.taskId);
        }
      }
      this.#memberships.bindRequired(runId, [{ kind: "task", memberId: input.taskId }]);
      const result = this.getTask(runId, input.taskId);
      this.#event(runId, "task-created", actorAgentId, result);
      return result;
    });
  }

  claimTask(
    runId: string,
    actorAgentId: string,
    input: { taskId: string; expectedRevision: number; commandId: string },
  ): TaskResult {
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isTaskResult, () => {
      assertRunAcceptingWork(this.#database, runId);
      const eligible = this.#database
        .prepare("SELECT 1 FROM task_eligible_agents WHERE run_id = ? AND task_id = ? AND agent_id = ?")
        .get(runId, input.taskId, actorAgentId);
      if (!isRow(eligible)) {
        throw new FabricError("CAPABILITY_FORBIDDEN", "agent is not eligible to claim the task");
      }
      assertScopedTaskReadinessAllowed(this.#database, runId, input.taskId);
      assertScopedOperationAllowed(
        this.#database,
        runId,
        FABRIC_OPERATIONS.claimTask,
        { kind: "task", taskId: input.taskId as never },
      );
      assertTaskOperationAdmitted(this.#database, runId, input.taskId);
      const task = rowOrNotFound(
        this.#database
          .prepare("SELECT state, revision FROM tasks WHERE run_id = ? AND task_id = ?")
          .get(runId, input.taskId),
        "task",
      );
      if (stringField(task, "state") === "blocked") {
        throw new FabricError("TASK_DEPENDENCY_BLOCKED", "task dependencies are not terminal");
      }
      if (numberField(task, "revision") !== input.expectedRevision || stringField(task, "state") !== "ready") {
        throw new FabricError("TASK_REVISION_CONFLICT", "task revision or state changed");
      }
      const updated = this.#database
        .prepare(
          "UPDATE tasks SET owner_agent_id = ?, state = 'active', revision = revision + 1, owner_lease_generation = owner_lease_generation + 1 WHERE run_id = ? AND task_id = ? AND revision = ? AND state = 'ready'",
        )
        .run(actorAgentId, runId, input.taskId, input.expectedRevision);
      if (updated.changes !== 1) {
        throw new FabricError("TASK_REVISION_CONFLICT", "task was claimed concurrently");
      }
      const result = this.getTask(runId, input.taskId);
      this.#event(runId, "task-claimed", actorAgentId, result);
      return result;
    });
  }

  getTask(runId: string, taskId: string): TaskResult {
    const task = rowOrNotFound(
      this.#database
        .prepare("SELECT task_id, owner_agent_id, state, revision, owner_lease_generation FROM tasks WHERE run_id = ? AND task_id = ?")
        .get(runId, taskId),
      "task",
    );
    const proposal = this.#database
      .prepare("SELECT proposed_owner_agent_id FROM task_proposals WHERE run_id = ? AND task_id = ?")
      .get(runId, taskId);
    const proposedValue = isRow(proposal) ? proposal.proposed_owner_agent_id : null;
    if (typeof proposedValue !== "string" && proposedValue !== null) throw new Error("stored task proposal is invalid");
    const dependencies = this.#database
      .prepare("SELECT dependency_task_id FROM task_dependencies WHERE run_id = ? AND task_id = ? ORDER BY dependency_task_id")
      .all(runId, taskId)
      .map((value) => stringField(rowOrNotFound(value, "task dependency"), "dependency_task_id"));
    return taskResultFromRow(task, proposedValue, dependencies);
  }

  refreshTaskReadiness(
    runId: string,
    actorAgentId: string,
    input: { taskId: string; expectedRevision: number; commandId: string },
  ): TaskResult {
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isTaskResult, () => {
      assertScopedTaskReadinessAllowed(this.#database, runId, input.taskId);
      assertScopedOperationAllowed(
        this.#database,
        runId,
        FABRIC_OPERATIONS.refreshTaskReadiness,
        { kind: "task", taskId: input.taskId as never },
      );
      assertTaskOperationAdmitted(this.#database, runId, input.taskId);
      const task = this.getTask(runId, input.taskId);
      if (task.revision !== input.expectedRevision) {
        throw new FabricError("TASK_REVISION_CONFLICT", "task revision changed");
      }
      if (task.state !== "blocked") return task;
      const unresolved = this.#database
        .prepare(
          "SELECT COUNT(*) AS count FROM task_dependencies d JOIN tasks t ON t.run_id = d.run_id AND t.task_id = d.dependency_task_id WHERE d.run_id = ? AND d.task_id = ? AND t.state NOT IN ('complete', 'cancelled', 'degraded')",
        )
        .get(runId, input.taskId);
      if (numberField(rowOrNotFound(unresolved, "dependency count"), "count") > 0) {
        throw new FabricError("TASK_DEPENDENCY_BLOCKED", "task dependencies are not terminal");
      }
      this.#database
        .prepare("UPDATE tasks SET state = 'ready', revision = revision + 1 WHERE run_id = ? AND task_id = ? AND revision = ?")
        .run(runId, input.taskId, input.expectedRevision);
      const result = this.getTask(runId, input.taskId);
      this.#event(runId, "task-readiness-refreshed", actorAgentId, result);
      return result;
    });
  }

  recordObjectiveCheck(
    runId: string,
    actorAgentId: string,
    input: { taskId: string; checkId: string; status: "pass" | "fail"; evidence: string; commandId: string },
  ): { taskId: string; checkId: string; status: "pass" | "fail" } {
    const parse = (value: unknown): value is { taskId: string; checkId: string; status: "pass" | "fail" } =>
      isRow(value) && typeof value.taskId === "string" && typeof value.checkId === "string" && (value.status === "pass" || value.status === "fail");
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, parse, () => {
      assertTaskOperationAdmitted(this.#database, runId, input.taskId);
      const changed = this.#database
        .prepare("UPDATE task_objective_checks SET status = ?, evidence = ? WHERE run_id = ? AND task_id = ? AND check_id = ?")
        .run(input.status, input.evidence, runId, input.taskId, input.checkId);
      if (changed.changes !== 1) throw new FabricError("NOT_FOUND", "objective check is not declared for the task");
      return { taskId: input.taskId, checkId: input.checkId, status: input.status };
    });
  }

  acknowledgeTaskHandoff(
    runId: string,
    actorAgentId: string,
    input: { taskId: string; taskRevision: number; ownerLeaseGeneration: number; commandId: string },
  ): { acknowledged: true } {
    const parse = (value: unknown): value is { acknowledged: true } => isRow(value) && value.acknowledged === true;
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, parse, () => {
      assertTaskOperationAdmitted(this.#database, runId, input.taskId);
      const task = this.getTask(runId, input.taskId);
      if (task.revision !== input.taskRevision || task.ownerLeaseGeneration !== input.ownerLeaseGeneration) {
        throw new FabricError("TASK_REVISION_CONFLICT", "handoff revision or generation changed");
      }
      const intendedNextOwner = this.#intendedTaskHandoffOwner(runId, input.taskId, task.ownerAgentId);
      if (intendedNextOwner === null || actorAgentId !== intendedNextOwner) {
        throw new FabricError("CAPABILITY_FORBIDDEN", "only the intended next owner may acknowledge the task handoff");
      }
      this.#database
        .prepare("INSERT INTO task_handoff_acknowledgements(run_id, task_id, task_revision, owner_lease_generation, intended_next_owner_agent_id, acknowledged_by, acknowledged_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
        .run(runId, input.taskId, input.taskRevision, input.ownerLeaseGeneration, intendedNextOwner, actorAgentId, this.#clock());
      return { acknowledged: true };
    });
  }

  #intendedTaskHandoffOwner(runId: string, taskId: string, currentOwnerAgentId: string | null): string | null {
    const task = rowOrNotFound(
      this.#database.prepare("SELECT created_by FROM tasks WHERE run_id = ? AND task_id = ?").get(runId, taskId),
      "handoff task",
    );
    const creator = stringField(task, "created_by");
    if (creator !== currentOwnerAgentId) return creator;
    const participants = this.#database
      .prepare("SELECT agent_id FROM task_participants WHERE run_id = ? AND task_id = ? AND agent_id != ? ORDER BY agent_id")
      .all(runId, taskId, currentOwnerAgentId ?? "")
      .map((value) => stringField(rowOrNotFound(value, "handoff participant"), "agent_id"));
    return participants.length === 1 ? participants[0] ?? null : null;
  }

  updateTask(
    runId: string,
    actorAgentId: string,
    input: {
      taskId: string;
      expectedRevision: number;
      state: "complete" | "cancelled" | "degraded";
      commandId: string;
    },
  ): TaskResult {
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isTaskResult, () => {
      assertTaskOperationAdmitted(this.#database, runId, input.taskId);
      const task = rowOrNotFound(
        this.#database
          .prepare("SELECT owner_agent_id, revision, state FROM tasks WHERE run_id = ? AND task_id = ?")
          .get(runId, input.taskId),
        "task",
      );
      if (numberField(task, "revision") !== input.expectedRevision || stringField(task, "state") !== "active") {
        throw new FabricError("TASK_REVISION_CONFLICT", "task revision or state changed");
      }
      if (task.owner_agent_id !== actorAgentId) {
        throw new FabricError("TASK_NOT_OWNER", "only the current owner may complete the task");
      }
      if (isRow(this.#database.prepare(`
        SELECT 1 FROM provider_actions
         WHERE run_id=? AND task_id=?
           AND status IN ('prepared','dispatched','accepted','ambiguous')
         LIMIT 1
      `).get(runId, input.taskId))) {
        throw new FabricError("LIFECYCLE_PRECONDITION_FAILED", "task has an unresolved provider action");
      }
      this.#database
        .prepare("UPDATE tasks SET state = ?, revision = revision + 1 WHERE run_id = ? AND task_id = ? AND revision = ?")
        .run(input.state, runId, input.taskId, input.expectedRevision);
      this.#memberships.reconcile(runId, [{ kind: "task", memberId: input.taskId }]);
      const result = this.getTask(runId, input.taskId);
      this.#event(runId, "task-updated", actorAgentId, result);
      return result;
    });
  }

  recordTaskOwnerRecoveryProof(
    runId: string,
    actorAgentId: string,
    input: {
      taskId: string;
      ownerLeaseGeneration: number;
      kind: "predecessor-terminal" | "os-isolated" | "patch-only";
      detail: Record<string, string>;
      commandId: string;
    },
  ): ProofResult {
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isProofResult, () => {
      assertTaskOperationAdmitted(this.#database, runId, input.taskId);
      this.#assertChair(runId, actorAgentId);
      const task = this.getTask(runId, input.taskId);
      if (task.state !== "active" || task.ownerAgentId === null || task.ownerLeaseGeneration !== input.ownerLeaseGeneration) {
        throw new FabricError("STALE_LEASE_GENERATION", "task owner generation or state changed");
      }
      if (Object.keys(input.detail).length === 0) throw new FabricError("CAPABILITY_FORBIDDEN", "task recovery proof requires evidence");
      if (input.kind === "predecessor-terminal") {
        if (input.detail.agentId !== task.ownerAgentId) throw new FabricError("CAPABILITY_FORBIDDEN", "proof names the wrong predecessor");
        const revoked = this.#database
          .prepare("SELECT 1 FROM capabilities WHERE run_id = ? AND agent_id = ? AND revoked_at IS NOT NULL")
          .get(runId, task.ownerAgentId);
        if (!isRow(revoked)) throw new FabricError("CAPABILITY_FORBIDDEN", "predecessor capability is not revoked");
      }
      const proofId = uuidv7();
      this.#database
        .prepare(
          "INSERT INTO task_owner_recovery_proofs(proof_id, run_id, task_id, owner_lease_generation, predecessor_agent_id, kind, evidence_json, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .run(
          proofId,
          runId,
          input.taskId,
          input.ownerLeaseGeneration,
          task.ownerAgentId,
          input.kind,
          canonicalJson(input.detail),
          this.#clock(),
        );
      return { proofId };
    });
  }

  recoverTaskOwner(
    runId: string,
    actorAgentId: string,
    input: {
      taskId: string;
      expectedRevision: number;
      expectedOwnerLeaseGeneration: number;
      successorAgentId: string;
      proofId: string;
      commandId: string;
    },
  ): TaskResult {
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isTaskResult, () => {
      assertTaskOperationAdmitted(this.#database, runId, input.taskId);
      this.#assertChair(runId, actorAgentId);
      const task = this.getTask(runId, input.taskId);
      if (
        task.state !== "active" ||
        task.ownerAgentId === null ||
        task.revision !== input.expectedRevision ||
        task.ownerLeaseGeneration !== input.expectedOwnerLeaseGeneration
      ) throw new FabricError("TASK_REVISION_CONFLICT", "task owner revision or generation changed");
      const proof = rowOrNotFound(
        this.#database
          .prepare("SELECT predecessor_agent_id, owner_lease_generation FROM task_owner_recovery_proofs WHERE proof_id = ? AND run_id = ? AND task_id = ?")
          .get(input.proofId, runId, input.taskId),
        "task owner recovery proof",
      );
      if (
        stringField(proof, "predecessor_agent_id") !== task.ownerAgentId ||
        numberField(proof, "owner_lease_generation") !== task.ownerLeaseGeneration
      ) throw new FabricError("STALE_LEASE_GENERATION", "task owner recovery proof is stale");
      const eligible = this.#database
        .prepare("SELECT 1 FROM task_eligible_agents WHERE run_id = ? AND task_id = ? AND agent_id = ?")
        .get(runId, input.taskId, input.successorAgentId);
      if (!isRow(eligible)) throw new FabricError("CAPABILITY_FORBIDDEN", "successor is not eligible for the task");
      this.#database
        .prepare("UPDATE tasks SET owner_agent_id = ?, revision = revision + 1, owner_lease_generation = owner_lease_generation + 1 WHERE run_id = ? AND task_id = ?")
        .run(input.successorAgentId, runId, input.taskId);
      this.#database
        .prepare(
          "INSERT INTO task_owner_recoveries(recovery_id, run_id, task_id, predecessor_agent_id, successor_agent_id, prior_generation, new_generation, evidence_json, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .run(
          uuidv7(),
          runId,
          input.taskId,
          task.ownerAgentId,
          input.successorAgentId,
          task.ownerLeaseGeneration,
          task.ownerLeaseGeneration + 1,
          canonicalJson({ proofId: input.proofId }),
          this.#clock(),
        );
      const result = this.getTask(runId, input.taskId);
      this.#event(runId, "task-owner-recovered", actorAgentId, result);
      return result;
    });
  }

  recordRevocationProof(
    runId: string,
    actorAgentId: string,
    input: {
      leaseId: string;
      generation: number;
      kind: "predecessor-terminal" | "os-isolated" | "patch-only";
      detail: Record<string, string>;
      commandId: string;
    },
  ): ProofResult {
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isProofResult, () => {
      this.#assertChair(runId, actorAgentId);
      const lease = rowOrNotFound(
        this.#database
          .prepare("SELECT holder_agent_id, generation FROM leases WHERE run_id = ? AND lease_id = ?")
          .get(runId, input.leaseId),
        "lease",
      );
      if (numberField(lease, "generation") !== input.generation) {
        throw new FabricError("STALE_LEASE_GENERATION", "proof generation is stale");
      }
      if (input.kind === "predecessor-terminal" && input.detail.agentId !== stringField(lease, "holder_agent_id")) {
        throw new FabricError("CAPABILITY_FORBIDDEN", "proof names the wrong predecessor");
      }
      if (Object.keys(input.detail).length === 0) {
        throw new FabricError("CAPABILITY_FORBIDDEN", "revocation proof requires evidence detail");
      }
      const proofId = uuidv7();
      this.#database
        .prepare(
          "INSERT INTO revocation_proofs(proof_id, lease_id, generation, kind, evidence_json, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .run(proofId, input.leaseId, input.generation, input.kind, canonicalJson(input.detail), this.#clock());
      this.#event(runId, "revocation-proof-recorded", actorAgentId, {
        proofId,
        leaseId: input.leaseId,
        generation: input.generation,
        kind: input.kind,
      });
      return { proofId };
    });
  }

  revokeCapability(
    runId: string,
    actorAgentId: string,
    input: { agentId: string; commandId: string },
  ): void {
    this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isRevocationResult, () => {
      this.#assertChair(runId, actorAgentId);
      const result = this.#database
        .prepare(
          "UPDATE capabilities SET revoked_at = ?, principal_generation = principal_generation + 1 WHERE run_id = ? AND agent_id = ? AND revoked_at IS NULL",
        )
        .run(this.#clock(), runId, input.agentId);
      if (result.changes === 0) {
        rowOrNotFound(
          this.#database.prepare("SELECT 1 FROM agents WHERE run_id = ? AND agent_id = ?").get(runId, input.agentId),
          "agent",
        );
      }
      this.#event(runId, "capability-revoked", actorAgentId, { agentId: input.agentId });
      return { revoked: true };
    });
  }

  rotateCapability(
    runId: string,
    actorAgentId: string,
    input: { agentId: string; expectedPrincipalGeneration: number; commandId: string },
  ): CapabilityRotationResult {
    const stored = this.#commandJournal.execute(
      runId,
      actorAgentId,
      input.commandId,
      input,
      (value): value is { agentId: string; principalGeneration: number } => isRow(value) && typeof value.agentId === "string" && typeof value.principalGeneration === "number",
      () => {
        this.#assertChair(runId, actorAgentId);
        const current = rowOrNotFound(this.#database.prepare(`
          SELECT c.principal_generation, a.authority_json, a.authority_hash
            FROM capabilities c JOIN agents g ON g.run_id = c.run_id AND g.agent_id = c.agent_id
            JOIN authorities a ON a.authority_id = g.authority_id
           WHERE c.run_id = ? AND c.agent_id = ? AND c.revoked_at IS NULL
           ORDER BY c.principal_generation DESC LIMIT 1
        `).get(runId, input.agentId), "active capability");
        const generation = numberField(current, "principal_generation");
        if (generation !== input.expectedPrincipalGeneration) throw new FabricError("STALE_PRINCIPAL_GENERATION", "principal generation changed");
        const nextGeneration = generation + 1;
        const token = capabilityToken(this.#capabilityKey, runId, input.agentId, nextGeneration);
        const authority = parseAuthority(current);
        this.#database.prepare("UPDATE capabilities SET revoked_at = ? WHERE run_id = ? AND agent_id = ? AND revoked_at IS NULL").run(this.#clock(), runId, input.agentId);
        this.#database.prepare("INSERT INTO capabilities(token_hash, run_id, agent_id, principal_generation, expires_at) VALUES (?, ?, ?, ?, ?)").run(sha256(token), runId, input.agentId, nextGeneration, Date.parse(authority.expiresAt));
        this.#event(runId, "capability-rotated", actorAgentId, { agentId: input.agentId, priorGeneration: generation, principalGeneration: nextGeneration });
        return { agentId: input.agentId, principalGeneration: nextGeneration };
      },
    );
    return { ...stored, capability: capabilityToken(this.#capabilityKey, runId, input.agentId, stored.principalGeneration) };
  }

  acquireWriteLease(
    runId: string,
    actorAgentId: string,
    input: { scope: string[]; ttlMs: number; commandId: string; taskId?: string },
  ): LeaseResult {
    const workspaceRoot = this.#workspaceRootForRun(runId);
    const scopes = [...new Set(input.scope.map((path) => canonicalAuthorityPath(workspaceRoot, path)))].sort();
    const actor = rowOrNotFound(
      this.#database
        .prepare(
          "SELECT a.authority_json, a.authority_hash FROM agents g JOIN authorities a ON a.authority_id = g.authority_id WHERE g.run_id = ? AND g.agent_id = ?",
        )
        .get(runId, actorAgentId),
      "agent authority",
    );
    const authority = parseAuthority(actor);
    if (
      scopes.some(
        (scope) =>
          !authority.workspaceRoots.some((root) => pathContains(root, scope)) ||
          !authority.sourcePaths.some((root) => pathContains(root, scope)) ||
          authority.deniedPaths.some((denied) => scopesOverlap(denied, scope)),
      )
    ) {
      throw new FabricError("AUTHORITY_WIDENING", "write scope exceeds the actor authority");
    }
    const commandPayload = {
      operation: "acquire-write",
      scopes,
      ttlMs: input.ttlMs,
      taskId: input.taskId ?? null,
    };
    const replay = this.#commandJournal.read(runId, actorAgentId, input.commandId, commandPayload, isLeaseResult);
    if (replay !== undefined) return replay;
    const taskId = resolveTaskBindingForActiveWork(this.#database, runId, actorAgentId, input.taskId);
    assertScopedOperationAllowed(
      this.#database,
      runId,
      FABRIC_OPERATIONS.acquireWriteLease,
      taskId === undefined ? { kind: "run" } : { kind: "task", taskId: taskId as never },
    );
    if (taskId !== undefined) {
      assertScopedTaskReadinessAllowed(this.#database, runId, taskId);
      const bound = this.#database.prepare(`
        SELECT 1 FROM tasks WHERE run_id=? AND task_id=?
          AND (owner_agent_id=? OR EXISTS (
            SELECT 1 FROM task_participants participant
             WHERE participant.run_id=tasks.run_id AND participant.task_id=tasks.task_id
               AND participant.agent_id=?
          ))
      `).get(runId, taskId, actorAgentId, actorAgentId);
      if (!isRow(bound)) throw new FabricError("CAPABILITY_FORBIDDEN", "write lease task is outside the actor task scope");
    }
    const lifecycle = this.#service.getAgentLifecycle(runId, actorAgentId);
    if (lifecycle.contextState === "context-unreconciled") {
      throw new FabricError("CONTEXT_UNRECONCILED", "unreconciled provider context cannot acquire a write lease");
    }
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, commandPayload, isLeaseResult, () => {
      const conflicts = this.#writeLeaseConflicts(runId, scopes);
      if (conflicts.some((item) => item.status === "quarantined")) {
        throw new FabricError("WRITE_SCOPE_QUARANTINED", "write scope overlaps a quarantined lease");
      }
      const activeConflict = conflicts.find((item) => item.status === "active");
      if (activeConflict !== undefined) {
        if (activeConflict.expiresAt <= this.#clock()) {
          throw new FabricError(
            "WRITE_SCOPE_RECOVERY_REQUIRED",
            "write scope overlaps an expired lease whose predecessor has not been fenced",
          );
        }
        throw new FabricError("WRITE_SCOPE_CONFLICT", "write scope overlaps an active lease");
      }
      const leaseId = uuidv7();
      const now = this.#clock();
      this.#database
        .prepare(
          "INSERT INTO leases(lease_id, run_id, kind, holder_agent_id, generation, status, expires_at, updated_at) VALUES (?, ?, 'write', ?, 1, 'active', ?, ?)",
        )
        .run(leaseId, runId, actorAgentId, now + input.ttlMs, now);
      for (const scope of scopes) {
        this.#database.prepare("INSERT INTO write_scope_entries(lease_id, canonical_path) VALUES (?, ?)").run(leaseId, scope);
      }
      if (taskId !== undefined) {
        this.#database.prepare(`
          INSERT INTO task_obligation_bindings(
            coordination_run_id, task_id, obligation_kind, obligation_id, state, created_at, updated_at
          ) VALUES (?, ?, 'write-lease', ?, 'active', ?, ?)
        `).run(runId, taskId, leaseId, now, now);
      }
      this.#memberships.bindRequired(runId, [{ kind: "lease", memberId: leaseId }]);
      const result: LeaseResult = { leaseId, holderAgentId: actorAgentId, generation: 1, status: "active", scope: scopes };
      this.#event(runId, "write-lease-acquired", actorAgentId, result);
      return result;
    });
  }

  #writeLeaseConflicts(runId: string, scopes: string[]): Array<{ status: string; expiresAt: number }> {
    const rows = this.#database
      .prepare(
        "SELECT l.status, l.expires_at, w.canonical_path FROM leases l JOIN write_scope_entries w ON w.lease_id = l.lease_id WHERE l.run_id = ? AND l.kind = 'write' AND l.status IN ('active', 'quarantined')",
      )
      .all(runId);
    return rows
      .map((value) => rowOrNotFound(value, "lease"))
      .filter((row) => scopes.some((scope) => scopesOverlap(scope, stringField(row, "canonical_path"))))
      .map((row) => ({ status: stringField(row, "status"), expiresAt: numberField(row, "expires_at") }));
  }

  recoverWriteLease(
    runId: string,
    actorAgentId: string,
    input: {
      leaseId: string;
      expectedGeneration: number;
      commandId: string;
      evidence: RecoveryEvidence;
    },
  ): LeaseResult {
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isLeaseResult, () => {
      this.#assertChair(runId, actorAgentId);
      const lease = rowOrNotFound(
        this.#database
          .prepare("SELECT holder_agent_id, generation, status, expires_at FROM leases WHERE lease_id = ? AND run_id = ?")
          .get(input.leaseId, runId),
        "lease",
      );
      const generation = numberField(lease, "generation");
      if (generation !== input.expectedGeneration) {
        throw new FabricError("STALE_LEASE_GENERATION", "lease generation is stale");
      }
      if (numberField(lease, "expires_at") > this.#clock()) {
        throw new FabricError("LEASE_NOT_EXPIRED", "lease has not expired");
      }
      const scopes = this.#database
        .prepare("SELECT canonical_path FROM write_scope_entries WHERE lease_id = ? ORDER BY canonical_path")
        .all(input.leaseId)
        .map((value) => stringField(rowOrNotFound(value, "scope"), "canonical_path"));
      if (input.evidence.kind === "unproven") {
        this.#database
          .prepare("UPDATE leases SET status = 'quarantined', updated_at = ? WHERE lease_id = ?")
          .run(this.#clock(), input.leaseId);
        const result: LeaseResult = {
          leaseId: input.leaseId,
          holderAgentId: stringField(lease, "holder_agent_id"),
          generation,
          status: "quarantined",
          scope: scopes,
        };
        this.#event(runId, "write-lease-quarantined", actorAgentId, result);
        return result;
      }
      if (
        input.evidence.kind === "predecessor-terminal" &&
        input.evidence.agentId !== stringField(lease, "holder_agent_id")
      ) {
        throw new FabricError("CAPABILITY_FORBIDDEN", "terminal evidence names the wrong predecessor");
      }
      const evidenceDetail: Record<string, string> =
        input.evidence.kind === "predecessor-terminal"
          ? { agentId: input.evidence.agentId, providerSessionRef: input.evidence.providerSessionRef }
          : input.evidence.kind === "os-isolated"
            ? { proofRef: input.evidence.proofRef }
            : { serialApplierRef: input.evidence.serialApplierRef };
      const proof = this.#database
        .prepare(
          "SELECT proof_id FROM revocation_proofs WHERE lease_id = ? AND generation = ? AND kind = ? AND evidence_json = ?",
        )
        .get(input.leaseId, generation, input.evidence.kind, canonicalJson(evidenceDetail));
      if (!isRow(proof)) {
        throw new FabricError("CAPABILITY_FORBIDDEN", "recovery requires chair-recorded revocation proof");
      }
      const nextGeneration = generation + 1;
      this.#database
        .prepare(
          "UPDATE leases SET holder_agent_id = ?, generation = ?, status = 'active', expires_at = ?, updated_at = ? WHERE lease_id = ?",
        )
        .run(actorAgentId, nextGeneration, this.#clock() + 30_000, this.#clock(), input.leaseId);
      const result: LeaseResult = {
        leaseId: input.leaseId,
        holderAgentId: actorAgentId,
        generation: nextGeneration,
        status: "active",
        scope: scopes,
      };
      this.#event(runId, "write-lease-recovered", actorAgentId, result);
      return result;
    });
  }

  renewWriteLease(
    runId: string,
    actorAgentId: string,
    input: { leaseId: string; expectedGeneration: number; ttlMs: number; commandId: string },
  ): LeaseResult {
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isLeaseResult, () => {
      const lease = rowOrNotFound(
        this.#database
          .prepare("SELECT holder_agent_id, generation, status, expires_at FROM leases WHERE lease_id = ? AND run_id = ?")
          .get(input.leaseId, runId),
        "lease",
      );
      const generation = numberField(lease, "generation");
      if (generation !== input.expectedGeneration) {
        throw new FabricError("STALE_LEASE_GENERATION", "lease generation is stale");
      }
      if (stringField(lease, "status") === "quarantined") {
        throw new FabricError("LEASE_QUARANTINED", "lease is quarantined");
      }
      if (stringField(lease, "holder_agent_id") !== actorAgentId) {
        throw new FabricError("CAPABILITY_FORBIDDEN", "actor does not hold the lease");
      }
      const now = this.#clock();
      if (numberField(lease, "expires_at") <= now) {
        throw new FabricError("LEASE_EXPIRED", "expired lease cannot be renewed");
      }
      this.#database.prepare("UPDATE leases SET expires_at = ?, updated_at = ? WHERE lease_id = ?").run(
        now + input.ttlMs,
        now,
        input.leaseId,
      );
      const scopes = this.#database
        .prepare("SELECT canonical_path FROM write_scope_entries WHERE lease_id = ? ORDER BY canonical_path")
        .all(input.leaseId)
        .map((value) => stringField(rowOrNotFound(value, "scope"), "canonical_path"));
      return {
        leaseId: input.leaseId,
        holderAgentId: actorAgentId,
        generation,
        status: "active",
        scope: scopes,
      };
    });
  }

  getWriteLease(runId: string, leaseId: string): LeaseResult {
    const lease = rowOrNotFound(
      this.#database
        .prepare("SELECT holder_agent_id, generation, status FROM leases WHERE run_id = ? AND lease_id = ? AND kind = 'write'")
        .get(runId, leaseId),
      "write lease",
    );
    const statusValue = stringField(lease, "status");
    if (statusValue !== "active" && statusValue !== "quarantined") {
      throw new FabricError("NOT_FOUND", "write lease is no longer visible");
    }
    const scope = this.#database
      .prepare("SELECT canonical_path FROM write_scope_entries WHERE lease_id = ? ORDER BY canonical_path")
      .all(leaseId)
      .map((value) => stringField(rowOrNotFound(value, "scope"), "canonical_path"));
    return {
      leaseId,
      holderAgentId: stringField(lease, "holder_agent_id"),
      generation: numberField(lease, "generation"),
      status: statusValue,
      scope,
    };
  }

  releaseWriteLease(
    runId: string,
    actorAgentId: string,
    input: { leaseId: string; expectedGeneration: number; commandId: string },
  ): { leaseId: string; status: "released"; generation: number } {
    const parse = (value: unknown): value is { leaseId: string; status: "released"; generation: number } =>
      isRow(value) && value.leaseId === input.leaseId && value.status === "released" && typeof value.generation === "number";
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, parse, () => {
      const lease = rowOrNotFound(
        this.#database
          .prepare("SELECT holder_agent_id, generation, status FROM leases WHERE run_id = ? AND lease_id = ?")
          .get(runId, input.leaseId),
        "write lease",
      );
      if (stringField(lease, "holder_agent_id") !== actorAgentId) {
        throw new FabricError("CAPABILITY_FORBIDDEN", "only the lease holder may release it");
      }
      const generation = numberField(lease, "generation");
      if (generation !== input.expectedGeneration) {
        throw new FabricError("STALE_LEASE_GENERATION", "lease generation changed");
      }
      if (stringField(lease, "status") !== "active") {
        throw new FabricError("LEASE_QUARANTINED", "only an active lease can be released");
      }
      this.#database.prepare("UPDATE leases SET status = 'released', updated_at = ? WHERE lease_id = ?").run(
        this.#clock(),
        input.leaseId,
      );
      this.#memberships.reconcile(runId, [{ kind: "lease", memberId: input.leaseId }]);
      this.#event(runId, "write-lease-released", actorAgentId, { leaseId: input.leaseId, generation });
      return { leaseId: input.leaseId, status: "released", generation };
    });
  }

  getAgentLifecycle(runId: string, agentId: string): LifecycleCurrentStateV1 {
    return this.#service.getAgentLifecycle(runId, agentId);
  }

  reportProviderState(
    runId: string,
    actorAgentId: string,
    input: {
      sourceEventId: string;
      providerSessionRef: string;
      agentId: string;
      providerSessionGeneration: number;
      contextRevision: number;
      evidenceDigest: `sha256:${string}`;
      checkpointSha256?: string;
      commandId: string;
    },
  ): LifecycleResult {
    return this.#service.reportProviderState(runId, actorAgentId, input);
  }

  async requestLifecycle(
    runId: string,
    actorAgentId: string,
    input: {
      action: "compact" | "rotate" | "completion-ready" | "release";
      agentId: string;
      taskId: string;
      taskRevision: number;
      checkpoint: LifecycleCheckpoint;
      commandId: string;
    },
  ): Promise<LifecycleResult> {
    return await this.#service.requestLifecycle(runId, actorAgentId, input);
  }

  recordOperatorIntervention(
    runId: string,
    actorAgentId: string,
    input: {
      source: "fabric" | "integration";
      directInputProvenance: "complete" | "partial" | "unavailable";
      taskRevision: number;
      summary: string;
      commandId: string;
    },
  ): InterventionResult {
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isInterventionResult, () => {
      const interventionId = uuidv7();
      this.#database
        .prepare(
          "INSERT INTO operator_interventions(intervention_id, run_id, actor_agent_id, source, direct_input_provenance, task_revision, summary, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .run(
          interventionId,
          runId,
          actorAgentId,
          input.source,
          input.directInputProvenance,
          input.taskRevision,
          input.summary,
          this.#clock(),
        );
      this.#event(runId, "operator-intervention", actorAgentId, { interventionId, ...input });
      return { interventionId };
    });
  }

  recordVisibilityFailure(
    runId: string,
    actorAgentId: string,
    input: { kind: "herdr-telemetry" | "observer-pane" | "interactive-tui"; agentId: string; commandId: string },
  ): { visibility: "degraded" | "lost"; providerSession: "healthy" | "lost"; delivery: "active" | "frozen"; recovery?: "reattach-or-rotate" } {
    return this.#commandJournal.execute<{ visibility: "degraded" | "lost"; providerSession: "healthy" | "lost"; delivery: "active" | "frozen"; recovery?: "reattach-or-rotate" }>(runId, actorAgentId, input.commandId, input, (value): value is { visibility: "degraded" | "lost"; providerSession: "healthy" | "lost"; delivery: "active" | "frozen"; recovery?: "reattach-or-rotate" } =>
      isRow(value) && (value.visibility === "degraded" || value.visibility === "lost") && (value.providerSession === "healthy" || value.providerSession === "lost") && (value.delivery === "active" || value.delivery === "frozen"), () => {
      this.#assertChair(runId, actorAgentId);
      rowOrNotFound(this.#database.prepare("SELECT 1 FROM agents WHERE run_id = ? AND agent_id = ?").get(runId, input.agentId), "visibility agent");
      if (input.kind === "interactive-tui") {
        this.#database.prepare("UPDATE agents SET lifecycle = 'suspended' WHERE run_id = ? AND agent_id = ?").run(runId, input.agentId);
        this.#database.prepare("INSERT INTO delivery_freezes(run_id, agent_id, reason, created_at) VALUES (?, ?, ?, ?) ON CONFLICT(run_id, agent_id) DO UPDATE SET reason = excluded.reason, created_at = excluded.created_at").run(runId, input.agentId, "interactive-tui-lost", this.#clock());
        const result = { visibility: "lost" as const, providerSession: "lost" as const, delivery: "frozen" as const, recovery: "reattach-or-rotate" as const };
        this.#event(runId, "visibility-degraded", actorAgentId, { ...input, ...result });
        return result;
      }
      const result = { visibility: "degraded" as const, providerSession: "healthy" as const, delivery: "active" as const };
      this.#event(runId, "visibility-degraded", actorAgentId, { ...input, ...result });
      return result;
    });
  }

  async dispatchProviderAction(
    runId: string,
    actorAgentId: string,
    input: ProviderActionDispatchRequest,
  ): Promise<ProviderActionResult> {
    return await this.#providerActionCoordinator.dispatch(runId, actorAgentId, input);
  }

  async reconcileProviderAction(
    runId: string,
    actorAgentId: string,
    input: { adapterId: string; actionId: string; commandId: string },
  ): Promise<ProviderActionResult> {
    return await this.#providerActionCoordinator.reconcile(runId, actorAgentId, input);
  }

  getProviderAction(runId: string, adapterId: string, actionId: string): ProviderActionResult {
    return this.#providerActionCoordinator.get(runId, adapterId, actionId);
  }

  createTeam(runId: string, actorAgentId: string, input: TeamCreateInput): TeamResult {
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isTeamResult, () => {
      const { depth, parentTeamId } = this.#teamCreationPosition(runId, actorAgentId, input.parentTeamId);
      return this.#createTeam(runId, actorAgentId, input, depth, parentTeamId);
    });
  }

  #createTeam(
    runId: string,
    actorAgentId: string,
    input: TeamCreateInput,
    depth: number,
    parentTeamId: string | null,
  ): TeamResult {
    if (input.initialMembers.length > 5) {
      throw new FabricError("BUDGET_EXCEEDED", "team exceeds five workers");
    }
    validateIntegerBudget(input.reservedBudget);
    const leaderAuthority = normaliseAuthority(input.leader.authority, this.#workspaceRootForRun(runId));
    for (const [unit, reserved] of Object.entries(input.reservedBudget)) {
      if ((leaderAuthority.budget[unit] ?? -1) < reserved) {
        throw new FabricError("BUDGET_EXCEEDED", `team reservation exceeds leader authority for ${unit}`);
      }
      const memberTotal = input.initialMembers.reduce((sum, member) => sum + (member.authority.budget[unit] ?? 0), 0);
      if (memberTotal > reserved) throw new FabricError("BUDGET_EXCEEDED", `member reservations exceed team budget for ${unit}`);
    }
    const actorAuthority = rowOrNotFound(
      this.#database.prepare("SELECT authority_id FROM agents WHERE run_id = ? AND agent_id = ?").get(runId, actorAgentId),
      "team creator",
    );
    const leaderGrant = this.delegateAuthority(runId, actorAgentId, {
      parentAuthorityId: stringField(actorAuthority, "authority_id"),
      authority: input.leader.authority,
      commandId: `${input.commandId}:leader-authority`,
    });
    this.#registerAgentIdentity(runId, actorAgentId, {
      agentId: input.leader.agentId,
      authorityId: leaderGrant.authorityId,
    });
    const initialMembers: Array<{ agentId: string; authorityId: string }> = [];
    for (const member of input.initialMembers) {
      const grant = this.delegateAuthority(runId, input.leader.agentId, {
        parentAuthorityId: leaderGrant.authorityId,
        authority: member.authority,
        commandId: `${input.commandId}:member-authority:${member.agentId}`,
      });
      this.#registerAgentIdentity(runId, input.leader.agentId, { agentId: member.agentId, authorityId: grant.authorityId });
      initialMembers.push({ agentId: member.agentId, authorityId: grant.authorityId });
    }
    const rootTask = this.#createTask(runId, actorAgentId, {
      taskId: input.rootTask.taskId,
      authorityId: leaderGrant.authorityId,
      proposedOwnerAgentId: input.leader.agentId,
      participantAgentIds: [input.leader.agentId, ...initialMembers.map((member) => member.agentId)],
      eligibleAgentIds: [input.leader.agentId],
      dependencies: [],
      objective: input.rootTask.objective,
      baseRevision: input.rootTask.baseRevision,
      commandId: `${input.commandId}:root-task`,
    }, false);
    const budgetId = `${input.teamId}:budget`;
    const initialReserved = Object.fromEntries(
      Object.keys(input.reservedBudget).map((unit) => [
        unit,
        input.initialMembers.reduce((sum, member) => sum + (member.authority.budget[unit] ?? 0), 0),
      ]),
    );
    this.#insertTeamRecords(runId, {
      teamId: input.teamId,
      parentTeamId,
      depth,
      leaderAgentId: input.leader.agentId,
      rootTaskId: rootTask.taskId,
      ownedTaskIds: [rootTask.taskId],
      memberAgentIds: [input.leader.agentId, ...initialMembers.map((member) => member.agentId)],
      authorityId: leaderGrant.authorityId,
      budgetId,
      budget: input.reservedBudget,
      initialReserved,
      discussionGroups: input.discussionGroups,
      actorAgentId,
    });
    return {
      ...this.getTeam(runId, input.teamId),
      leader: {
        agentId: input.leader.agentId,
        authorityId: leaderGrant.authorityId,
      },
      rootTask,
      initialMembers,
    };
  }

  #teamCreationPosition(runId: string, actorAgentId: string, requestedParentTeamId?: string): {
    depth: number;
    parentTeamId: string | null;
  } {
    const totalLeaders = numberField(
      rowOrNotFound(
        this.#database.prepare("SELECT COUNT(*) AS count FROM teams WHERE run_id = ?").get(runId),
        "leader count",
      ),
      "count",
    );
    if (totalLeaders >= 4) throw new FabricError("BUDGET_EXCEEDED", "run already has four team leaders");
    if (requestedParentTeamId === undefined) {
      this.#assertChair(runId, actorAgentId);
      return { depth: 1, parentTeamId: null };
    }
    const parent = rowOrNotFound(
      this.#database.prepare("SELECT depth, leader_agent_id, state FROM teams WHERE run_id = ? AND team_id = ?").get(runId, requestedParentTeamId),
      "parent team",
    );
    if (stringField(parent, "leader_agent_id") !== actorAgentId || stringField(parent, "state") !== "active") {
      throw new FabricError("CAPABILITY_FORBIDDEN", "only an active parent-team leader may create a child team");
    }
    const depth = numberField(parent, "depth") + 1;
    if (depth > 2) throw new FabricError("TEAM_DEPTH_EXCEEDED", "team depth exceeds two levels below the chair");
    return { depth, parentTeamId: requestedParentTeamId };
  }

  #insertTeamRecords(
    runId: string,
    input: {
      teamId: string;
      parentTeamId: string | null;
      depth: number;
      leaderAgentId: string;
      rootTaskId: string;
      ownedTaskIds: string[];
      memberAgentIds: string[];
      authorityId: string;
      budgetId: string;
      budget: Record<string, number>;
      initialReserved: Record<string, number>;
      discussionGroups: DiscussionGroupInput[];
      actorAgentId: string;
    },
  ): void {
    validateIntegerBudget(input.budget);
    validateIntegerBudget(input.initialReserved);
    for (const taskId of input.ownedTaskIds) {
      rowOrNotFound(this.#database.prepare("SELECT 1 FROM tasks WHERE run_id = ? AND task_id = ?").get(runId, taskId), `team task ${taskId}`);
      if (isRow(this.#database.prepare("SELECT 1 FROM team_owned_tasks WHERE run_id = ? AND task_id = ?").get(runId, taskId))) {
        throw new FabricError("TASK_SUBTREE_CONFLICT", `task is already owned by another team: ${taskId}`);
      }
    }
    this.#database
      .prepare(
        "INSERT INTO teams(run_id, team_id, parent_team_id, depth, leader_agent_id, original_leader_agent_id, root_task_id, authority_id, budget_id, state, generation, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'active', 1, ?)",
      )
      .run(
        runId,
        input.teamId,
        input.parentTeamId,
        input.depth,
        input.leaderAgentId,
        input.leaderAgentId,
        input.rootTaskId,
        input.authorityId,
        input.budgetId,
        this.#clock(),
      );
    for (const agentId of [...new Set(input.memberAgentIds)].sort()) {
      this.#database.prepare("INSERT INTO team_members(run_id, team_id, agent_id) VALUES (?, ?, ?)").run(runId, input.teamId, agentId);
    }
    for (const taskId of [...new Set(input.ownedTaskIds)].sort()) {
      this.#database.prepare("INSERT INTO team_owned_tasks(run_id, team_id, task_id) VALUES (?, ?, ?)").run(runId, input.teamId, taskId);
    }
    const parentBudgetId = input.parentTeamId === null ? null : this.getTeam(runId, input.parentTeamId).budgetId;
    if (parentBudgetId !== null) {
      for (const [unit, requested] of Object.entries(input.budget)) {
        const parent = rowOrNotFound(
          this.#database
            .prepare("SELECT granted, reserved, consumed, usage_unknown FROM budget_dimensions WHERE run_id = ? AND budget_id = ? AND unit_key = ?")
            .get(runId, parentBudgetId, unit),
          `parent team budget ${unit}`,
        );
        if (numberField(parent, "usage_unknown") === 1) throw new FabricError("BUDGET_USAGE_UNKNOWN", `parent team usage is unknown for ${unit}`);
        if (numberField(parent, "granted") - numberField(parent, "reserved") - numberField(parent, "consumed") < requested) {
          throw new FabricError("BUDGET_EXCEEDED", `child team exceeds inherited parent budget for ${unit}`);
        }
      }
      for (const [unit, requested] of Object.entries(input.budget)) {
        this.#database
          .prepare("UPDATE budget_dimensions SET reserved = reserved + ? WHERE run_id = ? AND budget_id = ? AND unit_key = ?")
          .run(requested, runId, parentBudgetId, unit);
      }
    }
    this.#database
      .prepare("INSERT INTO budgets(run_id, budget_id, parent_budget_id, team_id, owner_agent_id, state, created_at) VALUES (?, ?, ?, ?, ?, 'active', ?)")
      .run(runId, input.budgetId, parentBudgetId, input.teamId, input.leaderAgentId, this.#clock());
    for (const [unit, granted] of Object.entries(input.budget)) {
      this.#database
        .prepare("INSERT INTO budget_dimensions(run_id, budget_id, unit_key, granted, reserved) VALUES (?, ?, ?, ?, ?)")
        .run(runId, input.budgetId, unit, granted, input.initialReserved[unit] ?? 0);
    }
    for (const group of input.discussionGroups) {
      const members = [...new Set(group.memberAgentIds)];
      if (members.some((agentId) => !input.memberAgentIds.includes(agentId))) {
        throw new FabricError("MESSAGE_RELATIONSHIP_FORBIDDEN", "discussion group contains a non-team member");
      }
      this.#database
        .prepare("INSERT INTO discussion_groups(run_id, group_id, team_id, created_by) VALUES (?, ?, ?, ?)")
        .run(runId, group.groupId, input.teamId, input.actorAgentId);
      for (const agentId of members) {
        this.#database
          .prepare("INSERT INTO discussion_group_members(run_id, group_id, agent_id) VALUES (?, ?, ?)")
          .run(runId, group.groupId, agentId);
      }
    }
  }

  getTeam(runId: string, teamId: string): TeamResult {
    const team = rowOrNotFound(
      this.#database
        .prepare("SELECT parent_team_id, depth, leader_agent_id, successor_agent_id, root_task_id, budget_id, state, generation FROM teams WHERE run_id = ? AND team_id = ?")
        .get(runId, teamId),
      "team",
    );
    const parentValue = team.parent_team_id;
    const successorValue = team.successor_agent_id;
    if ((typeof parentValue !== "string" && parentValue !== null) || (typeof successorValue !== "string" && successorValue !== null)) {
      throw new Error("stored team relationship is invalid");
    }
    const ownedTaskIds = this.#database
      .prepare("SELECT task_id FROM team_owned_tasks WHERE run_id = ? AND team_id = ? ORDER BY task_id")
      .all(runId, teamId)
      .map((value) => stringField(rowOrNotFound(value, "team task"), "task_id"));
    const memberAgentIds = this.#database
      .prepare("SELECT agent_id FROM team_members WHERE run_id = ? AND team_id = ? ORDER BY agent_id")
      .all(runId, teamId)
      .map((value) => stringField(rowOrNotFound(value, "team member"), "agent_id"));
    const groups = this.#database
      .prepare("SELECT group_id FROM discussion_groups WHERE run_id = ? AND team_id = ? ORDER BY group_id")
      .all(runId, teamId)
      .map((value) => {
        const groupId = stringField(rowOrNotFound(value, "discussion group"), "group_id");
        const members = this.#database
          .prepare("SELECT agent_id FROM discussion_group_members WHERE run_id = ? AND group_id = ? ORDER BY agent_id")
          .all(runId, groupId)
          .map((item) => stringField(rowOrNotFound(item, "discussion member"), "agent_id"));
        return { groupId, memberAgentIds: members };
      });
    const budgetId = stringField(team, "budget_id");
    const budget = this.getBudget(runId, budgetId);
    const reservedBudget = Object.fromEntries(Object.entries(budget.dimensions).map(([unit, value]) => [unit, value.granted]));
    const state = stringField(team, "state");
    if (state !== "active" && state !== "frozen" && state !== "barrier-closed") throw new Error("stored team state is invalid");
    return {
      teamId,
      parentTeamId: parentValue,
      depth: numberField(team, "depth"),
      leaderAgentId: stringField(team, "leader_agent_id"),
      rootTaskId: stringField(team, "root_task_id"),
      ownedTaskIds,
      memberAgentIds,
      budgetId,
      state,
      generation: numberField(team, "generation"),
      successorAgentId: successorValue,
      discussionGroups: groups,
      reservedBudget,
    };
  }

  freezeSubtree(
    runId: string,
    actorAgentId: string,
    input: { teamId: string; expectedGeneration: number; reason: string; commandId: string },
  ): TeamResult {
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isTeamResult, () => {
      this.#assertChair(runId, actorAgentId);
      const team = this.getTeam(runId, input.teamId);
      if (team.generation !== input.expectedGeneration) throw new FabricError("STALE_TEAM_GENERATION", "team generation changed");
      if (input.reason.length === 0) throw new FabricError("CAPABILITY_FORBIDDEN", "subtree freeze requires a reason");
      const teamIds = this.#teamSubtreeIds(runId, input.teamId);
      const placeholders = teamIds.map(() => "?").join(",");
      this.#database
        .prepare(`UPDATE teams SET state = 'frozen', generation = generation + 1, successor_agent_id = NULL WHERE run_id = ? AND team_id IN (${placeholders})`)
        .run(runId, ...teamIds);
      this.#event(runId, "subtree-frozen", actorAgentId, { teamId: input.teamId, teamIds, reason: input.reason });
      return this.getTeam(runId, input.teamId);
    });
  }

  #teamSubtreeIds(runId: string, teamId: string): string[] {
    return this.#database.prepare(`
      WITH RECURSIVE subtree(team_id) AS (
        SELECT team_id FROM teams WHERE run_id = ? AND team_id = ?
        UNION ALL
        SELECT child.team_id
          FROM teams child
          JOIN subtree parent ON child.parent_team_id = parent.team_id
         WHERE child.run_id = ?
      )
      SELECT team_id FROM subtree ORDER BY team_id
    `).all(runId, teamId, runId).map((value) => stringField(rowOrNotFound(value, "subtree team"), "team_id"));
  }

  adoptSubtree(
    runId: string,
    actorAgentId: string,
    input: {
      teamId: string;
      successorAgentId: string;
      expectedGeneration: number;
      handoffEvidence: string;
      commandId: string;
    },
  ): TeamResult {
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isTeamResult, () => {
      this.#assertChair(runId, actorAgentId);
      const team = this.getTeam(runId, input.teamId);
      if (team.generation !== input.expectedGeneration) throw new FabricError("STALE_TEAM_GENERATION", "team generation changed");
      if (team.state !== "frozen" || input.handoffEvidence.length === 0) {
        throw new FabricError("CAPABILITY_FORBIDDEN", "adoption requires a frozen team and handoff evidence");
      }
      rowOrNotFound(
        this.#database.prepare("SELECT 1 FROM agents WHERE run_id = ? AND agent_id = ?").get(runId, input.successorAgentId),
        "successor agent",
      );
      const teamIds = this.#teamSubtreeIds(runId, input.teamId);
      const descendantIds = teamIds.filter((teamId) => teamId !== input.teamId);
      if (descendantIds.length > 0) {
        const placeholders = descendantIds.map(() => "?").join(",");
        this.#database
          .prepare(`UPDATE teams SET state = 'active', generation = generation + 1 WHERE run_id = ? AND team_id IN (${placeholders}) AND state = 'frozen'`)
          .run(runId, ...descendantIds);
      }
      this.#database
        .prepare(
          "UPDATE teams SET state = 'active', generation = generation + 1, leader_agent_id = ?, successor_agent_id = ?, handoff_evidence = ? WHERE run_id = ? AND team_id = ?",
        )
        .run(input.successorAgentId, input.successorAgentId, input.handoffEvidence, runId, input.teamId);
      this.#database
        .prepare("INSERT OR IGNORE INTO team_members(run_id, team_id, agent_id) VALUES (?, ?, ?)")
        .run(runId, input.teamId, input.successorAgentId);
      this.#database
        .prepare("UPDATE budgets SET owner_agent_id = ? WHERE run_id = ? AND budget_id = ?")
        .run(input.successorAgentId, runId, team.budgetId);
      this.#event(runId, "subtree-adopted", actorAgentId, { teamId: input.teamId, teamIds, successorAgentId: input.successorAgentId });
      return this.getTeam(runId, input.teamId);
    });
  }

  closeSubtreeBarrier(
    runId: string,
    actorAgentId: string,
    input: { teamId: string; expectedGeneration: number; commandId: string },
  ): { teamId: string; generation: number; closed: true } {
    const parse = (value: unknown): value is { teamId: string; generation: number; closed: true } =>
      isRow(value) && typeof value.teamId === "string" && typeof value.generation === "number" && value.closed === true;
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, parse, () => {
      assertScopedBarrierAllowed(this.#database, runId, input.teamId);
      const team = this.getTeam(runId, input.teamId);
      if (team.leaderAgentId !== actorAgentId) throw new FabricError("CAPABILITY_FORBIDDEN", "only the current team leader may close its barrier");
      if (team.generation !== input.expectedGeneration) throw new FabricError("STALE_TEAM_GENERATION", "team generation changed");
      if (team.state !== "active") throw new FabricError("BARRIER_PRECONDITION_FAILED", "only an active subtree may close its barrier");
      const teamIds = this.#teamSubtreeIds(runId, input.teamId);
      const teamPlaceholders = teamIds.map(() => "?").join(",");
      const taskIds = this.#database
        .prepare(`SELECT task_id FROM team_owned_tasks WHERE run_id = ? AND team_id IN (${teamPlaceholders}) ORDER BY task_id`)
        .all(runId, ...teamIds)
        .map((value) => stringField(rowOrNotFound(value, "subtree task"), "task_id"));
      const taskPlaceholders = taskIds.map(() => "?").join(",");
      const unresolved = taskIds.length === 0
        ? 0
        : numberField(
            rowOrNotFound(
              this.#database
                .prepare(`SELECT COUNT(*) AS count FROM tasks WHERE run_id = ? AND task_id IN (${taskPlaceholders}) AND state NOT IN ('complete', 'cancelled', 'degraded')`)
                .get(runId, ...taskIds),
              "subtree task count",
            ),
            "count",
          );
      const memberAgentIds = this.#database
        .prepare(`SELECT DISTINCT agent_id FROM team_members WHERE run_id = ? AND team_id IN (${teamPlaceholders}) ORDER BY agent_id`)
        .all(runId, ...teamIds)
        .map((value) => stringField(rowOrNotFound(value, "subtree member"), "agent_id"));
      const memberPlaceholders = memberAgentIds.map(() => "?").join(",");
      const activeLeases = memberAgentIds.length === 0 ? 0 : numberField(rowOrNotFound(this.#database.prepare(`SELECT COUNT(*) AS count FROM leases WHERE run_id = ? AND holder_agent_id IN (${memberPlaceholders}) AND status IN ('active', 'quarantined')`).get(runId, ...memberAgentIds), "subtree lease count"), "count");
      const openDeliveries = memberAgentIds.length === 0 ? 0 : numberField(rowOrNotFound(this.#database.prepare(`SELECT COUNT(*) AS count FROM deliveries d JOIN messages m ON m.message_id = d.message_id WHERE d.run_id = ? AND d.recipient_id IN (${memberPlaceholders}) AND m.requires_ack = 1 AND d.state NOT IN ('acknowledged', 'abandoned', 'expired')`).get(runId, ...memberAgentIds), "subtree delivery count"), "count");
      const openProviderActions = numberField(rowOrNotFound(this.#database.prepare("SELECT COUNT(*) AS count FROM provider_actions WHERE run_id = ? AND status NOT IN ('terminal', 'quarantined')").get(runId), "subtree provider action count"), "count");
      if (unresolved + activeLeases + openDeliveries + openProviderActions > 0) {
        throw new FabricError("BARRIER_PRECONDITION_FAILED", `subtree unresolved: tasks=${unresolved} leases=${activeLeases} deliveries=${openDeliveries} providerActions=${openProviderActions}`);
      }
      this.#assertTaskEvidence(runId, taskIds, true);
      const closedAt = this.#clock();
      for (const teamId of teamIds) {
        const current = this.getTeam(runId, teamId);
        this.#database
          .prepare("INSERT INTO subtree_barriers(run_id, team_id, generation, closed_at) VALUES (?, ?, ?, ?)")
          .run(runId, teamId, current.generation, closedAt);
      }
      this.#database.prepare(`UPDATE teams SET state = 'barrier-closed' WHERE run_id = ? AND team_id IN (${teamPlaceholders})`).run(runId, ...teamIds);
      return { teamId: input.teamId, generation: team.generation, closed: true };
    });
  }

  reserveBudget(
    runId: string,
    actorAgentId: string,
    input: {
      teamId: string;
      expectedTeamGeneration: number;
      parentBudgetId: string;
      budgetId: string;
      dimensions: Record<string, number>;
      commandId: string;
    },
  ): BudgetResult {
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isBudgetResult, () => {
      const team = this.getTeam(runId, input.teamId);
      if (team.generation !== input.expectedTeamGeneration) throw new FabricError("STALE_TEAM_GENERATION", "team generation changed");
      if (team.state !== "active" || team.leaderAgentId !== actorAgentId || team.budgetId !== input.parentBudgetId) {
        throw new FabricError("CAPABILITY_FORBIDDEN", "only the current leader may reserve from the team budget");
      }
      validateIntegerBudget(input.dimensions);
      for (const [unit, requested] of Object.entries(input.dimensions)) {
        const parent = rowOrNotFound(
          this.#database
            .prepare("SELECT granted, reserved, consumed, usage_unknown FROM budget_dimensions WHERE run_id = ? AND budget_id = ? AND unit_key = ?")
            .get(runId, input.parentBudgetId, unit),
          `parent budget ${unit}`,
        );
        if (numberField(parent, "usage_unknown") === 1) throw new FabricError("BUDGET_USAGE_UNKNOWN", `budget usage is unknown for ${unit}`);
        if (numberField(parent, "granted") - numberField(parent, "reserved") - numberField(parent, "consumed") < requested) {
          throw new FabricError("BUDGET_EXCEEDED", `insufficient team budget for ${unit}`);
        }
      }
      this.#database
        .prepare("INSERT INTO budgets(run_id, budget_id, parent_budget_id, team_id, owner_agent_id, state, created_at) VALUES (?, ?, ?, ?, ?, 'active', ?)")
        .run(runId, input.budgetId, input.parentBudgetId, input.teamId, actorAgentId, this.#clock());
      for (const [unit, granted] of Object.entries(input.dimensions)) {
        this.#database
          .prepare("UPDATE budget_dimensions SET reserved = reserved + ? WHERE run_id = ? AND budget_id = ? AND unit_key = ?")
          .run(granted, runId, input.parentBudgetId, unit);
        this.#database
          .prepare("INSERT INTO budget_dimensions(run_id, budget_id, unit_key, granted) VALUES (?, ?, ?, ?)")
          .run(runId, input.budgetId, unit, granted);
      }
      return this.getBudget(runId, input.budgetId);
    });
  }

  recordBudgetUsage(
    runId: string,
    actorAgentId: string,
    input: { budgetId: string; usage: Record<string, number | null>; commandId: string },
  ): BudgetResult {
    validateBudgetUnitKeys(input.usage);
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isBudgetResult, () => {
      const budget = rowOrNotFound(
        this.#database.prepare("SELECT owner_agent_id, parent_budget_id FROM budgets WHERE run_id = ? AND budget_id = ?").get(runId, input.budgetId),
        "budget",
      );
      if (stringField(budget, "owner_agent_id") !== actorAgentId) throw new FabricError("CAPABILITY_FORBIDDEN", "only the budget owner may report usage");
      const changedUnits = new Set<string>();
      for (const [unit, amount] of Object.entries(input.usage)) {
        const dimension = rowOrNotFound(
          this.#database.prepare("SELECT granted, consumed FROM budget_dimensions WHERE run_id = ? AND budget_id = ? AND unit_key = ?").get(runId, input.budgetId, unit),
          `budget dimension ${unit}`,
        );
        if (amount === null) {
          this.#database.prepare("UPDATE budget_dimensions SET direct_usage_unknown = 1, usage_unknown = 1 WHERE run_id = ? AND budget_id = ? AND unit_key = ?").run(runId, input.budgetId, unit);
        } else {
          if (
            !Number.isInteger(amount) ||
            amount < numberField(dimension, "consumed") ||
            amount > numberField(dimension, "granted")
          ) {
            throw new FabricError("BUDGET_EXCEEDED", `invalid consumption for ${unit}`);
          }
          this.#database.prepare("UPDATE budget_dimensions SET consumed = ? WHERE run_id = ? AND budget_id = ? AND unit_key = ?").run(amount, runId, input.budgetId, unit);
        }
        changedUnits.add(unit);
      }
      for (const unit of changedUnits) this.#refreshBudgetUnknownAncestors(runId, input.budgetId, unit);
      return this.getBudget(runId, input.budgetId);
    });
  }

  reconcileBudgetUsage(
    runId: string,
    actorAgentId: string,
    input: { budgetId: string; consumed: Record<string, number>; commandId: string },
  ): BudgetResult {
    validateBudgetUnitKeys(input.consumed);
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isBudgetResult, () => {
      this.#assertChair(runId, actorAgentId);
      for (const [unit, amount] of Object.entries(input.consumed)) {
        const dimension = rowOrNotFound(
          this.#database.prepare("SELECT granted FROM budget_dimensions WHERE run_id = ? AND budget_id = ? AND unit_key = ?").get(runId, input.budgetId, unit),
          `budget dimension ${unit}`,
        );
        if (!Number.isInteger(amount) || amount < 0 || amount > numberField(dimension, "granted")) throw new FabricError("BUDGET_EXCEEDED", `invalid reconciliation for ${unit}`);
        this.#database
          .prepare("UPDATE budget_dimensions SET consumed = ?, direct_usage_unknown = 0 WHERE run_id = ? AND budget_id = ? AND unit_key = ?")
          .run(amount, runId, input.budgetId, unit);
        this.#refreshBudgetUnknownAncestors(runId, input.budgetId, unit);
      }
      return this.getBudget(runId, input.budgetId);
    });
  }

  releaseBudget(runId: string, actorAgentId: string, input: { budgetId: string; commandId: string }): BudgetResult {
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isBudgetResult, () => {
      const budget = rowOrNotFound(
        this.#database.prepare("SELECT owner_agent_id, parent_budget_id, state FROM budgets WHERE run_id = ? AND budget_id = ?").get(runId, input.budgetId),
        "budget",
      );
      if (stringField(budget, "owner_agent_id") !== actorAgentId) throw new FabricError("CAPABILITY_FORBIDDEN", "only the budget owner may release it");
      if (stringField(budget, "state") === "released") return this.getBudget(runId, input.budgetId);
      const dimensions = this.getBudget(runId, input.budgetId).dimensions;
      if (Object.values(dimensions).some((dimension) => dimension.usageUnknown)) {
        this.#database.prepare("UPDATE budgets SET state = 'usage-unknown' WHERE run_id = ? AND budget_id = ?").run(runId, input.budgetId);
        return this.getBudget(runId, input.budgetId);
      }
      const returned: Record<string, number> = {};
      const parentBudgetId = budget.parent_budget_id;
      for (const [unit, dimension] of Object.entries(dimensions)) {
        const unused = dimension.granted - dimension.consumed;
        returned[unit] = unused;
        if (typeof parentBudgetId === "string") {
          this.#database
            .prepare("UPDATE budget_dimensions SET reserved = reserved - ?, consumed = consumed + ? WHERE run_id = ? AND budget_id = ? AND unit_key = ?")
            .run(dimension.granted, dimension.consumed, runId, parentBudgetId, unit);
        }
      }
      this.#database.prepare("UPDATE budgets SET state = 'released', returned_json = ? WHERE run_id = ? AND budget_id = ?").run(
        canonicalJson(returned),
        runId,
        input.budgetId,
      );
      return this.getBudget(runId, input.budgetId);
    });
  }

  getBudget(runId: string, budgetId: string): BudgetResult {
    const budget = rowOrNotFound(
      this.#database.prepare("SELECT parent_budget_id, state, returned_json FROM budgets WHERE run_id = ? AND budget_id = ?").get(runId, budgetId),
      "budget",
    );
    const parentValue = budget.parent_budget_id;
    if (typeof parentValue !== "string" && parentValue !== null) throw new Error("stored parent budget is invalid");
    const state = stringField(budget, "state");
    if (state !== "active" && state !== "usage-unknown" && state !== "released") throw new Error("stored budget state is invalid");
    const dimensions: Record<string, BudgetDimensionResult> = {};
    for (const value of this.#database
      .prepare("SELECT unit_key, granted, reserved, consumed, usage_unknown FROM budget_dimensions WHERE run_id = ? AND budget_id = ? ORDER BY unit_key")
      .all(runId, budgetId)) {
      const row = rowOrNotFound(value, "budget dimension");
      const granted = numberField(row, "granted");
      const reserved = numberField(row, "reserved");
      const consumed = numberField(row, "consumed");
      dimensions[stringField(row, "unit_key")] = {
        granted,
        reserved,
        consumed,
        available: granted - reserved - consumed,
        usageUnknown: numberField(row, "usage_unknown") === 1,
      };
    }
    const returnedValue: unknown = JSON.parse(stringField(budget, "returned_json"));
    if (!isNumberRecord(returnedValue)) throw new Error("stored returned budget is invalid");
    return { budgetId, parentBudgetId: parentValue, state, dimensions, returned: returnedValue };
  }

  #syncBudgetState(runId: string, budgetId: string): void {
    const unknown = numberField(
      rowOrNotFound(
        this.#database.prepare("SELECT COUNT(*) AS count FROM budget_dimensions WHERE run_id = ? AND budget_id = ? AND usage_unknown = 1").get(runId, budgetId),
        "unknown usage count",
      ),
      "count",
    );
    this.#database.prepare("UPDATE budgets SET state = ? WHERE run_id = ? AND budget_id = ? AND state != 'released'").run(
      unknown > 0 ? "usage-unknown" : "active",
      runId,
      budgetId,
    );
  }

  #refreshBudgetUnknownAncestors(runId: string, budgetId: string, unit: string): void {
    let currentBudgetId: string | null = budgetId;
    while (currentBudgetId !== null) {
      this.#database.prepare(`
        UPDATE budget_dimensions
           SET usage_unknown = CASE
             WHEN direct_usage_unknown = 1 OR EXISTS (
               SELECT 1
                 FROM budgets child
                 JOIN budget_dimensions child_dimension
                   ON child_dimension.run_id = child.run_id
                  AND child_dimension.budget_id = child.budget_id
                WHERE child.run_id = budget_dimensions.run_id
                  AND child.parent_budget_id = budget_dimensions.budget_id
                  AND child_dimension.unit_key = budget_dimensions.unit_key
                  AND child_dimension.usage_unknown = 1
             ) THEN 1 ELSE 0 END
         WHERE run_id = ? AND budget_id = ? AND unit_key = ?
      `).run(runId, currentBudgetId, unit);
      this.#syncBudgetState(runId, currentBudgetId);
      const current = rowOrNotFound(
        this.#database.prepare("SELECT parent_budget_id FROM budgets WHERE run_id = ? AND budget_id = ?").get(runId, currentBudgetId),
        "budget ancestor",
      );
      const parent = current.parent_budget_id;
      if (typeof parent !== "string" && parent !== null) throw new Error("stored parent budget is invalid");
      currentBudgetId = parent;
    }
  }

  publishArtifact(
    runId: string,
    actorAgentId: string,
    input: { taskId?: string; relativePath: string; sha256: string; commandId: string },
  ): ArtifactResult {
    if (!/^[0-9a-f]{64}$/u.test(input.sha256)) {
      throw new FabricError("ARTIFACT_DIGEST_INVALID", "artifact SHA-256 must be 64 lowercase hex characters");
    }
    if (
      input.relativePath.length === 0 ||
      isAbsolute(input.relativePath) ||
      input.relativePath.split(/[\\/]/u).includes("..")
    ) {
      throw new FabricError("ARTIFACT_PATH_FORBIDDEN", "artifact path must be relative and traversal-free");
    }
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isArtifactResult, () => {
      const taskId = resolveTaskBindingForActiveWork(this.#database, runId, actorAgentId, input.taskId);
      if (taskId !== undefined) {
        const bound = this.#database.prepare(`
          SELECT 1 FROM tasks task WHERE task.run_id=? AND task.task_id=? AND (
            task.owner_agent_id=? OR EXISTS (
              SELECT 1 FROM task_participants participant
               WHERE participant.run_id=task.run_id AND participant.task_id=task.task_id
                 AND participant.agent_id=?
            )
          )
        `).get(runId, taskId, actorAgentId, actorAgentId);
        if (!isRow(bound)) throw new FabricError("CAPABILITY_FORBIDDEN", "artifact task is outside the actor task scope");
      }
      const registered = this.#artifactRegistry.registerAgentEvidence({
        runId,
        agentId: actorAgentId,
        taskId: taskId ?? null,
        requestedSourceKind: "run-file",
        evidenceKind: "artifact",
        relativePath: input.relativePath,
        digest: input.sha256,
        verifyBytes: false,
      });
      const result: ArtifactResult = {
        artifactId: registered.evidenceId,
        relativePath: input.relativePath,
        sha256: input.sha256,
      };
      this.#event(runId, "artifact-published", actorAgentId, result);
      return result;
    });
  }

  publishEvidence(
    runId: string,
    actorAgentId: string,
    input: EvidencePublishRequest,
  ): EvidenceArtifactRegistration {
    if (input.coordinationRunId !== runId) {
      throw new FabricError("CAPABILITY_FORBIDDEN", "evidence run binding differs from the authenticated run");
    }
    const root = resolveRunArtifactRoot(this.#database, runId);
    if (input.projectSessionId !== root.projectSessionId) {
      throw new FabricError("CAPABILITY_FORBIDDEN", "evidence session binding differs from the authenticated session");
    }
    return this.#commandJournal.execute(
      runId,
      actorAgentId,
      input.commandId,
      input,
      isEvidenceArtifactRegistration,
      () => {
        const taskId = resolveTaskBindingForActiveWork(this.#database, runId, actorAgentId, input.taskId);
        const registered = this.#artifactRegistry.registerAgentEvidence({
          runId,
          agentId: actorAgentId,
          taskId: taskId ?? null,
          requestedSourceKind: input.requestedSourceKind,
          evidenceKind: input.evidenceKind,
          relativePath: input.relativePath,
          digest: input.sourceDigest,
        });
        if (registered.projectSessionId === null || registered.coordinationRunId === null) {
          throw new Error("agent evidence registration lost its session/run binding");
        }
        return {
          evidenceId: registered.evidenceId,
          evidenceRevision: registered.evidenceRevision,
          projectId: registered.projectId as never,
          projectSessionId: registered.projectSessionId as never,
          coordinationRunId: registered.coordinationRunId as never,
          taskId: registered.taskId as never,
          sourceKind: registered.sourceKind as "project-file" | "run-file",
          evidenceKind: registered.evidenceKind,
          artifactRef: registered.artifactRef as never,
          publisherKind: "agent",
          publisherRef: registered.publisherRef as never,
          createdAt: new Date(registered.createdAt).toISOString() as never,
        };
      },
    );
  }

  closeBarrier(
    runId: string,
    actorAgentId: string,
    input: { scope: "run" | "stage"; stageId?: string; commandId: string },
  ): BarrierResult {
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, isBarrierResult, () => {
      this.#assertChair(runId, actorAgentId);
      const stageId = input.scope === "stage" ? input.stageId ?? "default" : "";
      assertScopedBarrierAllowed(this.#database, runId, `${runId}:${input.scope}:${stageId}`);
      const unreconciled = numberField(
        rowOrNotFound(
          this.#database
            .prepare("SELECT COUNT(*) AS count FROM agents WHERE run_id = ? AND lifecycle = 'context-unreconciled'")
            .get(runId),
          "unreconciled context count",
        ),
        "count",
      );
      if (unreconciled > 0) {
        throw new FabricError("CONTEXT_UNRECONCILED", "a provider context is not reconciled");
      }
      const openTasks = numberField(
        rowOrNotFound(
          this.#database
            .prepare("SELECT COUNT(*) AS count FROM tasks WHERE run_id = ? AND state NOT IN ('complete', 'cancelled', 'degraded')")
            .get(runId),
          "task count",
        ),
        "count",
      );
      const openLeases = numberField(
        rowOrNotFound(
          this.#database
            .prepare("SELECT COUNT(*) AS count FROM leases WHERE run_id = ? AND status IN ('active', 'quarantined')")
            .get(runId),
          "lease count",
        ),
        "count",
      );
      const openDeliveries = numberField(
        rowOrNotFound(
          this.#database
            .prepare(
              "SELECT COUNT(*) AS count FROM deliveries d JOIN messages m ON m.message_id = d.message_id WHERE d.run_id = ? AND m.requires_ack = 1 AND d.state NOT IN ('acknowledged', 'abandoned', 'expired')",
            )
            .get(runId),
          "delivery count",
        ),
        "count",
      );
      const openProviderActions = numberField(
        rowOrNotFound(this.#database.prepare("SELECT COUNT(*) AS count FROM provider_actions WHERE run_id = ? AND status NOT IN ('terminal', 'quarantined')").get(runId), "provider action count"),
        "count",
      );
      if (openTasks > 0 || openLeases > 0 || openDeliveries > 0 || openProviderActions > 0) {
        throw new FabricError(
          "BARRIER_PRECONDITION_FAILED",
          `barrier has unresolved work: tasks=${openTasks} leases=${openLeases} deliveries=${openDeliveries} providerActions=${openProviderActions}`,
        );
      }
      const taskIds = this.#database
        .prepare("SELECT task_id FROM tasks WHERE run_id = ? ORDER BY task_id")
        .all(runId)
        .map((value) => stringField(rowOrNotFound(value, "barrier task"), "task_id"));
      this.#assertTaskEvidence(runId, taskIds, input.scope === "stage");
      const receipt = this.exportReceipt(runId, actorAgentId, `${input.commandId}:receipt`);
      this.#database
        .prepare(
          "INSERT INTO barriers(run_id, scope, stage_id, state, closed_at, receipt_sha256) VALUES (?, ?, ?, 'closed', ?, ?) ON CONFLICT(run_id, scope, stage_id) DO UPDATE SET state = 'closed', closed_at = excluded.closed_at, receipt_sha256 = excluded.receipt_sha256",
        )
        .run(runId, input.scope, stageId, this.#clock(), receipt.sha256);
      const result: BarrierResult = { scope: input.scope, closed: true, receipt };
      this.#event(runId, "barrier-closed", actorAgentId, { scope: input.scope, stageId, receipt });
      return result;
    });
  }

  #assertTaskEvidence(runId: string, taskIds: string[], requireHandoff: boolean): void {
    if (taskIds.length === 0) return;
    const placeholders = taskIds.map(() => "?").join(",");
    const count = (sql: string): number =>
      numberField(rowOrNotFound(this.#database.prepare(sql).get(runId, ...taskIds), "barrier evidence count"), "count");
    const missingArtifacts = count(
      `SELECT COUNT(*) AS count FROM task_expected_artifacts e WHERE e.run_id = ? AND e.task_id IN (${placeholders}) AND NOT EXISTS (SELECT 1 FROM artifacts a WHERE a.run_id = e.run_id AND a.task_id = e.task_id AND a.relative_path = e.relative_path)`,
    );
    const failedChecks = count(
      `SELECT COUNT(*) AS count FROM task_objective_checks WHERE run_id = ? AND task_id IN (${placeholders}) AND status != 'pass'`,
    );
    const unresolvedGates = count(
      `SELECT COUNT(DISTINCT g.gate_id) AS count
         FROM scoped_gates g
         JOIN scoped_gate_tasks t ON t.gate_id=g.gate_id
        WHERE g.coordination_run_id=? AND t.task_id IN (${placeholders})
          AND g.status IN ('pending','deferred')`,
    );
    const missingCheckpoints = count(
      `SELECT COUNT(*) AS count FROM tasks t JOIN agents a ON a.run_id = t.run_id AND a.agent_id = t.owner_agent_id WHERE t.run_id = ? AND t.task_id IN (${placeholders}) AND a.provider_session_ref IS NOT NULL AND NOT EXISTS (SELECT 1 FROM lifecycle_checkpoints c WHERE c.run_id = t.run_id AND c.task_id = t.task_id AND c.task_revision = t.revision)`,
    );
    const missingHandoffs = requireHandoff
      ? count(
          `SELECT COUNT(*) AS count FROM tasks t WHERE t.run_id = ? AND t.task_id IN (${placeholders}) AND NOT EXISTS (SELECT 1 FROM task_handoff_acknowledgements h WHERE h.run_id = t.run_id AND h.task_id = t.task_id AND h.task_revision = t.revision AND h.owner_lease_generation = t.owner_lease_generation AND h.acknowledged_by = h.intended_next_owner_agent_id)`,
        )
      : 0;
    if (missingArtifacts + failedChecks + unresolvedGates + missingCheckpoints + missingHandoffs > 0) {
      throw new FabricError(
        "BARRIER_PRECONDITION_FAILED",
        `barrier evidence incomplete: artifacts=${missingArtifacts} checks=${failedChecks} gates=${unresolvedGates} checkpoints=${missingCheckpoints} handoffs=${missingHandoffs}`,
      );
    }
  }

  whoami(runId: string, agentId: string, tokenHash: string): CallerIdentity { return readCallerIdentity(this.#database, runId, agentId, tokenHash); }
  getRunStatus(authenticatedRunId: string, requestedRunId: string): {
    runId: string;
    chairAgentId: string;
    barrier: { state: "open" | "closed" };
    counts: {
      agents: number;
      tasks: number;
      tasksTerminal: number;
      messages: number;
      deliveriesUnacknowledged: number;
      leasesActive: number;
    };
  } {
    if (authenticatedRunId !== requestedRunId) {
      throw new FabricError("CAPABILITY_FORBIDDEN", "cross-run reads are forbidden");
    }
    const run = rowOrNotFound(
      this.#database.prepare("SELECT chair_agent_id FROM runs WHERE run_id = ?").get(requestedRunId),
      "run",
    );
    const count = (sql: string): number =>
      numberField(rowOrNotFound(this.#database.prepare(sql).get(requestedRunId), "count"), "count");
    const barrier = this.#database
      .prepare("SELECT 1 FROM barriers WHERE run_id = ? AND scope = 'run' AND state = 'closed'")
      .get(requestedRunId);
    return {
      runId: requestedRunId,
      chairAgentId: stringField(run, "chair_agent_id"),
      barrier: { state: isRow(barrier) ? "closed" : "open" },
      counts: {
        agents: count("SELECT COUNT(*) AS count FROM agents WHERE run_id = ?"),
        tasks: count("SELECT COUNT(*) AS count FROM tasks WHERE run_id = ?"),
        tasksTerminal: count("SELECT COUNT(*) AS count FROM tasks WHERE run_id = ? AND state IN ('complete', 'cancelled', 'degraded')"),
        messages: count("SELECT COUNT(*) AS count FROM messages WHERE run_id = ?"),
        deliveriesUnacknowledged: count("SELECT COUNT(*) AS count FROM deliveries WHERE run_id = ? AND state NOT IN ('acknowledged', 'abandoned', 'expired')"),
        leasesActive: count("SELECT COUNT(*) AS count FROM leases WHERE run_id = ? AND status = 'active'"),
      },
    };
  }

  eventsAfter(runId: string, input: { cursor: number; limit: number }): EventsAfterResult {
    if (!Number.isSafeInteger(input.cursor) || input.cursor < 0) {
      throw new TypeError("event cursor must be a non-negative safe integer");
    }
    if (!Number.isSafeInteger(input.limit) || input.limit < 1 || input.limit > 100) {
      throw new TypeError("event read limit must be an integer from 1 to 100");
    }
    const events = this.#database
      .prepare("SELECT s.sequence AS cursor, e.event_id, e.type, e.actor_agent_id, e.created_at, e.payload_json FROM observer_event_sequence s JOIN events e ON e.event_id = s.event_id WHERE e.run_id = ? AND s.sequence > ? ORDER BY s.sequence LIMIT ?")
      .all(runId, input.cursor, input.limit)
      .map((value) => {
        const row = rowOrNotFound(value, "event");
        const actor = row.actor_agent_id;
        if (actor !== null && typeof actor !== "string") throw new Error("stored event actor is invalid");
        const type = stringField(row, "type");
        let summary = actor === null ? type : `${type} by ${actor}`;
        if (type === "message-persisted" && actor !== null && typeof row.payload_json === "string") {
          const payload: unknown = JSON.parse(row.payload_json);
          if (isRow(payload) && typeof payload.messageId === "string" && isStringArray(payload.recipients)) {
            const message = rowOrNotFound(
              this.#database.prepare("SELECT kind, body FROM messages WHERE run_id = ? AND message_id = ?").get(runId, payload.messageId),
              "observer message",
            );
            const preview = renderSafePreview(stringField(message, "body"), 160);
            summary = `${stringField(message, "kind")} ${actor} → ${payload.recipients.join(", ")}: ${preview}`;
          }
        }
        return {
          cursor: numberField(row, "cursor"),
          eventId: stringField(row, "event_id"),
          type,
          actorAgentId: actor,
          createdAt: numberField(row, "created_at"),
          summary,
        };
      });
    return { events, nextCursor: events.at(-1)?.cursor ?? input.cursor };
  }

  listTasks(runId: string, requesterId: string): { tasks: TaskResult[] } {
    const tasks = this.#database
      .prepare("SELECT task_id FROM tasks WHERE run_id = ? ORDER BY task_id")
      .all(runId)
      .map((value) => stringField(rowOrNotFound(value, "task"), "task_id"))
      .filter((taskId) => this.#readPolicy.canReadTask(runId, requesterId, taskId))
      .map((taskId) => this.getTask(runId, taskId));
    return { tasks };
  }

  listAgents(runId: string, requesterId: string): { agents: Array<{
    agentId: string;
    parentAgentId: string | null;
    lifecycle: string;
    bridgeState: "active" | "none" | "lost";
    bridgeGeneration: number;
  }> } {
    const agents = this.#database
      .prepare(`
        SELECT a.agent_id, a.parent_agent_id, a.lifecycle,
               COALESCE(b.bridge_state, 'none') AS bridge_state,
               COALESCE(b.bridge_generation, 1) AS bridge_generation
          FROM agents a
          LEFT JOIN agent_bridge_state b ON b.run_id=a.run_id AND b.agent_id=a.agent_id
         WHERE a.run_id = ? ORDER BY a.agent_id
      `)
      .all(runId)
      .map((value) => {
        const row = rowOrNotFound(value, "agent");
        const parentValue = row.parent_agent_id;
        if (parentValue !== null && typeof parentValue !== "string") {
          throw new Error("stored parent agent is invalid");
        }
        const bridgeState = stringField(row, "bridge_state");
        if (bridgeState !== "active" && bridgeState !== "none" && bridgeState !== "lost") {
          throw new Error("stored agent bridge state is invalid");
        }
        return {
          agentId: stringField(row, "agent_id"),
          parentAgentId: parentValue,
          lifecycle: stringField(row, "lifecycle"),
          bridgeState: bridgeState as "active" | "none" | "lost",
          bridgeGeneration: numberField(row, "bridge_generation"),
        };
      })
      .filter((agent) => this.#readPolicy.canReadAgent(runId, requesterId, agent.agentId));
    return { agents };
  }

  listReceipts(runId: string, requesterId: string): { receipts: Array<{ relativePath: string; sha256: string; exportedAt: number }> } {
    if (!this.#readPolicy.isChair(runId, requesterId)) return { receipts: [] };
    const receipts = this.#database
      .prepare("SELECT relative_path, sha256, exported_at FROM receipt_exports WHERE run_id = ? ORDER BY exported_at, relative_path")
      .all(runId)
      .map((value) => {
        const row = rowOrNotFound(value, "receipt");
        return {
          relativePath: stringField(row, "relative_path"),
          sha256: stringField(row, "sha256"),
          exportedAt: numberField(row, "exported_at"),
        };
      });
    return { receipts };
  }

  exportReceipt(runId: string, actorAgentId: string, commandId: string): ReceiptResult {
    const payload = { operation: "export-receipt" };
    const replay = this.#commandJournal.read(runId, actorAgentId, commandId, payload, isReceiptResult);
    if (replay !== undefined) return replay;
    const run = rowOrNotFound(
      this.#database.prepare("SELECT chair_agent_id FROM runs WHERE run_id = ?").get(runId),
      "run",
    );
    if (stringField(run, "chair_agent_id") !== actorAgentId) {
      throw new FabricError("CAPABILITY_FORBIDDEN", "only the chair may export the receipt");
    }
    const artifactRoot = resolveRunArtifactRoot(this.#database, runId);
    const directoryValue = artifactRoot.artifactRoot;
    if (directoryValue === null) {
      throw new FabricError("NOT_FOUND", "run has no project receipt directory");
    }
    const receipt = this.#database.transaction(() => projectFabricReceipt(this.#database, runId))();
    assertFabricReceiptSchema(receipt);
    const bytes = `${JSON.stringify(receipt, null, 2)}\n`;
    mkdirSync(directoryValue, { recursive: true, mode: 0o700 });
    const digest = sha256(bytes);
    const relativePath = `fabric-receipt-${digest}.json`;
    writeFileSync(join(directoryValue, relativePath), bytes, { encoding: "utf8", mode: 0o600 });
    writeFileSync(join(directoryValue, "fabric-receipt.json"), bytes, { encoding: "utf8", mode: 0o600 });
    const result: ReceiptResult = { relativePath, schemaVersion: 2, sha256: digest };
    this.#database.transaction(() => {
      this.#database
        .prepare(
          "INSERT OR IGNORE INTO receipt_exports(run_id, relative_path, sha256, exported_at) VALUES (?, ?, ?, ?)",
        )
        .run(runId, relativePath, digest, this.#clock());
      this.#artifactRegistry.register({
        projectId: artifactRoot.projectId,
        projectSessionId: artifactRoot.projectSessionId,
        runId,
        taskId: null,
        publisherKind: "fabric",
        publisherRef: "fabric-receipt-export",
        publisherAgentId: null,
        sourceKind: artifactRoot.projectRelativeDirectory === "." ? "project-file" : "run-file",
        evidenceKind: "receipt",
        relativePath,
        digest,
      });
      this.#commandJournal.write(runId, actorAgentId, commandId, payload, result);
    })();
    return result;
  }

  #adapter(adapterId: string): NonNullable<FabricOpenOptions["adapters"]>[string] {
    const adapter = this.#adapters[adapterId];
    if (adapter === undefined) throw new FabricError("ADAPTER_DISABLED", `adapter is not activated: ${adapterId}`);
    return adapter;
  }

  #adapterIdForAgent(runId: string, agentId: string): string {
    const binding = rowOrNotFound(
      this.#database.prepare("SELECT adapter_id FROM agent_adapter_bindings WHERE run_id = ? AND agent_id = ?").get(runId, agentId),
      "agent adapter binding",
    );
    return stringField(binding, "adapter_id");
  }

}
