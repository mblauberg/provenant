import { randomUUID } from "node:crypto";
import { createConnection } from "node:net";

import {
  ACTIVITY_NARRATIVE_GROUPING_FEATURE,
  DECLARED_RUN_PROGRESS_FEATURE,
  AGENT_TOPOLOGY_PROJECTION_FEATURE,
  GATE_SYSTEM_SUPERSESSION_FEATURE,
  NATIVE_NOTIFICATION_PROJECTION_FEATURE,
  RUN_IDENTITY_PROJECTION_FEATURE,
  RUN_LIFECYCLE_FACTS_FEATURE,
  RUN_SCOPED_PROJECTION_FEATURE,
  RUN_SESSION_PROJECTION_FEATURE,
  WORK_FACTS_PROJECTION_FEATURE,
  NdjsonRpcTransport,
  ProtocolRemoteError,
  ProtocolResultShapeError,
  ProtocolTransportError,
  createOperatorClient,
  type CommandId,
  type NegotiatedOperatorClient,
  type NonTakeoverOperatorAction,
  type OperatorAttachment,
  type OperatorCapabilityCredential,
  type OperatorClientId,
  type OperatorId,
  type OperatorMutationContext,
  type ProjectId,
  type ProjectSessionDiscovery,
  type ProjectSessionId,
  type ProtocolFeature,
  type ProtocolInitializeRequest,
  type Timestamp,
  type ChairBridgeRecoveryIntent,
} from "@local/agent-fabric-protocol";

import {
  FabricDaemonClient,
  startFabricDaemon,
  type DaemonStartOptions,
} from "../daemon/client.js";
import { resolveFabricPaths, type FabricPaths } from "../cli/paths.js";
import { defaultDaemonStartOptions } from "../cli/default-daemon-options.js";
import { trustedWorkspaceIdentity } from "../cli/workspace-trust.js";
import { FabricError } from "../errors.js";
import { REVIEW_PROFILE_CATALOGUE_MEMBERS } from "../review/profile/catalogue-digest.js";

const PROJECT_ACTIONS = ["launch", "read"] as const;
const SESSION_ACTIONS = [
  "read",
  "decide",
  "launch",
  "steer",
  "pause",
  "resume",
  "cancel",
  "drain",
  "stop",
] as const satisfies readonly NonTakeoverOperatorAction[];
const NON_ATTACHABLE_SESSION_STATES = new Set(["closed", "cancelled", "launch_failed"]);
const NON_SELECTABLE_SESSION_STATES = new Set(["closed", "cancelled"]);
const REQUIRED_FEATURES: readonly ProtocolFeature[] = Object.freeze([
  "operator-control.v1",
  "operator-projection.v1",
  RUN_SESSION_PROJECTION_FEATURE,
  DECLARED_RUN_PROGRESS_FEATURE,
  RUN_IDENTITY_PROJECTION_FEATURE,
  ACTIVITY_NARRATIVE_GROUPING_FEATURE,
  AGENT_TOPOLOGY_PROJECTION_FEATURE,
  WORK_FACTS_PROJECTION_FEATURE,
  RUN_SCOPED_PROJECTION_FEATURE,
  RUN_LIFECYCLE_FACTS_FEATURE,
  "artifact-content-read.v1",
] as const satisfies readonly ProtocolFeature[]);
export const CURRENT_CONSOLE_OPTIONAL_FEATURES: readonly ProtocolFeature[] = Object.freeze([
  "project-sessions.v1",
  "operator-projection.v2",
  "scoped-gate-read.v1",
  "scoped-gates.v1",
  "intakes.v1",
  "operator-actions.v1",
  "message-body-read.v1",
  "operator-repository-read.v1",
  "lifecycle-control.v1",
  "launch-custody.v1",
] as const satisfies readonly ProtocolFeature[]);
const OPTIONAL_FEATURES: readonly ProtocolFeature[] = Object.freeze([
  NATIVE_NOTIFICATION_PROJECTION_FEATURE,
  GATE_SYSTEM_SUPERSESSION_FEATURE,
  ...CURRENT_CONSOLE_OPTIONAL_FEATURES,
]);

/**
 * Truthful reasons a local Console bootstrap cannot attach. The four legacy
 * arms (`configuration-missing`, `schema-cutover-required`,
 * `authority-unavailable`, `start-failed`) collapsed every daemon/transport
 * failure into `start-failed`; the specific daemon-election, socket, spawn,
 * handshake and incompatibility arms preserve the exact causal finding the
 * lifecycle-and-failure contract requires so the System view can name the
 * failed stage and its bounded remediation rather than a generic collapse.
 */
export type LocalOperatorConsoleUnavailableReason =
  | "configuration-missing"
  | "schema-cutover-required"
  | "authority-unavailable"
  | "daemon-unreachable"
  | "daemon-incompatible"
  | "socket-unavailable"
  | "daemon-election-conflict"
  | "daemon-spawn-failed"
  | "bootstrap-receipt-invalid"
  | "review-profile-catalogue-invalid"
  | "start-failed";

const CONSOLE_UNAVAILABLE_CODES = {
  "configuration-missing": "CONSOLE_CONFIGURATION_UNAVAILABLE",
  "schema-cutover-required": "SCHEMA_CUTOVER_REQUIRED",
  "authority-unavailable": "CONSOLE_AUTHORITY_UNAVAILABLE",
  "daemon-unreachable": "CONSOLE_DAEMON_UNREACHABLE",
  "daemon-incompatible": "CONSOLE_DAEMON_INCOMPATIBLE",
  "socket-unavailable": "CONSOLE_SOCKET_UNAVAILABLE",
  "daemon-election-conflict": "CONSOLE_DAEMON_ELECTION_CONFLICT",
  "daemon-spawn-failed": "CONSOLE_DAEMON_SPAWN_FAILED",
  "bootstrap-receipt-invalid": "CONSOLE_BOOTSTRAP_RECEIPT_INVALID",
  "review-profile-catalogue-invalid": "CONSOLE_REVIEW_PROFILE_CATALOGUE_INVALID",
  "start-failed": "CONSOLE_START_FAILED",
} as const satisfies Record<LocalOperatorConsoleUnavailableReason, string>;

export type LocalOperatorConsoleUnavailableCode =
  typeof CONSOLE_UNAVAILABLE_CODES[LocalOperatorConsoleUnavailableReason];

/**
 * Maps a `startFabricDaemon` failure to the exact truthful Console reason.
 * Bootstrap surfaces stable, non-secret `code` strings (`BOOTSTRAP_*`,
 * `SCHEMA_CUTOVER_REQUIRED`) and typed error names; anything unrecognised
 * remains the honest `start-failed` fallback rather than a fabricated stage.
 */
export function daemonStartUnavailableReason(
  error: unknown,
): LocalOperatorConsoleUnavailableReason {
  if (typeof error !== "object" || error === null) return "start-failed";
  const code = "code" in error && typeof error.code === "string"
    ? error.code
    : null;
  if (code === "SCHEMA_CUTOVER_REQUIRED") return "schema-cutover-required";
  if (
    error instanceof FabricError &&
    (code === "ARTIFACT_DIGEST_INVALID" || code === "NOT_FOUND") &&
    error.field !== undefined &&
    REVIEW_PROFILE_CATALOGUE_MEMBERS.some((member) => member === error.field)
  ) {
    return "review-profile-catalogue-invalid";
  }
  if (code === "BOOTSTRAP_SOCKET_MISMATCH") return "socket-unavailable";
  if (
    code === "BOOTSTRAP_INCOMPATIBLE_INCUMBENT" ||
    code === "PROTOCOL_INCOMPATIBLE"
  ) return "daemon-incompatible";
  if (
    code === "BOOTSTRAP_HANDSHAKE_INVALID" ||
    code === "BOOTSTRAP_ACTION_MISMATCH" ||
    code === "BOOTSTRAP_RECEIPT_INVALID"
  ) {
    return "bootstrap-receipt-invalid";
  }
  const name = "name" in error && typeof error.name === "string"
    ? error.name
    : null;
  if (name === "BootstrapElectionError") return "daemon-election-conflict";
  if (name === "BootstrapSpawnPhaseError") return "daemon-spawn-failed";
  if (typeof code === "string" && code.startsWith("BOOTSTRAP_")) {
    return "daemon-unreachable";
  }
  return "start-failed";
}

export class LocalOperatorConsoleUnavailableError extends Error {
  readonly code: LocalOperatorConsoleUnavailableCode;
  readonly reason: LocalOperatorConsoleUnavailableReason;

  constructor(reason: LocalOperatorConsoleUnavailableReason) {
    super(reason === "schema-cutover-required"
      ? "CUTOVER REQUIRED — existing database preserved"
      : reason === "review-profile-catalogue-invalid"
        ? "local Console review-profile-catalogue-invalid. Run: npm run profile:catalogue:pin"
        : `local Console ${reason}`);
    this.name = "LocalOperatorConsoleUnavailableError";
    this.reason = reason;
    this.code = CONSOLE_UNAVAILABLE_CODES[reason];
  }
}

export type LocalOperatorConsoleCompatibility = Readonly<{ mode: "current" }>;

export type ProtocolFailureAnnotation = Readonly<{
  code: string;
  message: string;
}>;

export class LocalOperatorConsoleProtocolIncompatibleError extends Error {
  readonly code = "CONSOLE_PROTOCOL_INCOMPATIBLE" as const;
  readonly primary: ProtocolFailureAnnotation;
  readonly result: (ProtocolFailureAnnotation & {
    operation?: string;
    closedReason?: string;
  }) | undefined;

  constructor(input: Readonly<{
    primary: ProtocolFailureAnnotation;
    result?: ProtocolFailureAnnotation & { operation?: string; closedReason?: string };
    cause: Error;
  }>) {
    super(`local Console protocol incompatible: ${input.primary.message}`, { cause: input.cause });
    this.name = "LocalOperatorConsoleProtocolIncompatibleError";
    this.primary = input.primary;
    this.result = input.result;
  }
}

export type LocalOperatorConsoleSessionOptions = Readonly<{
  projectRoot: string;
  surface: "standalone" | "herdr";
  projectSessionId?: ProjectSessionId;
  paths?: FabricPaths;
  agentsHome?: string;
  daemon?: Pick<
    DaemonStartOptions,
    | "adapters"
    | "executionProfile"
    | "maximumConcurrentProviderTurns"
    | "workspaceRoots"
    | "trustedGitConfiguration"
    | "configuration"
  >;
  clientId?: string;
  now?: () => number;
  credentialLifetimeMs?: number;
  attachmentLeaseMs?: number;
  heartbeatIntervalMs?: number;
}>;

export type LocalOperatorConsoleSession = Readonly<{
  client: NegotiatedOperatorClient;
  compatibility: LocalOperatorConsoleCompatibility;
  credential: OperatorCapabilityCredential;
  projectClient: NegotiatedOperatorClient;
  projectCompatibility: LocalOperatorConsoleCompatibility;
  projectCredential: OperatorCapabilityCredential;
  attachableProjectSessions: readonly ProjectSessionDiscovery[];
  projectId: ProjectId;
  operatorId: OperatorId;
  projectSessionId: ProjectSessionId | undefined;
  chairRecoveryIntent: Extract<ChairBridgeRecoveryIntent, { path: "abandon" }> | undefined;
  clientId: OperatorClientId;
  daemonPid: number;
  refreshProjectSessions(): Promise<readonly ProjectSessionDiscovery[]>;
  selectProjectSession(projectSessionId: ProjectSessionId): Promise<void>;
  selectProject(): Promise<void>;
  detach(input: Readonly<{ reason: "operator" | "safety" | "signal" }>): Promise<void>;
  close(): Promise<void>;
}>;

function positiveDuration(
  value: number | undefined,
  fallback: number,
  label: string,
): number {
  const selected = value ?? fallback;
  if (!Number.isSafeInteger(selected) || selected < 1) {
    throw new TypeError(`${label} must be a positive safe integer`);
  }
  return selected;
}

function isoTimestamp(milliseconds: number): Timestamp {
  return new Date(milliseconds).toISOString() as Timestamp;
}

function assertFuture(expiresAt: string, now: number): void {
  const parsed = Date.parse(expiresAt);
  if (!Number.isFinite(parsed) || parsed <= now) {
    throw new LocalOperatorConsoleUnavailableError("authority-unavailable");
  }
}

async function projectCredential(
  privateClient: FabricDaemonClient,
  identity: Awaited<ReturnType<typeof trustedWorkspaceIdentity>>,
  now: number,
  credentialLifetimeMs: number,
): ReturnType<FabricDaemonClient["openLocalOperatorConsoleCapability"]> {
  return await privateClient.openLocalOperatorConsoleCapability({
    canonicalRoot: identity.canonicalRoot,
    trustRecordDigest: identity.trustRecordDigest,
    projectAuthorityGeneration: 1,
    actions: PROJECT_ACTIONS,
    expiresAt: isoTimestamp(now + credentialLifetimeMs),
  });
}

async function sessionCredential(
  privateClient: FabricDaemonClient,
  identity: Awaited<ReturnType<typeof trustedWorkspaceIdentity>>,
  project: Awaited<ReturnType<FabricDaemonClient["openLocalOperatorConsoleCapability"]>>,
  selected: ProjectSessionDiscovery,
  now: number,
  credentialLifetimeMs: number,
): Promise<
  | Awaited<ReturnType<FabricDaemonClient["openLocalOperatorConsoleSessionCapability"]>>
  | Awaited<ReturnType<FabricDaemonClient["openLocalOperatorConsoleTakeoverCapability"]>>
> {
  const expiresAt = isoTimestamp(Math.min(
    Date.parse(project.expiresAt),
    now + credentialLifetimeMs,
  ));
  assertFuture(expiresAt, now);
  if (selected.state === "recovery_required") {
    return await privateClient.openLocalOperatorConsoleTakeoverCapability({
      projectId: project.projectId,
      canonicalRoot: identity.canonicalRoot,
      trustRecordDigest: identity.trustRecordDigest,
      projectCapability: project.credential,
      projectSessionId: selected.projectSessionId,
      expiresAt,
    });
  }
  return await privateClient.openLocalOperatorConsoleSessionCapability({
    projectId: project.projectId,
    canonicalRoot: identity.canonicalRoot,
    trustRecordDigest: identity.trustRecordDigest,
    projectCapability: project.credential,
    projectSessionId: selected.projectSessionId,
    sessionGeneration: selected.generation,
    actions: SESSION_ACTIONS,
    expiresAt,
    launchEnvelopeExpiresAt: expiresAt,
  });
}

function protocolFailureAnnotation(error: Error): ProtocolFailureAnnotation {
  const code = "code" in error && typeof error.code === "string" ? error.code : error.name;
  return { code, message: error.message };
}

function operatorInitializeRequest(input: Readonly<{
  credential: OperatorCapabilityCredential;
  surface: "standalone" | "herdr";
  optionalFeatures: readonly ProtocolFeature[];
}>): ProtocolInitializeRequest {
  return {
    protocolVersion: 1,
    client: { name: `agent-fabric-console-${input.surface}`, version: "0.1.0" },
    authentication: {
      scheme: "capability",
      credential: input.credential.token,
      clientNonce: `console_${randomUUID()}`,
    },
    expectedPrincipalKind: "operator",
    requiredFeatures: REQUIRED_FEATURES,
    optionalFeatures: input.optionalFeatures,
  };
}

export async function connectLocalOperatorConsoleClient(input: Readonly<{
  socketPath: string;
  credential: OperatorCapabilityCredential;
  surface: "standalone" | "herdr";
}>): Promise<Readonly<{
  client: NegotiatedOperatorClient;
  compatibility: LocalOperatorConsoleCompatibility;
}>> {
  const connect = async (optionalFeatures: readonly ProtocolFeature[]): Promise<NegotiatedOperatorClient> => {
    const transport = await NdjsonRpcTransport.connect(
      createConnection(input.socketPath),
      operatorInitializeRequest({ ...input, optionalFeatures }),
    );
    return createOperatorClient(transport);
  };
  try {
    return { client: await connect(OPTIONAL_FEATURES), compatibility: { mode: "current" } };
  } catch (primary: unknown) {
    if (!(primary instanceof ProtocolRemoteError) || primary.code !== "PROTOCOL_INVALID") throw primary;
    throw new LocalOperatorConsoleProtocolIncompatibleError({
      primary: protocolFailureAnnotation(primary),
      cause: primary,
    });
  }
}

function resultProtocolIncompatible(
  error: ProtocolTransportError,
): LocalOperatorConsoleProtocolIncompatibleError {
  const shape = error.cause instanceof ProtocolResultShapeError ? error.cause : undefined;
  const result = {
    ...protocolFailureAnnotation(error),
    ...(shape === undefined ? {} : { operation: shape.operation, closedReason: shape.reason }),
  };
  return new LocalOperatorConsoleProtocolIncompatibleError({
    primary: protocolFailureAnnotation(error),
    result,
    cause: error,
  });
}

async function discoverProjectSessions(input: {
  client: NegotiatedOperatorClient;
  credential: OperatorCapabilityCredential;
  projectId: ProjectId;
  canonicalRoot: string;
}): Promise<readonly ProjectSessionDiscovery[]> {
  const projection = input.client.projection;
  if (projection === undefined) {
    throw new LocalOperatorConsoleUnavailableError("authority-unavailable");
  }
  const discovered: ProjectSessionDiscovery[] = [];
  const ids = new Set<ProjectSessionId>();
  let after = 0;
  for (;;) {
    const discovery = await projection.discover({
      credential: input.credential,
      projectId: input.projectId,
      after,
      limit: 100,
    });
    if (
      discovery.project.freshness !== "live" ||
      discovery.project.value.projectId !== input.projectId ||
      discovery.project.value.canonicalRoot !== input.canonicalRoot ||
      discovery.sessions.freshness !== "live"
    ) {
      throw new LocalOperatorConsoleUnavailableError("authority-unavailable");
    }
    for (const session of discovery.sessions.value.items) {
      if (ids.has(session.projectSessionId)) {
        throw new LocalOperatorConsoleUnavailableError("authority-unavailable");
      }
      ids.add(session.projectSessionId);
      discovered.push(session);
    }
    if (!discovery.sessions.value.hasMore) return discovered;
    const next = discovery.sessions.value.nextCursor;
    if (!Number.isSafeInteger(next) || next <= after) {
      throw new LocalOperatorConsoleUnavailableError("authority-unavailable");
    }
    after = next;
  }
}

function attachableSessions(
  sessions: readonly ProjectSessionDiscovery[],
): readonly ProjectSessionDiscovery[] {
  return sessions.filter(({ state }) => !NON_ATTACHABLE_SESSION_STATES.has(state));
}

/**
 * An explicitly named session is selectable while an operator may still act on
 * it: live states plus `launch_failed`, whose recovery path is a fresh
 * operator-prepared launch on the same session. Closed and cancelled sessions
 * stay non-selectable.
 */
function selectableSession(
  sessions: readonly ProjectSessionDiscovery[],
  projectSessionId: ProjectSessionId,
): ProjectSessionDiscovery | undefined {
  const session = sessions.find(
    (candidate) => candidate.projectSessionId === projectSessionId,
  );
  if (session === undefined || NON_SELECTABLE_SESSION_STATES.has(session.state)) return undefined;
  return session;
}

function mutationContext(input: {
  credential: OperatorCapabilityCredential;
  commandId: string;
  expectedRevision: number;
  operatorId: OperatorId;
  clientId: OperatorClientId;
  inputEventId: string;
}): OperatorMutationContext {
  return {
    credential: input.credential,
    commandId: input.commandId as CommandId,
    expectedRevision: input.expectedRevision,
    actor: input.operatorId,
    provenance: {
      kind: "console-direct-input",
      clientId: input.clientId,
      inputEventId: input.inputEventId,
    },
    evidenceRefs: [],
  };
}

function attachmentOwner(input: {
  client: NegotiatedOperatorClient;
  credential: OperatorCapabilityCredential;
  operatorId: OperatorId;
  clientId: OperatorClientId;
  projectId: ProjectId;
  projectSessionId?: ProjectSessionId;
  initial: OperatorAttachment;
  capabilityExpiresAt: string;
  now: () => number;
  attachmentLeaseMs: number;
  heartbeatIntervalMs: number;
}): Pick<LocalOperatorConsoleSession, "detach" | "close"> {
  const operatorControl = input.client.operatorControl;
  if (operatorControl === undefined) {
    throw new LocalOperatorConsoleUnavailableError("authority-unavailable");
  }
  let current = input.initial;
  let detached = false;
  let detachPromise: Promise<void> | undefined;
  let closePromise: Promise<void> | undefined;
  let leaseQueue: Promise<void> = Promise.resolve();

  const heartbeat = async (): Promise<void> => {
    if (detached) return;
    const capabilityExpiry = Date.parse(input.capabilityExpiresAt);
    const extendUntilMs = Math.min(
      capabilityExpiry,
      input.now() + input.attachmentLeaseMs,
    );
    if (extendUntilMs <= input.now()) return;
    const generation = current.generation;
    const next = await operatorControl.heartbeat({
      command: mutationContext({
        credential: input.credential,
        commandId: `${input.clientId}:heartbeat:${String(generation)}`,
        expectedRevision: generation,
        operatorId: input.operatorId,
        clientId: input.clientId,
        inputEventId: `${input.clientId}:heartbeat:${String(generation)}`,
      }),
      attachmentGeneration: generation,
      extendUntil: isoTimestamp(extendUntilMs),
    });
    current = next;
  };
  const timer = setInterval(() => {
    leaseQueue = leaseQueue.then(heartbeat).catch(() => {
      clearInterval(timer);
    });
  }, input.heartbeatIntervalMs);
  timer.unref();

  const detach = async (): Promise<void> => {
    if (detachPromise !== undefined) return await detachPromise;
    detached = true;
    clearInterval(timer);
    detachPromise = (async () => {
      await leaseQueue;
      const generation = current.generation;
      await operatorControl.detach({
        command: mutationContext({
          credential: input.credential,
          commandId: `${input.clientId}:detach:${String(generation)}`,
          expectedRevision: generation,
          operatorId: input.operatorId,
          clientId: input.clientId,
          inputEventId: `${input.clientId}:detach:${String(generation)}`,
        }),
        attachmentGeneration: generation,
      });
    })();
    return await detachPromise;
  };

  return {
    detach: async (_input) => await detach(),
    close(): Promise<void> {
      closePromise ??= (async () => {
        try {
          await detach();
        } finally {
          await input.client.close();
        }
      })();
      return closePromise;
    },
  };
}

export async function openLocalOperatorConsoleSession(
  options: LocalOperatorConsoleSessionOptions,
): Promise<LocalOperatorConsoleSession> {
  const now = options.now ?? Date.now;
  const credentialLifetimeMs = positiveDuration(
    options.credentialLifetimeMs,
    8 * 60 * 60_000,
    "credentialLifetimeMs",
  );
  const attachmentLeaseMs = positiveDuration(
    options.attachmentLeaseMs,
    30_000,
    "attachmentLeaseMs",
  );
  const heartbeatIntervalMs = positiveDuration(
    options.heartbeatIntervalMs,
    10_000,
    "heartbeatIntervalMs",
  );
  if (heartbeatIntervalMs >= attachmentLeaseMs) {
    throw new TypeError("heartbeatIntervalMs must be shorter than attachmentLeaseMs");
  }
  const paths = options.paths ?? resolveFabricPaths({ createDirectories: true });
  let identity: Awaited<ReturnType<typeof trustedWorkspaceIdentity>>;
  try {
    identity = await trustedWorkspaceIdentity({
      stateDirectory: paths.stateDirectory,
      canonicalRoot: options.projectRoot,
    });
  } catch {
    throw new LocalOperatorConsoleUnavailableError("configuration-missing");
  }

  let daemon: Awaited<ReturnType<typeof startFabricDaemon>>;
  try {
    daemon = await startFabricDaemon({
      ...(options.daemon === undefined
          ? defaultDaemonStartOptions(paths, {
            agentsHomeFlag: options.agentsHome,
            environment: process.env,
            projectRoot: identity.canonicalRoot,
          })
        : { ...paths, ...options.daemon }),
    });
  } catch (error: unknown) {
    throw new LocalOperatorConsoleUnavailableError(
      daemonStartUnavailableReason(error),
    );
  }

  let privateClient: FabricDaemonClient | undefined;
  let projectClient: NegotiatedOperatorClient | undefined;
  let sessionClient: NegotiatedOperatorClient | undefined;
  try {
    privateClient = await FabricDaemonClient.connect(
      daemon.address.path,
      daemon.bootstrapCapability,
    );
    const project = await projectCredential(
      privateClient,
      identity,
      now(),
      credentialLifetimeMs,
    );
    const projectConnection = await connectLocalOperatorConsoleClient({
      socketPath: daemon.address.path,
      credential: project.credential as OperatorCapabilityCredential,
      surface: options.surface,
    });
    projectClient = projectConnection.client;
    const projectCompatibility = projectConnection.compatibility;
    let discoveredProjectSessions = await discoverProjectSessions({
      client: projectClient,
      credential: project.credential as OperatorCapabilityCredential,
      projectId: project.projectId as ProjectId,
      canonicalRoot: identity.canonicalRoot,
    });
    let attachableProjectSessions = attachableSessions(discoveredProjectSessions);
    // Spec item 35 forbids auto-selecting when a project has *multiple*
    // attachable sessions; the single-session case is the documented attach
    // path and is covered by "selects an existing session ... never creates a
    // session implicitly". Selecting nothing unconditionally would make every
    // ordinary one-session project require a manual pick.
    const selected = options.projectSessionId === undefined
      ? attachableProjectSessions.length === 1
        ? attachableProjectSessions[0]
        : undefined
      : selectableSession(discoveredProjectSessions, options.projectSessionId);
    if (options.projectSessionId !== undefined && selected === undefined) {
      throw new LocalOperatorConsoleUnavailableError("authority-unavailable");
    }
    let activeCredential = project.credential as OperatorCapabilityCredential;
    let activeCompatibility = projectCompatibility;
    let activeProjectSessionId: ProjectSessionId | undefined;
    let activeChairRecoveryIntent: Extract<ChairBridgeRecoveryIntent, { path: "abandon" }> | undefined;
    if (selected !== undefined) {
      const issued = await sessionCredential(
        privateClient,
        identity,
        project,
        selected,
        now(),
        credentialLifetimeMs,
      );
      const sessionConnection = await connectLocalOperatorConsoleClient({
        socketPath: daemon.address.path,
        credential: issued.credential as OperatorCapabilityCredential,
        surface: options.surface,
      });
      sessionClient = sessionConnection.client;
      activeCompatibility = sessionConnection.compatibility;
      activeCredential = issued.credential as OperatorCapabilityCredential;
      activeProjectSessionId = selected.projectSessionId;
      activeChairRecoveryIntent = issued.kind === "takeover"
        ? issued.recoveryIntent
        : undefined;
    }
    const retainedProjectClient = projectClient;
    const retainedPrivateClient = privateClient;
    const projectProjection = retainedProjectClient.projection;
    const projectOperatorControl = retainedProjectClient.operatorControl;
    if (projectOperatorControl === undefined || projectProjection === undefined) {
      throw new LocalOperatorConsoleUnavailableError("authority-unavailable");
    }
    const projectId = project.projectId as ProjectId;
    const projectCredentialValue = project.credential as OperatorCapabilityCredential;
    const projectSnapshot = await projectProjection.snapshot({
      credential: projectCredentialValue,
      projectId,
    });
    const expectedRevision = projectSnapshot.project.revision;
    const clientId = (options.clientId ?? `console_${randomUUID()}`) as OperatorClientId;
    const operatorId = project.operatorId as OperatorId;
    const attachExpiry = Math.min(
      Date.parse(project.expiresAt),
      now() + attachmentLeaseMs,
    );
    assertFuture(isoTimestamp(attachExpiry), now());
    const attachment = await projectOperatorControl.attach({
      command: mutationContext({
        credential: projectCredentialValue,
        commandId: `${clientId}:attach`,
        expectedRevision,
        operatorId,
        clientId,
        inputEventId: `${clientId}:attach`,
      }),
      projectId,
      requestedExpiresAt: isoTimestamp(attachExpiry),
    });
    if (
      attachment.clientId !== clientId ||
      attachment.projectId !== projectId ||
      attachment.projectSessionId !== null
    ) {
      throw new LocalOperatorConsoleUnavailableError("authority-unavailable");
    }
    daemon.release();
    const owner = attachmentOwner({
      client: retainedProjectClient,
      credential: projectCredentialValue,
      operatorId,
      clientId,
      projectId,
      initial: attachment,
      capabilityExpiresAt: project.expiresAt,
      now,
      attachmentLeaseMs,
      heartbeatIntervalMs,
    });
    let closed = false;
    let closePromise: Promise<void> | undefined;
    let selectionQueue: Promise<void> = Promise.resolve();
    const refreshProjectSessions = async (): Promise<readonly ProjectSessionDiscovery[]> => {
      if (closed) throw new Error("local Console session is closed");
      discoveredProjectSessions = await discoverProjectSessions({
        client: retainedProjectClient,
        credential: projectCredentialValue,
        projectId,
        canonicalRoot: identity.canonicalRoot,
      });
      attachableProjectSessions = attachableSessions(discoveredProjectSessions);
      return attachableProjectSessions;
    };
    const selectProjectSession = async (projectSessionId: ProjectSessionId): Promise<void> => {
      const change = async (): Promise<void> => {
        if (closed) throw new Error("local Console session is closed");
        if (activeProjectSessionId === projectSessionId && sessionClient !== undefined) return;
        let selectedSession = selectableSession(discoveredProjectSessions, projectSessionId);
        if (selectedSession === undefined) {
          await refreshProjectSessions();
          selectedSession = selectableSession(discoveredProjectSessions, projectSessionId);
        }
        if (selectedSession === undefined) {
          throw new LocalOperatorConsoleUnavailableError("authority-unavailable");
        }
        const issued = await sessionCredential(
          retainedPrivateClient,
          identity,
          project,
          selectedSession,
          now(),
          credentialLifetimeMs,
        );
        const connection = await connectLocalOperatorConsoleClient({
          socketPath: daemon.address.path,
          credential: issued.credential as OperatorCapabilityCredential,
          surface: options.surface,
        });
        try {
          const snapshot = await connection.client.projection?.snapshot({
            credential: issued.credential as OperatorCapabilityCredential,
            projectId,
            projectSessionId,
          });
          if (
            snapshot?.session.freshness !== "live" ||
            snapshot.session.value?.projectSessionId !== projectSessionId
          ) {
            throw new LocalOperatorConsoleUnavailableError("authority-unavailable");
          }
        } catch (error: unknown) {
          await connection.client.close().catch(() => undefined);
          throw error;
        }
        const previous = sessionClient;
        sessionClient = connection.client;
        activeCompatibility = connection.compatibility;
        activeCredential = issued.credential as OperatorCapabilityCredential;
        activeProjectSessionId = projectSessionId;
        activeChairRecoveryIntent = issued.kind === "takeover"
          ? issued.recoveryIntent
          : undefined;
        await previous?.close();
      };
      const operation = selectionQueue.then(change, change);
      selectionQueue = operation.catch(() => undefined);
      await operation;
    };
    const selectProject = async (): Promise<void> => {
      const change = async (): Promise<void> => {
        if (closed) throw new Error("local Console session is closed");
        const previous = sessionClient;
        sessionClient = undefined;
        activeCredential = projectCredentialValue;
        activeCompatibility = projectCompatibility;
        activeProjectSessionId = undefined;
        activeChairRecoveryIntent = undefined;
        await previous?.close();
      };
      const operation = selectionQueue.then(change, change);
      selectionQueue = operation.catch(() => undefined);
      await operation;
    };
    const result: LocalOperatorConsoleSession = {
      get client() { return sessionClient ?? retainedProjectClient; },
      get compatibility() { return activeCompatibility; },
      get credential() { return activeCredential; },
      projectClient: retainedProjectClient,
      projectCompatibility,
      projectCredential: projectCredentialValue,
      get attachableProjectSessions() { return attachableProjectSessions; },
      projectId,
      operatorId,
      get projectSessionId() { return activeProjectSessionId; },
      get chairRecoveryIntent() { return activeChairRecoveryIntent; },
      clientId,
      daemonPid: daemon.pid,
      refreshProjectSessions,
      selectProjectSession,
      selectProject,
      detach: owner.detach,
      close(): Promise<void> {
        closePromise ??= (async () => {
          closed = true;
          await selectionQueue;
          const secondary = sessionClient;
          sessionClient = undefined;
          const closedConnections = await Promise.allSettled([
            secondary?.close() ?? Promise.resolve(),
            owner.close(),
            retainedPrivateClient.close(),
          ]);
          const failures = closedConnections
            .filter((outcome): outcome is PromiseRejectedResult => outcome.status === "rejected")
            .map((outcome) => outcome.reason);
          if (failures.length > 0) {
            throw new AggregateError(failures, "local Console close failed");
          }
        })();
        return closePromise;
      },
    };
    return result;
  } catch (error: unknown) {
    daemon.release();
    await Promise.allSettled([
      privateClient?.close() ?? Promise.resolve(),
      sessionClient?.close() ?? Promise.resolve(),
      projectClient?.close() ?? Promise.resolve(),
    ]);
    if (
      error instanceof LocalOperatorConsoleUnavailableError ||
      error instanceof LocalOperatorConsoleProtocolIncompatibleError
    ) throw error;
    if (error instanceof ProtocolTransportError && error.code === "PROTOCOL_INCOMPATIBLE") {
      throw resultProtocolIncompatible(error);
    }
    throw new LocalOperatorConsoleUnavailableError("authority-unavailable");
  }
}
