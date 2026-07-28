import {
  parseLaunchAdapterOutcomeV1,
  parseLaunchPacketV1,
  parseLaunchProviderActionJournalRefV1,
  parseLaunchResourcePlanV1,
  parseArtifactRef,
  parseAuthorityEnvelopeV2,
  parseIdentifier,
  type AuthorityEnvelopeV2,
  type LaunchProviderActionJournalRefV1,
  type McpSeatProvisioningDescriptorV1,
  type ProjectSessionLaunchIntent,
  type ArtifactRef,
  type Sha256Digest,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";
import { constants, closeSync, fstatSync, lstatSync, openSync, readFileSync, realpathSync } from "node:fs";
import { isAbsolute, relative, resolve, sep } from "node:path";

import { ProjectFabricCoreError } from "./contracts.js";
import { exactDigest, nonEmptyString, protocol } from "./provider-agent-custody.js";
import {
  canonicalJson,
  integer,
  isRow,
  row,
  sha256,
  text,
  type Row,
} from "../persistence/row-codec.js";

/**
 * Byte-moved from `launch-custody.ts` (issue #354, S4e, plan §2 "S4e"): the project-session
 * launch family's contracts, artifact parsing and shared pure helpers. This module holds no
 * database mutation; it is imported by `launch-service.ts` and `launch-settlement.ts`.
 */

export type Digest = Sha256Digest;
export type ArtifactBinding = ArtifactRef;
export type ResourceAmounts = Readonly<Record<string, number>>;

export type LaunchCustodyIntent = ProjectSessionLaunchIntent;

export type NormalisedLaunchAuthority = AuthorityEnvelopeV2;

const UNIT_KEY = /^[a-z][a-z0-9_.:-]{0,63}$/u;
const MAX_ARTIFACT_BYTES = 1024 * 1024;

export const CLOSED_PREFLIGHT_FAILURE_CODES = new Set([
  "ACTION_INPUT_CONFLICT",
  "CAPABILITY_EXPIRED",
  "CAPABILITY_FORBIDDEN",
  "DEDUPE_CONFLICT",
  "PROTOCOL_INVALID",
  "WRONG_PROJECT",
]);

export function isDeterministicClosedPreflightFailure(error: unknown): boolean {
  let current = error;
  for (let depth = 0; depth < 4; depth += 1) {
    if (current === null || typeof current !== "object") return false;
    const record = current as { code?: unknown; cause?: unknown };
    if (typeof record.code === "string") return CLOSED_PREFLIGHT_FAILURE_CODES.has(record.code);
    if (record.cause === undefined || record.cause === current) return false;
    current = record.cause;
  }
  return false;
}

export type LaunchAdapterContract = Readonly<{
  schemaVersion: 1;
  method: "launch_chair";
  oneUse: true;
  secretTransport: "private-environment";
  environment: Readonly<{
    capability: "AGENT_FABRIC_CAPABILITY";
    socketPath: "AGENT_FABRIC_SOCKET_PATH";
    attestationChallenge: "AGENT_FABRIC_ATTESTATION_CHALLENGE";
  }>;
  inputSchemaId: string;
  publicPayloadSchema: Record<string, unknown>;
  noEffectProofSchemas: Readonly<Record<string, Record<string, unknown>>>;
  attestation: Readonly<{
    method: "provider-session-random-challenge-v1";
    bridgeContract: "agent-fabric-session-bridge-v1";
    origin: "provider-session-tool-call";
    oneUse: true;
    bridgeLifetime: "provider-session";
    digestAlgorithm: "sha256";
    nativeAttribution:
      | "claude-sdk-assistant-request-tool-use-v1"
      | "codex-app-server-thread-turn-call-v1";
  }>;
}>;

export function parseLaunchAdapterContract(value: unknown): LaunchAdapterContract {
  const contract = exactRecord(value, "chairLaunch", [
    "schemaVersion", "method", "oneUse", "secretTransport", "environment",
    "inputSchemaId", "publicPayloadSchema", "noEffectProofSchemas", "attestation",
  ]);
  if (
    contract.schemaVersion !== 1 ||
    contract.method !== "launch_chair" ||
    contract.oneUse !== true ||
    contract.secretTransport !== "private-environment"
  ) protocol("chairLaunch contract version or private handoff semantics are invalid");
  const environment = exactRecord(contract.environment, "chairLaunch.environment", [
    "capability", "socketPath", "attestationChallenge",
  ]);
  if (
    environment.capability !== "AGENT_FABRIC_CAPABILITY" ||
    environment.socketPath !== "AGENT_FABRIC_SOCKET_PATH" ||
    environment.attestationChallenge !== "AGENT_FABRIC_ATTESTATION_CHALLENGE"
  ) protocol("chairLaunch environment contract is invalid");
  if (!isRow(contract.publicPayloadSchema)) protocol("chairLaunch public payload schema must be an object");
  if (!isRow(contract.noEffectProofSchemas)) protocol("chairLaunch no-effect proof schemas must be an object");
  const noEffectProofSchemas: Record<string, Record<string, unknown>> = {};
  for (const [schemaId, schema] of Object.entries(contract.noEffectProofSchemas)) {
    nonEmptyString(schemaId, "chairLaunch no-effect proof schema ID");
    if (!isRow(schema)) protocol(`chairLaunch no-effect proof schema ${schemaId} must be an object`);
    noEffectProofSchemas[schemaId] = schema;
  }
  const attestation = exactRecord(contract.attestation, "chairLaunch.attestation", [
    "method", "bridgeContract", "origin", "oneUse", "bridgeLifetime", "digestAlgorithm", "nativeAttribution",
  ]);
  if (
    attestation.method !== "provider-session-random-challenge-v1" ||
    attestation.bridgeContract !== "agent-fabric-session-bridge-v1" ||
    attestation.origin !== "provider-session-tool-call" ||
    attestation.oneUse !== true ||
    attestation.bridgeLifetime !== "provider-session" ||
    attestation.digestAlgorithm !== "sha256" ||
    (
      attestation.nativeAttribution !== "claude-sdk-assistant-request-tool-use-v1" &&
      attestation.nativeAttribution !== "codex-app-server-thread-turn-call-v1"
    )
  ) protocol("chairLaunch attestation contract is invalid");
  return {
    schemaVersion: 1,
    method: "launch_chair",
    oneUse: true,
    secretTransport: "private-environment",
    environment: {
      capability: "AGENT_FABRIC_CAPABILITY",
      socketPath: "AGENT_FABRIC_SOCKET_PATH",
      attestationChallenge: "AGENT_FABRIC_ATTESTATION_CHALLENGE",
    },
    inputSchemaId: nonEmptyString(contract.inputSchemaId, "chairLaunch.inputSchemaId"),
    publicPayloadSchema: contract.publicPayloadSchema,
    noEffectProofSchemas,
    attestation: {
      method: "provider-session-random-challenge-v1",
      bridgeContract: "agent-fabric-session-bridge-v1",
      origin: "provider-session-tool-call",
      oneUse: true,
      bridgeLifetime: "provider-session",
      digestAlgorithm: "sha256",
      nativeAttribution: attestation.nativeAttribution,
    },
  };
}

export type LaunchPacket = Readonly<{
  schemaVersion: 1;
  projectId: string;
  projectSessionId: string;
  runId: string;
  chairAgentId: string;
  projectRunDirectory: string;
  topologyMode: "coordinated" | "independent";
  budgetRef: string;
  resourcePlanRef: ArtifactBinding;
  chairAuthority: NormalisedLaunchAuthority;
  provider: Readonly<{
    adapterId: string;
    actionId: string;
    contractDigest: Digest;
    inputSchemaId: string;
    input: Record<string, unknown>;
  }>;
}>;

export type LaunchResourcePlan = Readonly<{
  schemaVersion: 1;
  projectId: string;
  projectSessionId: string;
  runId: string;
  budgetRef: string;
  scopes: Readonly<{
    project: Readonly<{ scopeId: string; limits: ResourceAmounts }>;
    projectSession: Readonly<{ scopeId: string; limits: ResourceAmounts }>;
    coordinationRun: Readonly<{ scopeId: string; limits: ResourceAmounts }>;
  }>;
  launchReservation: Readonly<{ amounts: ResourceAmounts }>;
}>;

export type LaunchInspection = Readonly<{
  intent: LaunchCustodyIntent;
  canonicalProjectRoot: string;
  packet: LaunchPacket;
  plan: LaunchResourcePlan;
  launchBindingDigest: Digest;
  inspectedProjectRevision: number;
  inspectedSessionRevision: number;
  inspectedSessionGeneration: number;
}>;

export type LaunchDispatchHandle = Readonly<{
  schemaVersion: 1;
  providerAdapterId: string;
  providerActionId: string;
  providerContractDigest: Digest;
  publicPayload: Record<string, unknown>;
  capability: string;
  socketPath: string;
  attestationChallenge: string;
  attestationChallengeDigest: Digest;
  expectedPrincipal: Readonly<{
    agentId: string;
    projectSessionId: string;
    runId: string;
    principalGeneration: number;
  }>;
}>;

export type RetainedChairBridge = Readonly<{
  projectSessionId: string;
  runId: string;
  agentId: string;
  principalGeneration: number;
  adapterId: string;
  actionId: string;
  providerSessionRef: string;
  providerSessionGeneration: number;
  bridgeGeneration: number;
}>;

export type ChairBridgeLossObservation = RetainedChairBridge & Readonly<{ reason: string }>;

export type LaunchOutcomeBase = Readonly<{
  schemaVersion: 1;
  providerAdapterId: string;
  providerActionId: string;
  providerContractDigest: Digest;
  observationKind: "dispatch-return" | "lookup";
  observedAt: string;
}>;

export type LaunchTerminalSuccess = LaunchOutcomeBase & Readonly<{
  outcome: Readonly<{
    kind: "terminal-success";
    providerSessionRef: string;
    providerSessionGeneration: number;
    effectDigest: Digest;
    resourceUsage: Readonly<Record<string, number | "unknown">>;
  }>;
}>;

export type LaunchTerminalNoEffect = LaunchOutcomeBase & Readonly<{
  outcome: Readonly<{
    kind: "terminal-no-effect";
    failureCode: string;
    noEffectProof: Readonly<{ schemaId: string; proof: Record<string, unknown>; digest: Digest }>;
  }>;
}>;

export type LaunchAmbiguous = LaunchOutcomeBase & Readonly<{
  outcome: Readonly<{
    kind: "ambiguous";
    reasonCode:
      | "absent"
      | "transport-error"
      | "adapter-error"
      | "malformed"
      | "incomplete"
      | "conflict"
      | "missing-resume-reference";
    evidenceDigest: Digest | null;
  }>;
}>;

export type LaunchAdapterOutcome = LaunchTerminalSuccess | LaunchTerminalNoEffect | LaunchAmbiguous;

export type LaunchRecoveryResult = Readonly<{
  preparedFailed: number;
  lookedUp: number;
  activated: number;
  failed: number;
  ambiguous: number;
  recoveryRequired: number;
}>;

export function forbidden(message: string): never {
  throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", message);
}

export function stale(message: string): never {
  throw new ProjectFabricCoreError("STALE_REVISION", message);
}

export function exactRecord(
  value: unknown,
  label: string,
  required: readonly string[],
  optional: readonly string[] = [],
): Row {
  if (!isRow(value)) protocol(`${label} must be an object`);
  const allowed = new Set([...required, ...optional]);
  const unknown = Object.keys(value).find((key) => !allowed.has(key));
  if (unknown !== undefined) protocol(`${label} contains unknown field ${unknown}`);
  const missing = required.find((key) => !(key in value));
  if (missing !== undefined) protocol(`${label} is missing ${missing}`);
  return value;
}

export function exactArtifact(value: unknown, label: string): ArtifactBinding {
  const record = exactRecord(value, label, ["path", "digest"]);
  return parseArtifactRef({
    path: safeRelativePath(nonEmptyString(record.path, `${label}.path`), `${label}.path`),
    digest: exactDigest(record.digest, `${label}.digest`),
  }, label);
}

export function safeRelativePath(value: string, label: string): string {
  if (isAbsolute(value) || value === "" || value.includes("\0")) forbidden(`${label} must be workspace-relative`);
  const segments = value.split(/[\\/]/u);
  if (segments.some((segment) => segment === "" || segment === "..")) forbidden(`${label} contains traversal`);
  const normal = segments.filter((segment) => segment !== ".").join("/");
  return normal.length === 0 ? "." : normal;
}

export function contained(root: string, target: string): boolean {
  const child = relative(root, target);
  return child === "" || (!child.startsWith(`..${sep}`) && child !== ".." && !isAbsolute(child));
}

export function resolveAuthorityPath(root: string, value: string, label: string): string {
  const relativePath = safeRelativePath(value, label);
  const absolute = resolve(root, relativePath);
  if (!contained(root, absolute)) forbidden(`${label} escapes the trusted project`);
  let cursor = root;
  if (relativePath !== ".") {
    for (const segment of relativePath.split("/")) {
      cursor = resolve(cursor, segment);
      try {
        if (lstatSync(cursor).isSymbolicLink()) forbidden(`${label} resolves through a symlink`);
      } catch (error: unknown) {
        if (isRow(error) && error.code === "ENOENT") break;
        throw error;
      }
    }
  }
  return absolute;
}

export function resourceAmounts(value: unknown, label: string, allowEmpty = false): ResourceAmounts {
  if (!isRow(value)) protocol(`${label} must be an object`);
  if (!allowEmpty && Object.keys(value).length === 0) protocol(`${label} must not be empty`);
  const result: Record<string, number> = {};
  for (const [unit, amount] of Object.entries(value).sort(([left], [right]) => left.localeCompare(right))) {
    if (!UNIT_KEY.test(unit)) protocol(`${label}.${unit} has an invalid unit key`);
    if (typeof amount !== "number" || !Number.isSafeInteger(amount) || amount < 0) {
      protocol(`${label}.${unit} must be a non-negative safe integer`);
    }
    result[unit] = amount;
  }
  return result;
}

export function normaliseLaunchChairAuthority(value: unknown, projectRoot: string): NormalisedLaunchAuthority {
  const root = realpathSync(projectRoot);
  const parsed = parseAuthorityEnvelopeV2(value, "chair_authority");
  const canonicalPaths = (paths: readonly string[], label: string): string[] => {
    return [...new Set(paths.map((path) => {
      const absolute = resolveAuthorityPath(root, path, label);
      const relativePath = relative(root, absolute);
      return relativePath === "" ? "." : relativePath.split(sep).join("/");
    }))].sort();
  };
  const workspaceRoots = canonicalPaths(parsed.workspaceRoots, "chair_authority.workspaceRoots");
  const sourcePaths = canonicalPaths(parsed.sourcePaths, "chair_authority.sourcePaths");
  const artifactPaths = canonicalPaths(parsed.artifactPaths, "chair_authority.artifactPaths");
  if (
    sourcePaths.some((path) => !workspaceRoots.some((workspace) => contained(workspace, path))) ||
    artifactPaths.some((path) => !workspaceRoots.some((workspace) => contained(workspace, path)))
  ) forbidden("chair authority source or artifact path escapes its workspace roots");
  const expires = Date.parse(parsed.expiresAt);
  if (!Number.isFinite(expires)) protocol("chair_authority.expiresAt must be an ISO timestamp");
  const disclosure = parsed.disclosure.level === "scoped"
    ? { level: "scoped" as const, scopes: [...parsed.disclosure.scopes].sort() }
    : parsed.disclosure;
  return {
    schemaVersion: 2,
    approval: { ...parsed.approval },
    workspaceRoots,
    sourcePaths,
    artifactPaths,
    actions: [...parsed.actions].sort(),
    deniedPaths: canonicalPaths(parsed.deniedPaths, "chair_authority.deniedPaths"),
    deniedActions: [...parsed.deniedActions].sort(),
    prohibitedActions: [...parsed.prohibitedActions].sort(),
    disclosure,
    secrets: parsed.secrets.access === "none"
      ? { access: "none" }
      : { access: "use-without-disclosure", references: [...parsed.secrets.references].sort() },
    deployment: parsed.deployment.allowed
      ? { allowed: true, targets: [...parsed.deployment.targets].sort() }
      : { allowed: false },
    irreversibleActions: parsed.irreversibleActions.allowed
      ? { allowed: true, actionIds: [...parsed.irreversibleActions.actionIds].sort() }
      : { allowed: false },
    network: parsed.network.toolEgress === "none"
      ? { toolEgress: "none" }
      : { toolEgress: "allowlist", allowedHosts: [...parsed.network.allowedHosts].sort() },
    expiresAt: new Date(expires).toISOString(),
    budget: resourceAmounts(parsed.budget, "chair_authority.budget", true),
  };
}

export function readArtifact(root: string, reference: ArtifactBinding, label: string): string {
  const relativePath = safeRelativePath(reference.path, `${label}.path`);
  const absolute = resolve(root, relativePath);
  if (!contained(root, absolute)) forbidden(`${label} escapes the trusted project`);
  let handle: number | undefined;
  try {
    handle = openSync(absolute, constants.O_RDONLY | constants.O_NOFOLLOW);
    const info = fstatSync(handle);
    if (!info.isFile() || info.size > MAX_ARTIFACT_BYTES) protocol(`${label} must be a bounded regular file`);
    if (realpathSync(absolute) !== absolute) forbidden(`${label} resolves through a symlink`);
    const value = readFileSync(handle, "utf8");
    if (`sha256:${sha256(value)}` !== reference.digest) stale(`${label} digest changed`);
    return value;
  } catch (error: unknown) {
    if (error instanceof ProjectFabricCoreError) throw error;
    forbidden(`${label} cannot be opened without following symlinks`);
  } finally {
    if (handle !== undefined) closeSync(handle);
  }
  throw new Error("unreachable artifact read");
}

export function jsonArtifact(root: string, reference: ArtifactBinding, label: string): unknown {
  const value = readArtifact(root, reference, label);
  try {
    return JSON.parse(value) as unknown;
  } catch {
    protocol(`${label} is not JSON`);
  }
}

export function assertSameArtifact(left: ArtifactBinding, right: ArtifactBinding, label: string): void {
  if (left.path !== right.path || left.digest !== right.digest) stale(`${label} changed`);
}

export function assertNarrowing(parent: ResourceAmounts, child: ResourceAmounts, label: string): void {
  for (const [unit, amount] of Object.entries(child)) {
    if (parent[unit] === undefined || amount > parent[unit]) forbidden(`${label}.${unit} widens its parent`);
  }
}

export function sameAmounts(left: ResourceAmounts, right: ResourceAmounts): boolean {
  return canonicalJson(left) === canonicalJson(right);
}

export function parsePacket(value: unknown, projectRoot: string): LaunchPacket {
  let packet: ReturnType<typeof parseLaunchPacketV1>;
  try {
    packet = parseLaunchPacketV1(value);
  } catch {
    protocol("launch_packet_v1 does not match the closed protocol schema");
  }
  const projectRunDirectory = safeRelativePath(
    packet.projectRunDirectory,
    "launch_packet_v1.project_run_directory",
  );
  resolveAuthorityPath(projectRoot, projectRunDirectory, "launch_packet_v1.project_run_directory");
  return {
    schemaVersion: 1,
    projectId: packet.projectId,
    projectSessionId: packet.projectSessionId,
    runId: packet.runId,
    chairAgentId: packet.chairAgentId,
    projectRunDirectory,
    topologyMode: packet.topologyMode,
    budgetRef: packet.budgetRef,
    resourcePlanRef: exactArtifact(packet.resourcePlanRef, "launch_packet_v1.resourcePlanRef"),
    chairAuthority: normaliseLaunchChairAuthority(packet.chairAuthority, projectRoot),
    provider: {
      adapterId: packet.provider.adapterId,
      actionId: packet.provider.actionId,
      contractDigest: packet.provider.contractDigest as Digest,
      inputSchemaId: packet.provider.inputSchemaId,
      input: packet.provider.input,
    },
  };
}

export function parsePlan(value: unknown): LaunchResourcePlan {
  let parsed: ReturnType<typeof parseLaunchResourcePlanV1>;
  try {
    parsed = parseLaunchResourcePlanV1(value);
  } catch {
    protocol("launch_resource_plan_v1 does not match the closed protocol schema");
  }
  const project = { scopeId: parsed.scopes.project.scopeId, limits: resourceAmounts(parsed.scopes.project.limits, "project limits") };
  const projectSession = { scopeId: parsed.scopes.projectSession.scopeId, limits: resourceAmounts(parsed.scopes.projectSession.limits, "project-session limits") };
  const coordinationRun = { scopeId: parsed.scopes.coordinationRun.scopeId, limits: resourceAmounts(parsed.scopes.coordinationRun.limits, "coordination-run limits") };
  assertNarrowing(project.limits, projectSession.limits, "project-session scope");
  assertNarrowing(projectSession.limits, coordinationRun.limits, "coordination-run scope");
  const amounts = resourceAmounts(parsed.launchReservation.amounts, "launch_resource_plan_v1.launchReservation.amounts");
  assertNarrowing(coordinationRun.limits, amounts, "launch reservation");
  return {
    schemaVersion: 1,
    projectId: parsed.projectId,
    projectSessionId: parsed.projectSessionId,
    runId: parsed.runId,
    budgetRef: parsed.budgetRef,
    scopes: { project, projectSession, coordinationRun },
    launchReservation: { amounts },
  };
}

export function isoTimestamp(value: unknown, label: string): string {
  const timestamp = nonEmptyString(value, label);
  const milliseconds = Date.parse(timestamp);
  if (!Number.isFinite(milliseconds)) protocol(`${label} must be an ISO timestamp`);
  return new Date(milliseconds).toISOString();
}

export function computeLaunchResourceStateDigest(
  database: Database.Database,
  projectId: string,
  projectSessionId: string,
): Digest {
  const scopes = database.prepare(`
    SELECT scope_id, project_session_id, coordination_run_id, parent_scope_id,
           scope_kind, owner_ref, state, revision
      FROM resource_scopes
     WHERE project_id=? AND (project_session_id IS NULL OR project_session_id=?)
     ORDER BY scope_kind, owner_ref, scope_id
  `).all(projectId, projectSessionId).filter(isRow).map((value) => ({
    scopeId: text(value, "scope_id"),
    projectSessionId: value.project_session_id,
    coordinationRunId: value.coordination_run_id,
    parentScopeId: value.parent_scope_id,
    scopeKind: text(value, "scope_kind"),
    ownerRef: text(value, "owner_ref"),
    state: text(value, "state"),
    revision: integer(value, "revision"),
    dimensions: database.prepare(`
      SELECT unit_key, limit_value, used, reserved, usage_unknown
        FROM resource_dimensions WHERE scope_id=? ORDER BY unit_key
    `).all(text(value, "scope_id")),
  }));
  const reservations = database.prepare(`
    SELECT reservation_id, coordination_run_id, leaf_scope_id, operation_id,
           state, revision, generation, path_json, amounts_json
      FROM resource_reservations
     WHERE project_session_id=? AND state NOT IN ('released','reconciled')
     ORDER BY reservation_id
  `).all(projectSessionId);
  return `sha256:${sha256(canonicalJson({ scopes, reservations }))}` as Digest;
}

/**
 * Byte-moved from `LaunchCustodyService#isProvedNoEffect` (issue #354, S4e): pure predicate over
 * a stored launch-custody row's provider-action outcome. Used by `launch-service.ts` to gate
 * retry inspection.
 */
export function isProvedNoEffectOutcome(value: Row): boolean {
  const serialized = value.result_json;
  if (typeof serialized !== "string") return false;
  try {
    const parsed = JSON.parse(serialized) as unknown;
    if (isRow(parsed) && parsed.kind === "core-pre-dispatch-no-effect") return true;
    return parseLaunchAdapterOutcomeV1(parsed).outcome.kind === "terminal-no-effect";
  } catch {
    return false;
  }
}

/**
 * Byte-moved from `LaunchCustodyService#launchProviderActionJournalRefForCommand` (issue #354,
 * S4e): a read-only projection, kept in the contracts module to hold `launch-service.ts` under
 * the 1,000-line ratchet (plan §2 "S4e": "split no further unless a new file would exceed
 * 1,000 lines").
 */
export function launchProviderActionJournalRefForCommand(
  database: Database.Database,
  operatorId: string,
  commandId: string,
): LaunchProviderActionJournalRefV1 {
  const value = row(database.prepare(`
    SELECT c.project_session_id, c.custody_attempt_generation, c.coordination_run_id,
           c.provider_adapter_id, c.provider_action_id, c.provider_contract_digest,
           p.journal_revision, p.status, p.result_json, p.execution_count, p.effect_count
      FROM project_session_launch_custody c
      JOIN provider_actions p
        ON p.adapter_id=c.provider_adapter_id AND p.action_id=c.provider_action_id
     WHERE c.operator_id=? AND c.operator_command_id=?
  `).get(operatorId, commandId), "launch provider action reference");
  const status = text(value, "status");
  const common = {
    schemaVersion: 1,
    projectSessionId: text(value, "project_session_id"),
    coordinationRunId: text(value, "coordination_run_id"),
    actionRef: {
      adapterId: text(value, "provider_adapter_id"),
      actionId: text(value, "provider_action_id"),
    },
    providerContractDigest: text(value, "provider_contract_digest"),
    custodyAttemptGeneration: integer(value, "custody_attempt_generation"),
    journalRevision: integer(value, "journal_revision"),
  };
  if (status === "prepared" || status === "dispatched" || status === "accepted") {
    return parseLaunchProviderActionJournalRefV1({
      ...common,
      journalState: status,
      outcomeKind: null,
      outcomeDigest: null,
    });
  }
  if (status === "ambiguous") {
    const result = value.result_json;
    if (typeof result !== "string") throw new Error("ambiguous launch action has no outcome");
    return parseLaunchProviderActionJournalRefV1({
      ...common,
      journalState: "ambiguous",
      outcomeKind: "ambiguous",
      outcomeDigest: `sha256:${sha256(result)}`,
    });
  }
  if (status === "terminal") {
    const result = value.result_json;
    if (typeof result !== "string") throw new Error("terminal launch action has no outcome");
    let outcomeKind: "terminal-success" | "terminal-no-effect";
    try {
      const parsed = JSON.parse(result) as unknown;
      if (isRow(parsed) && parsed.kind === "core-pre-dispatch-no-effect") outcomeKind = "terminal-no-effect";
      else {
        const adapterOutcome = parseLaunchAdapterOutcomeV1(parsed);
        if (adapterOutcome.outcome.kind === "ambiguous") throw new Error("terminal action stored ambiguity");
        outcomeKind = adapterOutcome.outcome.kind;
      }
    } catch (error: unknown) {
      throw new Error("terminal launch outcome is invalid", { cause: error });
    }
    return parseLaunchProviderActionJournalRefV1({
      ...common,
      journalState: "terminal",
      outcomeKind,
      outcomeDigest: `sha256:${sha256(result)}`,
    });
  }
  throw new Error(`launch provider action has invalid status ${status}`);
}

/**
 * Byte-moved from `LaunchCustodyService#seatProvisioningDescriptorForCommand` (issue #354, S4e);
 * see the note on `launchProviderActionJournalRefForCommand` above for why it lives here.
 */
export function seatProvisioningDescriptorForCommand(
  database: Database.Database,
  operatorId: string,
  commandId: string,
): McpSeatProvisioningDescriptorV1 {
  const value = row(database.prepare(`
    SELECT session.project_session_id, session.revision AS session_revision,
           session.generation AS session_generation, custody.coordination_run_id,
           run.revision AS run_revision, run.chair_agent_id,
           run.chair_generation, run.chair_lease_id
      FROM project_session_launch_custody custody
      JOIN project_sessions session
        ON session.project_session_id=custody.project_session_id
      JOIN runs run
        ON run.project_session_id=custody.project_session_id
       AND run.run_id=custody.coordination_run_id
     WHERE custody.operator_id=? AND custody.operator_command_id=?
  `).get(operatorId, commandId), "launch MCP seat provisioning descriptor");
  return {
    schemaVersion: 1,
    projectSessionId: parseIdentifier<"ProjectSessionId">(
      text(value, "project_session_id"),
      "launchSeatProvisioning.projectSessionId",
    ),
    sessionRevision: integer(value, "session_revision"),
    sessionGeneration: integer(value, "session_generation"),
    coordinationRunId: parseIdentifier<"CoordinationRunId">(
      text(value, "coordination_run_id"),
      "launchSeatProvisioning.coordinationRunId",
    ),
    runRevision: integer(value, "run_revision"),
    chairAgentId: parseIdentifier<"AgentId">(
      text(value, "chair_agent_id"),
      "launchSeatProvisioning.chairAgentId",
    ),
    chairGeneration: integer(value, "chair_generation"),
    chairLeaseId: parseIdentifier<"LeaseId">(
      text(value, "chair_lease_id"),
      "launchSeatProvisioning.chairLeaseId",
    ),
  };
}
