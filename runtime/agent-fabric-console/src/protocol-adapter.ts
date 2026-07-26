import { createHash } from "node:crypto";

import {
  ACTIVITY_NARRATIVE_GROUPING_FEATURE,
  FABRIC_OPERATIONS,
  NATIVE_NOTIFICATION_PROJECTION_FEATURE,
  type AgentId,
  type ArtifactContentClient,
  type ArtifactContentReadResult,
  type ArtifactContentTransformation,
  type ArtifactLineFragment,
  type ArtifactRef,
  type CoordinationRunId,
  type NegotiatedOperatorClient,
  type GitRepositoryReadClient,
  type GitRepositoryReadRequest,
  type MessageBodyClient,
  type OperatorActionClient,
  type OperatorCapabilityCredential,
  type OperatorDetailReadRequest,
  type OperatorDetailReadResult,
  type OperatorProjectionSnapshot,
  type OperatorViewPageRequest,
  type OperatorViewPageResult,
  type ProjectId,
  type ProjectSessionDiscovery,
  type ProjectSessionId,
  type RunProjectionPageRequest,
  type RunProjectionPageResult,
  type ProviderActionRefV1,
  type ProviderContextPressureReadRequestV1,
  type ProviderContextPressureReadV1,
  type ProviderRouteIntegrityRecoveryProjectionV1,
  type ProviderRouteIntegrityRecoveryReadErrorV1,
  type ProviderRouteIntegrityRecoveryReadRequestV1,
  type ProjectionEventsRequest,
  type ProjectionEventsResult,
  type ProjectionSnapshotRequest,
  type ScopedGateReadRequest,
  type ScopedGateReadResult,
  type Sha256Digest,
  type SystemViewItem,
  type TaskId,
  type TopologyWaveCurrentReadRequestV1,
  type TopologyWaveCurrentReadV1,
  type ReviewCompletionReadRequestV1,
  type ReviewCompletionV1,
  type ReviewEvidenceListRequestV1,
  type ReviewEvidenceListResultV1,
  type ReviewEvidenceReadV1,
  type ReviewReadErrorV1,
  type ReviewTargetPreparationReadV1,
} from "@local/agent-fabric-protocol";
import { parseArtifactContentReadResult } from "@local/agent-fabric-protocol";

import { inspectNarrativeActivity } from "./activity-inspection.js";
import {
  inspectRunProjection,
} from "./run-projection-adapter.js";

export type { ConsoleRunProjection } from "./run-projection-adapter.js";
import type {
  ConsoleArtifactContentPage,
  ConsoleArtifactContentResult,
  ConsoleInspectionBinding,
  ConsoleReadInspection,
} from "./protocol-adapter-inspection.js";
export type {
  ConsoleArtifactContentPage,
  ConsoleArtifactContentResult,
  ConsoleInspectionBinding,
  ConsoleReadInspection,
} from "./protocol-adapter-inspection.js";

import {
  BOOTSTRAP_FAILURE_DETAILS,
  connectionDiagnosisRows,
  createConnectionDiagnosis,
  type BootstrapUnavailableReason,
  type ConsoleConnectionDiagnosis,
} from "./connection-diagnosis.js";

export type {
  BootstrapUnavailableReason,
  ConsoleConnectionDiagnosis,
  ConsoleConnectionStage,
  ConsoleConnectionStageId,
  ConsoleConnectionStageState,
} from "./connection-diagnosis.js";

import {
  FABRIC_VIEWS,
  createEmptyViewPages,
  mapProtocolRow,
  rankConsoleRows,
  revisionFromProtocol,
  revisionToProtocol,
  type ConsoleRow,
  type ConsoleViewPage,
  type ConsoleViewPages,
  type FabricView,
  type ConsoleWorkflowCapabilities,
  type Revision,
} from "./model.js";

export type ConsoleProtocolPort = Readonly<{
  snapshot(
    request: ProjectionSnapshotRequest,
  ): Promise<OperatorProjectionSnapshot>;
  events(request: ProjectionEventsRequest): Promise<ProjectionEventsResult>;
  viewPage(
    request: OperatorViewPageRequest,
  ): Promise<OperatorViewPageResult>;
  runPage(
    request: RunProjectionPageRequest,
  ): Promise<RunProjectionPageResult>;
  readDetail(
    request: OperatorDetailReadRequest,
  ): Promise<OperatorDetailReadResult>;
  readGate(request: ScopedGateReadRequest): Promise<ScopedGateReadResult>;
  readMessageBody: MessageBodyClient["read"] | null;
  readRepository: GitRepositoryReadClient["read"] | null;
  readArtifactContent: ArtifactContentClient["readContent"] | null;
  reviewCompletionRead?:
    | ((request: ReviewCompletionReadRequestV1) => Promise<ReviewCompletionV1 | ReviewReadErrorV1>)
    | null;
  reviewEvidenceList?:
    | ((request: ReviewEvidenceListRequestV1) => Promise<ReviewEvidenceListResultV1 | ReviewReadErrorV1>)
    | null;
  providerRouteIntegrityRecoveryRead?:
    | ((request: ProviderRouteIntegrityRecoveryReadRequestV1) => Promise<
        ProviderRouteIntegrityRecoveryProjectionV1 |
        ProviderRouteIntegrityRecoveryReadErrorV1
      >)
    | null;
  providerContextPressureRead?:
    | ((request: ProviderContextPressureReadRequestV1) => Promise<ProviderContextPressureReadV1>)
    | null;
  topologyWaveCurrentRead?:
    | ((request: TopologyWaveCurrentReadRequestV1) => Promise<TopologyWaveCurrentReadV1>)
    | null;
}>;

export type ConsoleProtocolBinding =
  | Readonly<{
      ok: true;
      port: ConsoleProtocolPort;
      readOnly: boolean;
      actions: OperatorActionClient | null;
      nativeNotificationProjection: "daemon-journal" | "feature-unavailable";
      runSessionProjection: "exact";
      compatibility: ConsoleProtocolCompatibility;
    }>
  | Readonly<{
      ok: false;
      missingFeatures: readonly string[];
    }>;

export type ConsoleProtocolCompatibility = Readonly<{ mode: "current" }>;

export type ConsoleSessionCompatibility = Readonly<{ mode: "current" }>;

export function bindConsoleProtocolClient(
  client: NegotiatedOperatorClient,
  sessionCompatibility?: ConsoleSessionCompatibility,
): ConsoleProtocolBinding {
  if (sessionCompatibility !== undefined && sessionCompatibility.mode !== "current") {
    return { ok: false, missingFeatures: ["current-protocol-baseline"] };
  }
  const available = new Set<string>(client.features);
  const missingFeatures = [
    "operator-projection.v2",
    "scoped-gate-read.v1",
    "run-session-projection.v1",
    "declared-run-progress.v2",
    "run-identity-projection.v2",
    ACTIVITY_NARRATIVE_GROUPING_FEATURE,
    "agent-topology-projection.v1",
    "work-facts-projection.v1",
    "run-scoped-projection.v1",
    "artifact-content-read.v1",
  ].filter((feature) => !available.has(feature));
  if (missingFeatures.length > 0) {
    return {
      ok: false,
      missingFeatures,
    };
  }
  if (
    client.projection === undefined ||
    client.console === undefined ||
    client.artifacts === undefined
  ) {
    return { ok: false, missingFeatures: ["operator-console-operations"] };
  }
  const projection = client.projection;
  const consoleClient = client.console;
  const nativeNotificationProjection = client.features.includes(
    NATIVE_NOTIFICATION_PROJECTION_FEATURE,
  ) ? "daemon-journal" : "feature-unavailable";
  const compatibility: ConsoleProtocolCompatibility = { mode: "current" };
  const artifacts = client.artifacts;
  const artifactRead: ArtifactContentClient["readContent"] =
    (request) => artifacts.readContent(request);
  return {
    ok: true,
    readOnly: consoleClient.readOnly,
    actions: consoleClient.readOnly ? null : consoleClient.actions,
    nativeNotificationProjection,
    runSessionProjection: "exact",
    compatibility,
    port: {
      snapshot: (request) => projection.snapshot(request),
      events: (request) => projection.events(request),
      viewPage: (request) => consoleClient.projection.viewPage(request),
      runPage: (request) => consoleClient.projection.runPage(request),
      readDetail: (request) => consoleClient.projection.readDetail(request),
      readGate: (request) => consoleClient.gates.read(request),
      readMessageBody: client.messages?.read ?? null,
      readRepository: client.repository?.read ?? null,
      readArtifactContent: artifactRead,
      reviewCompletionRead:
        client.operations?.[FABRIC_OPERATIONS.reviewCompletionRead] ?? null,
      reviewEvidenceList:
        client.operations?.[FABRIC_OPERATIONS.reviewEvidenceList] ?? null,
      providerRouteIntegrityRecoveryRead:
        client.operations?.[FABRIC_OPERATIONS.providerRouteIntegrityRecoveryRead] ?? null,
      providerContextPressureRead:
        client.operations?.[FABRIC_OPERATIONS.providerContextPressureRead] ?? null,
      topologyWaveCurrentRead:
        client.operations?.[FABRIC_OPERATIONS.topologyWaveCurrentRead] ?? null,
    },
  };
}

export type ConsoleConnection =
  | Readonly<{
      state: "live";
      compatibility: ConsoleProtocolCompatibility;
    }>
  | Readonly<{
      state: "degraded" | "unavailable";
      reason:
        | "transport-failure"
        | "projection-invalid"
        | "resnapshot-exhausted"
        | "bootstrap-unavailable";
    }>
  | Readonly<{
      state: "unsupported";
      missingFeatures: readonly string[];
    }>
  | Readonly<{
      state: "protocol-incompatible";
      code: "PROTOCOL_INCOMPATIBLE" | "CONSOLE_PROTOCOL_INCOMPATIBLE";
      message: string;
      operation: string | null;
      closedReason: string | null;
      primary: Readonly<{ code: string; message: string }>;
    }>;

function unavailableRepository(
  binding: ConsoleInspectionBinding,
  reason: Extract<ConsoleReadInspection, { kind: "repository"; state: "unavailable" }>["reason"],
): ConsoleReadInspection {
  return { kind: "repository", state: "unavailable", binding, reason };
}

function unavailableArtifact(
  binding: ConsoleInspectionBinding,
  reason: Extract<ConsoleReadInspection, { kind: "artifact"; state: "unavailable" }>["reason"],
): ConsoleReadInspection {
  return { kind: "artifact", state: "unavailable", binding, reason };
}

function failureCode(error: unknown): string | null {
  if (typeof error !== "object" || error === null) return null;
  const code = Reflect.get(error, "code");
  return typeof code === "string" ? code : null;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

const MAX_ARTIFACT_PAGES = 2_048;
const MAX_ARTIFACT_RENDERED_BYTES = 2_097_152;

function contentDigest(value: string): Sha256Digest {
  return `sha256:${createHash("sha256").update(value).digest("hex")}` as Sha256Digest;
}

function artifactLineCount(value: string): number {
  if (value.length === 0) return 0;
  let lines = 1;
  for (const byte of Buffer.from(value, "utf8")) {
    if (byte === 0x0a) lines += 1;
  }
  return lines;
}

function returnedPageLineCount(value: string): number {
  if (value.length === 0) return 0;
  const lines = artifactLineCount(value);
  return value.endsWith("\n") ? lines - 1 : lines;
}

function sameArtifactRef(left: ArtifactRef, right: ArtifactRef): boolean {
  return left.path === right.path && left.digest === right.digest;
}

function expectedLineFragment(input: {
  startsAtLineBoundary: boolean;
  endsAtLineBoundary: boolean;
}): ArtifactLineFragment {
  return input.startsAtLineBoundary
    ? input.endsAtLineBoundary ? "whole" : "start"
    : input.endsAtLineBoundary ? "end" : "middle";
}

function reviewDisposition(
  transformation: ArtifactContentTransformation,
): ConsoleArtifactContentResult["reviewDisposition"] {
  if (transformation === "none") return "eligible";
  if (transformation === "terminal-neutralised") {
    return "confirm-terminal-neutralised";
  }
  return "blocked-redacted";
}

export type ConsoleClosedProjectionUnavailableReason =
  | "operation-not-negotiated"
  | "preparation-id-not-projected"
  | "operator-route-projection-unavailable"
  | "run-correlation-unavailable"
  | "read-error"
  | "contract-invalid";

export type ConsoleClosedProjectionRead<Value> =
  | Readonly<{ state: "current"; value: Value }>
  | Readonly<{
      state: "unavailable";
      reason: ConsoleClosedProjectionUnavailableReason;
      code: string | null;
    }>;

export type ConsoleClosedProjectionUnavailable = Extract<
  ConsoleClosedProjectionRead<never>,
  { state: "unavailable" }
>;

export type ConsoleReviewRunProjection = Readonly<{
  projectSessionId: ProjectSessionId;
  coordinationRunId: CoordinationRunId;
  preparation: ConsoleClosedProjectionRead<ReviewTargetPreparationReadV1>;
  completion: ConsoleClosedProjectionRead<ReviewCompletionV1>;
  evidence: ConsoleClosedProjectionRead<readonly ReviewEvidenceReadV1[]>;
  recoveries: readonly Readonly<{
    actionRef: ProviderActionRefV1;
    read: ConsoleClosedProjectionRead<ProviderRouteIntegrityRecoveryProjectionV1>;
  }>[];
  providerRoute: ConsoleClosedProjectionUnavailable;
  capabilityFreshness: ConsoleClosedProjectionUnavailable;
}>;

export type ConsoleTopologyProjection = Readonly<{
  taskId: TaskId;
  coordinationRunId: CoordinationRunId | null;
  read: ConsoleClosedProjectionRead<TopologyWaveCurrentReadV1>;
}>;

export type ConsoleContextPressureProjection = Readonly<{
  agentId: AgentId;
  coordinationRunId: CoordinationRunId | null;
  read: ConsoleClosedProjectionRead<ProviderContextPressureReadV1>;
}>;

export type ConsoleReviewProjections = Readonly<{
  reviewRuns: readonly ConsoleReviewRunProjection[];
  topology: readonly ConsoleTopologyProjection[];
  contextPressure: readonly ConsoleContextPressureProjection[];
}>;

export type FabricConsoleDataset = Readonly<{
  connection: ConsoleConnection;
  connectionDiagnosis?: ConsoleConnectionDiagnosis;
  snapshot: OperatorProjectionSnapshot | null;
  snapshotRevision: Revision | null;
  cursor: number;
  pages: ConsoleViewPages;
  loadedAtMs: number;
  canMutate: boolean;
  projectSessions?: Readonly<{
    choices: readonly ProjectSessionDiscovery[];
    selectedProjectSessionId: ProjectSessionId | null;
  }>;
  workflowCapabilities?: ConsoleWorkflowCapabilities;
  productionActionPlanning?: true;
  inspection?: ConsoleReadInspection;
  review?: ConsoleReviewProjections;
}>;

export type ConsoleProtocolAdapterOptions = Readonly<{
  binding: ConsoleProtocolBinding;
  credential: OperatorCapabilityCredential;
  projectId: ProjectId;
  projectSessionId?: ProjectSessionId;
  now?: () => number;
  pageLimit?: number;
  eventLimit?: number;
  maxPagesPerView?: number;
  maxResnapshotAttempts?: number;
}>;

const MAX_CLOSED_PROJECTION_PAGES = 100;

type ReviewCompletionActionShape = Readonly<{
  slots: readonly Readonly<{ actionRef: ProviderActionRefV1 | null }>[];
}>;

type ReviewEvidenceListPageShape = Readonly<{
  entries: readonly ReviewEvidenceReadV1[];
  nextCursor: string | null;
}>;

function closedProjectionUnavailable(
  reason: ConsoleClosedProjectionUnavailableReason,
  code: string | null = null,
): ConsoleClosedProjectionUnavailable {
  return { state: "unavailable", reason, code };
}

/**
 * Truthful System-view rendering for each bootstrap-unavailable reason. The
 * transport axis stays `bootstrap-unavailable` (one honest connection axis),
 * while the System `daemon` row names the exact causal finding and its bounded
 * remediation instead of collapsing every daemon/transport failure into a
 * generic message. Every detail is a safe, non-secret operator summary.
 *
 * Text derives from `BOOTSTRAP_FAILURE_DETAILS` (connection-diagnosis.ts),
 * the single source of per-reason narrative text, so the two files never
 * maintain independent prose for the same 11 reasons. A few reasons keep a
 * distinct operator-facing phrasing (e.g. an all-caps cutover alert) via
 * `DETAIL_OVERRIDES`.
 */
const DETAIL_OVERRIDES: Partial<Record<BootstrapUnavailableReason, string>> = {
  "schema-cutover-required": "CUTOVER REQUIRED — existing database preserved",
  "daemon-unreachable":
    "no reachable Fabric daemon — the daemon socket is absent or unresponsive; retry to start it",
};

const BOOTSTRAP_UNAVAILABLE_DETAILS: Record<
  BootstrapUnavailableReason,
  { kind: SystemViewItem["kind"]; detail: string }
> = Object.fromEntries(
  (Object.keys(BOOTSTRAP_FAILURE_DETAILS) as BootstrapUnavailableReason[]).map(
    (reason) => {
      const { summary, remediation } = BOOTSTRAP_FAILURE_DETAILS[reason];
      return [
        reason,
        {
          kind: "daemon" as const,
          detail: DETAIL_OVERRIDES[reason] ?? `${summary} — ${remediation}`,
        },
      ];
    },
  ),
) as Record<BootstrapUnavailableReason, { kind: SystemViewItem["kind"]; detail: string }>;

export function createBootstrapUnavailableDataset(
  reason: BootstrapUnavailableReason,
  nowMs = Date.now(),
): FabricConsoleDataset {
  const pages = createEmptyViewPages();
  const revision = revisionFromProtocol(0);
  const diagnosis = createConnectionDiagnosis(reason, nowMs);
  const stageRows = connectionDiagnosisRows(diagnosis, nowMs).map((stage) => ({
    view: "system" as const,
    ...stage,
    detailRef: null,
    actionAvailability: {
      state: "read-only" as const,
      reason: "fact-unavailable" as const,
    },
  }));
  return {
    connection: { state: "unavailable", reason: "bootstrap-unavailable" },
    connectionDiagnosis: diagnosis,
    snapshot: null,
    snapshotRevision: null,
    cursor: 0,
    pages: {
      ...pages,
      system: {
        view: "system",
        rows: [
          {
            view: "system" as const,
            stableId: "bootstrap",
            revision,
            urgency: "safety-integrity" as const,
            freshness: {
              state: "unavailable" as const,
              source: "fabric" as const,
              revision,
              observedAt: new Date(nowMs).toISOString() as never,
              ageMs: 0,
              reason,
            },
            summary: {
              kind: "system" as const,
              systemKind: BOOTSTRAP_UNAVAILABLE_DETAILS[reason].kind,
              state: "unavailable" as const,
              detail: BOOTSTRAP_UNAVAILABLE_DETAILS[reason].detail,
            },
            detailRef: null,
            actionAvailability: {
              state: "read-only" as const,
              reason: "fact-unavailable" as const,
            },
          },
          ...stageRows,
        ],
        nextCursor: stageRows.length + 1,
        hasMore: false,
        snapshotRevision: null,
        readTransactionId: null,
      },
    },
    loadedAtMs: nowMs,
    canMutate: false,
  };
}

export function createProtocolIncompatibleDataset(
  input: Readonly<{
    primary: Readonly<{ code: string; message: string }>;
    result?: Readonly<{
      operation?: string;
      closedReason?: string;
      message?: string;
    }>;
  }>,
  nowMs = Date.now(),
): FabricConsoleDataset {
  return {
    connection: {
      state: "protocol-incompatible",
      code: "CONSOLE_PROTOCOL_INCOMPATIBLE",
      message: input.result?.message ?? input.primary.message,
      operation: input.result?.operation ?? null,
      closedReason: input.result?.closedReason ?? null,
      primary: input.primary,
    },
    snapshot: null,
    snapshotRevision: null,
    cursor: 0,
    pages: createEmptyViewPages(),
    loadedAtMs: nowMs,
    canMutate: false,
  };
}

class ResnapshotRequiredError extends Error {
  constructor() {
    super("projection resnapshot required");
  }
}

class ProjectionInvalidError extends Error {
  constructor(message: string) {
    super(message);
  }
}

function boundedPositiveInteger(
  value: number | undefined,
  fallback: number,
  maximum: number,
): number {
  if (value === undefined) {
    return fallback;
  }
  if (!Number.isSafeInteger(value) || value < 1 || value > maximum) {
    throw new TypeError(`limit must be an integer from 1 to ${String(maximum)}`);
  }
  return value;
}

export class ConsoleProtocolAdapter {
  readonly #binding: ConsoleProtocolBinding;
  readonly #credential: OperatorCapabilityCredential;
  readonly #projectId: ProjectId;
  readonly #projectSessionId: ProjectSessionId | undefined;
  readonly #now: () => number;
  readonly #pageLimit: number;
  readonly #eventLimit: number;
  readonly #maxPagesPerView: number;
  readonly #maxResnapshotAttempts: number;
  #lastGood: FabricConsoleDataset | null = null;

  constructor(options: ConsoleProtocolAdapterOptions) {
    this.#binding = options.binding;
    this.#credential = options.credential;
    this.#projectId = options.projectId;
    this.#projectSessionId = options.projectSessionId;
    this.#now = options.now ?? Date.now;
    this.#pageLimit = boundedPositiveInteger(options.pageLimit, 100, 1_000);
    this.#eventLimit = boundedPositiveInteger(options.eventLimit, 100, 1_000);
    this.#maxPagesPerView = boundedPositiveInteger(
      options.maxPagesPerView,
      100,
      1_000,
    );
    this.#maxResnapshotAttempts = boundedPositiveInteger(
      options.maxResnapshotAttempts,
      3,
      10,
    );
  }

  get actionClient(): OperatorActionClient | null {
    if (
      this.#binding.ok === false ||
      this.#binding.readOnly ||
      this.#lastGood?.connection.state !== "live"
    ) {
      return null;
    }
    return this.#binding.actions;
  }

  get port(): ConsoleProtocolPort | null {
    return this.#binding.ok ? this.#binding.port : null;
  }

  async open(): Promise<FabricConsoleDataset> {
    if (!this.#binding.ok) {
      return this.#unsupported();
    }
    return this.#loadWithFallback();
  }

  async refresh(): Promise<FabricConsoleDataset> {
    return this.open();
  }

  async poll(): Promise<FabricConsoleDataset> {
    if (!this.#binding.ok) {
      return this.#unsupported();
    }
    if (this.#lastGood?.snapshotRevision === null || this.#lastGood === null) {
      return this.#loadWithFallback();
    }
    try {
      const result = await this.#binding.port.events({
        ...this.#readScope(),
        after: this.#lastGood.cursor,
        limit: this.#eventLimit,
      });
      if (result.status === "resnapshot-required") {
        return this.#loadWithFallback();
      }
      if (
        result.events.length > 0 ||
        result.snapshotRevision !==
          revisionToProtocol(this.#lastGood.snapshotRevision)
      ) {
        return this.#loadWithFallback();
      }
      const review = await this.#loadReviewProjections(this.#lastGood.pages);
      const current: FabricConsoleDataset = {
        ...this.#lastGood,
        cursor: result.nextCursor,
        connection: {
          state: "live",
          compatibility: this.#binding.compatibility,
        },
        review,
        loadedAtMs: this.#now(),
        canMutate: !this.#binding.readOnly && this.#binding.actions !== null,
      };
      this.#lastGood = current;
      return current;
    } catch (error) {
      return this.#fallbackFor(error);
    }
  }

  async inspect(binding: ConsoleInspectionBinding): Promise<ConsoleReadInspection | null> {
    const dataset = this.#lastGood;
    if (binding.runTarget !== undefined) {
      const run = await inspectRunProjection({
        currentSnapshotRevision: dataset?.snapshotRevision ?? null,
        selectedProjectSessionId: this.#projectSessionId,
        bindingProjectSessionId: binding.projectSessionId,
        bindingTarget: binding.runTarget,
        bindingRevision: binding.projectionRevision,
        protocolSnapshotRevision: revisionToProtocol(binding.projectionRevision),
        port: this.#binding.ok ? this.#binding.port : null,
        readScope: this.#readScope(),
        pageLimit: this.#pageLimit,
        maximumPagesPerSection: this.#maxPagesPerView,
      });
      return { kind: "run", binding, ...run };
    }
    if (binding.view === "evidence") {
      const row = dataset?.pages.evidence.rows.find(
        (candidate) => candidate.stableId === binding.itemId,
      );
      if (
        dataset === null ||
        dataset?.snapshotRevision !== binding.projectionRevision ||
        row?.revision !== binding.itemRevision ||
        row.detailRef === null
      ) {
        return unavailableArtifact(binding, "projection-changed");
      }
      if (!this.#binding.ok) return unavailableArtifact(binding, "feature-unavailable");
      try {
        const detail = await this.#binding.port.readDetail({
          ...this.#readScope(),
          snapshotRevision: revisionToProtocol(binding.projectionRevision),
          detailRef: row.detailRef,
        });
        if (detail.status === "resnapshot-required") {
          return unavailableArtifact(binding, "projection-changed");
        }
        if (
          detail.snapshotRevision !== revisionToProtocol(binding.projectionRevision) ||
          detail.detailRef.kind !== "evidence" ||
          detail.detailRef.evidenceId !== row.detailRef.evidenceId ||
          detail.detailRef.expectedRevision !== row.detailRef.expectedRevision ||
          detail.detail.revision !== row.detailRef.expectedRevision
        ) {
          return unavailableArtifact(binding, "contract-invalid");
        }
        if (detail.detail.freshness === "unavailable") {
          return unavailableArtifact(binding, "detail-unavailable");
        }
        if (detail.detail.freshness === "conflict") {
          return unavailableArtifact(binding, "detail-conflict");
        }
        const evidence = detail.detail.value;
        if (
          evidence.kind !== "evidence" ||
          evidence.evidenceId !== binding.itemId
        ) {
          return unavailableArtifact(binding, "detail-invalid");
        }
        const read = this.#binding.port.readArtifactContent;
        if (read === null) return unavailableArtifact(binding, "feature-unavailable");
        const maximumBytes = 131_072;
        const maximumLines = 2_000;
        const unavailableReasons = {
          "not-found": "artifact-not-found",
          forbidden: "artifact-forbidden",
          "unsupported-media": "artifact-unsupported-media",
          "unsafe-content": "artifact-unsafe-content",
          stale: "artifact-stale",
          oversized: "artifact-oversized",
        } as const;
        const pages: ConsoleArtifactContentPage[] = [];
        const renderedParts: string[] = [];
        const seenCursors = new Set<string>();
        let cursor: string | null = null;
        let expectedPageIndex = 0;
        let previousEndedAtLineBoundary = true;
        let firstPage: Extract<ArtifactContentReadResult, { available: true }> | null = null;
        for (; expectedPageIndex < MAX_ARTIFACT_PAGES; expectedPageIndex += 1) {
          const rawResult: unknown = await read({
            ...this.#readScope(),
            evidenceId: evidence.evidenceId,
            expectedEvidenceRevision: revisionToProtocol(binding.itemRevision),
            artifactRef: evidence.artifactRef,
            cursor,
            maximumBytes,
            maximumLines,
          });
          let result: ArtifactContentReadResult;
          try {
            result = parseArtifactContentReadResult(rawResult);
          } catch {
            return unavailableArtifact(binding, "contract-invalid");
          }
          if (!sameArtifactRef(result.artifactRef, evidence.artifactRef)) {
            return unavailableArtifact(binding, "contract-invalid");
          }
          if (!result.available) {
            return unavailableArtifact(binding, unavailableReasons[result.reason]);
          }
          const pageBytes = Buffer.byteLength(result.content, "utf8");
          const finalPage = result.nextCursor === null;
          if (
            result.pageIndex !== expectedPageIndex ||
            contentDigest(result.content) !== result.pageContentDigest ||
            pageBytes > maximumBytes ||
            returnedPageLineCount(result.content) > maximumLines ||
            (result.content.length === 0 && !finalPage) ||
            result.lineFragment !== expectedLineFragment({
              startsAtLineBoundary: previousEndedAtLineBoundary,
              endsAtLineBoundary: finalPage || result.content.endsWith("\n"),
            })
          ) {
            return unavailableArtifact(binding, "contract-invalid");
          }
          if (firstPage === null) {
            firstPage = result;
            if (
              result.renderedTotalBytes > MAX_ARTIFACT_RENDERED_BYTES ||
              (result.transformation === "none" &&
                result.renderedArtifactDigest !== result.artifactRef.digest)
            ) {
              return unavailableArtifact(binding, "contract-invalid");
            }
          } else if (
            result.mediaType !== firstPage.mediaType ||
            result.totalBytes !== firstPage.totalBytes ||
            result.totalLines !== firstPage.totalLines ||
            result.renderedTotalBytes !== firstPage.renderedTotalBytes ||
            result.renderedTotalLines !== firstPage.renderedTotalLines ||
            result.renderedArtifactDigest !== firstPage.renderedArtifactDigest ||
            result.transformation !== firstPage.transformation ||
            result.terminalNeutralised !== firstPage.terminalNeutralised ||
            result.capabilityValuesRedacted !== firstPage.capabilityValuesRedacted ||
            result.credentialValuesRedacted !== firstPage.credentialValuesRedacted
          ) {
            return unavailableArtifact(binding, "contract-invalid");
          }
          pages.push({
            pageIndex: result.pageIndex,
            lineFragment: result.lineFragment,
            pageContentDigest: result.pageContentDigest,
            bytes: pageBytes,
          });
          renderedParts.push(result.content);
          previousEndedAtLineBoundary = result.content.endsWith("\n");
          const nextCursor = result.nextCursor;
          if (nextCursor === null) break;
          if (
            nextCursor === cursor ||
            seenCursors.has(nextCursor)
          ) {
            return unavailableArtifact(binding, "contract-invalid");
          }
          seenCursors.add(nextCursor);
          cursor = nextCursor;
        }
        const lastPage = pages.at(-1);
        if (firstPage === null || lastPage === undefined || expectedPageIndex >= MAX_ARTIFACT_PAGES) {
          return unavailableArtifact(binding, "contract-invalid");
        }
        const content = renderedParts.join("");
        if (
          Buffer.byteLength(content, "utf8") !== firstPage.renderedTotalBytes ||
          artifactLineCount(content) !== firstPage.renderedTotalLines ||
          contentDigest(content) !== firstPage.renderedArtifactDigest
        ) {
          return unavailableArtifact(binding, "contract-invalid");
        }
        const result: ConsoleArtifactContentResult = {
          artifactRef: firstPage.artifactRef,
          evidenceRevision: revisionToProtocol(binding.itemRevision),
          evidenceKind: evidence.evidenceKind,
          sourceKind: evidence.sourceKind,
          publisherKind: evidence.publisherKind,
          publisherRef: evidence.publisherRef,
          projectSessionId: evidence.projectSessionId,
          coordinationRunId: evidence.coordinationRunId,
          taskId: evidence.taskId,
          createdAt: evidence.createdAt,
          mediaType: firstPage.mediaType,
          content,
          totalBytes: firstPage.totalBytes,
          totalLines: firstPage.totalLines,
          renderedTotalBytes: firstPage.renderedTotalBytes,
          renderedTotalLines: firstPage.renderedTotalLines,
          renderedArtifactDigest: firstPage.renderedArtifactDigest,
          transformation: firstPage.transformation,
          terminalNeutralised: true,
          capabilityValuesRedacted: true,
          credentialValuesRedacted: true,
          pages,
          coverage: {
            complete: true,
            verified: true,
            pageCount: pages.length,
          },
          reviewDisposition: reviewDisposition(firstPage.transformation),
        };
        return {
          kind: "artifact",
          state: "current",
          binding,
          readTransactionId: detail.readTransactionId,
          result,
        };
      } catch (error: unknown) {
        const code = failureCode(error);
        return unavailableArtifact(
          binding,
          code === "STALE_REVISION" || code === "PROJECTION_RESNAPSHOT_REQUIRED"
            ? "projection-changed"
            : "transport-failure",
        );
      }
    }
    if (binding.view === "project") {
      const row = dataset?.pages.project.rows.find(
        (candidate) => candidate.stableId === binding.itemId,
      );
      if (
        dataset === null ||
        dataset?.snapshotRevision !== binding.projectionRevision ||
        row?.revision !== binding.itemRevision ||
        row.detailRef === null
      ) {
        return unavailableRepository(binding, "projection-changed");
      }
      if (!this.#binding.ok) return unavailableRepository(binding, "feature-unavailable");
      try {
        const detail = await this.#binding.port.readDetail({
          ...this.#readScope(),
          snapshotRevision: revisionToProtocol(binding.projectionRevision),
          detailRef: row.detailRef,
        });
        if (detail.status === "resnapshot-required") {
          return unavailableRepository(binding, "projection-changed");
        }
        if (
          detail.snapshotRevision !==
            revisionToProtocol(binding.projectionRevision) ||
          detail.detailRef.kind !== "project" ||
          detail.detailRef.projectId !== row.detailRef.projectId ||
          detail.detailRef.expectedRevision !== row.detailRef.expectedRevision ||
          detail.detail.revision !== row.detailRef.expectedRevision
        ) {
          return unavailableRepository(binding, "contract-invalid");
        }
        if (detail.detail.freshness === "unavailable") {
          return unavailableRepository(binding, "detail-unavailable");
        }
        if (detail.detail.freshness === "conflict") {
          return unavailableRepository(binding, "detail-conflict");
        }
        const project = detail.detail.value;
        if (project.kind !== "project" || project.projectId !== this.#projectId) {
          return unavailableRepository(binding, "detail-invalid");
        }
        const read = this.#binding.port.readRepository;
        if (read === null) return unavailableRepository(binding, "feature-unavailable");
        const selectedWorktree =
          project.repository !== undefined &&
          project.repository.canonicalWorktreePath !== project.canonicalRoot
            ? project.repository.canonicalWorktreePath
            : null;
        if (selectedWorktree !== null && this.#projectSessionId === undefined) {
          return unavailableRepository(binding, "detail-invalid");
        }
        const requestBase = {
          credential: this.#credential,
          projectId: this.#projectId,
          snapshotRevision: revisionToProtocol(binding.projectionRevision),
          diff: { kind: "working-tree" },
          log: { limit: 32 },
        } as const;
        let request: GitRepositoryReadRequest;
        if (selectedWorktree === null) {
          request = {
            ...requestBase,
            ...(this.#projectSessionId === undefined
              ? {}
              : { projectSessionId: this.#projectSessionId }),
            target: { kind: "project-root" },
          };
        } else {
          const projectSessionId = this.#projectSessionId;
          if (projectSessionId === undefined) {
            return unavailableRepository(binding, "detail-invalid");
          }
          request = {
            ...requestBase,
            projectSessionId,
            target: {
              kind: "session-worktree",
              canonicalWorktreePath: selectedWorktree,
            },
          };
        }
        const result = await read(request);
        if (result.status === "resnapshot-required") {
          return unavailableRepository(binding, "repository-resnapshot-required");
        }
        if (
          result.projectId !== this.#projectId ||
          result.projectSessionId !== (this.#projectSessionId ?? null) ||
          result.snapshotRevision !== revisionToProtocol(binding.projectionRevision) ||
          result.repository.canonicalRepositoryRoot !== project.canonicalRoot ||
          result.repository.canonicalWorktreePath !==
            (selectedWorktree ?? project.canonicalRoot)
        ) {
          return unavailableRepository(binding, "contract-invalid");
        }
        return {
          kind: "repository",
          state: "current",
          binding,
          readTransactionId: result.readTransactionId,
          repository: result.repository,
        };
      } catch (error: unknown) {
        const code = failureCode(error);
        return unavailableRepository(
          binding,
          code === "STALE_REVISION"
            ? "projection-changed"
            : code === "PROJECTION_RESNAPSHOT_REQUIRED"
              ? "repository-resnapshot-required"
              : "transport-failure",
        );
      }
    }
    if (binding.view !== "activity") return null;
    return inspectNarrativeActivity({
      binding,
      dataset,
      port: this.#binding.ok ? this.#binding.port : null,
      readScope: this.#readScope(),
    });
  }

  #readScope(): ProjectionSnapshotRequest {
    return {
      credential: this.#credential,
      projectId: this.#projectId,
      ...(this.#projectSessionId === undefined
        ? {}
        : { projectSessionId: this.#projectSessionId }),
    };
  }

  async #loadWithFallback(): Promise<FabricConsoleDataset> {
    try {
      const loaded = await this.#loadFresh();
      this.#lastGood = loaded;
      return loaded;
    } catch (error) {
      return this.#fallbackFor(error);
    }
  }

  async #loadFresh(): Promise<FabricConsoleDataset> {
    if (!this.#binding.ok) {
      return this.#unsupported();
    }
    for (let attempt = 0; attempt < this.#maxResnapshotAttempts; attempt += 1) {
      const snapshot = await this.#binding.port.snapshot(this.#readScope());
      const snapshotRevision = revisionFromProtocol(snapshot.snapshotRevision);
      try {
        const pages = await this.#loadPages(snapshot.snapshotRevision);
        const review = await this.#loadReviewProjections(pages);
        return {
          connection: {
            state: "live",
            compatibility: this.#binding.compatibility,
          },
          snapshot,
          snapshotRevision,
          cursor: snapshot.cursor,
          pages,
          review,
          loadedAtMs: this.#now(),
          canMutate:
            !this.#binding.readOnly && this.#binding.actions !== null,
        };
      } catch (error) {
        if (error instanceof ResnapshotRequiredError) {
          continue;
        }
        throw error;
      }
    }
    throw new ProjectionInvalidError("resnapshot attempts exhausted");
  }

  async #loadReviewProjections(
    pages: ConsoleViewPages,
  ): Promise<ConsoleReviewProjections> {
    if (!this.#binding.ok) {
      return { reviewRuns: [], topology: [], contextPressure: [] };
    }
    const runBindings = pages.runs.rows.flatMap((row) => {
      if (
        row.summary?.kind !== "run" ||
        row.summary.projectSessionId === undefined ||
        row.detailRef?.kind !== "run"
      ) {
        return [];
      }
      if (
        row.stableId !== row.detailRef.coordinationRunId ||
        (row.detailRef.projectSessionId !== undefined &&
          row.detailRef.projectSessionId !== row.summary.projectSessionId)
      ) {
        throw new ProjectionInvalidError(
          "run projection identity does not match its exact review scope",
        );
      }
      return [{
        projectSessionId: row.summary.projectSessionId,
        coordinationRunId: row.detailRef.coordinationRunId,
      }];
    });
    const uniqueRunBindings = runBindings.filter((candidate, index) =>
      runBindings.findIndex((other) =>
        other.projectSessionId === candidate.projectSessionId &&
        other.coordinationRunId === candidate.coordinationRunId
      ) === index
    );
    const reviewRuns = await Promise.all(
      uniqueRunBindings.map((scope) => this.#loadReviewRunProjection(scope)),
    );
    const exactRun = uniqueRunBindings.length === 1
      ? uniqueRunBindings[0] ?? null
      : null;
    const topology = await Promise.all(pages.work.rows.flatMap((row) => {
      if (row.detailRef?.kind !== "task") return [];
      const taskId = row.detailRef.taskId;
      if (exactRun === null) {
        return [Promise.resolve<ConsoleTopologyProjection>({
          taskId,
          coordinationRunId: null,
          read: closedProjectionUnavailable("run-correlation-unavailable"),
        })];
      }
      return [this.#readTopology(exactRun, taskId)];
    }));
    const contextPressure = await Promise.all(pages.agents.rows.flatMap((row) => {
      if (row.detailRef?.kind !== "agent") return [];
      const agentId = row.detailRef.agentId;
      if (exactRun === null) {
        return [Promise.resolve<ConsoleContextPressureProjection>({
          agentId,
          coordinationRunId: null,
          read: closedProjectionUnavailable("run-correlation-unavailable"),
        })];
      }
      return [this.#readContextPressure(exactRun, agentId)];
    }));
    return { reviewRuns, topology, contextPressure };
  }

  async #loadReviewRunProjection(scope: Readonly<{
    projectSessionId: ProjectSessionId;
    coordinationRunId: CoordinationRunId;
  }>): Promise<ConsoleReviewRunProjection> {
    const [completion, evidence] = await Promise.all([
      this.#readReviewCompletion(scope),
      this.#readReviewEvidence(scope),
    ]);
    const actionRefs = completion.state === "current"
      ? (completion.value as unknown as ReviewCompletionActionShape).slots.flatMap(({ actionRef }) =>
          actionRef === null ? [] : [actionRef]
        ).filter((candidate, index, all) =>
          all.findIndex((other) =>
            other.adapterId === candidate.adapterId &&
            other.actionId === candidate.actionId
          ) === index
        )
      : [];
    const recoveries = await Promise.all(actionRefs.map(async (actionRef) => ({
      actionRef,
      read: await this.#readRouteRecovery(scope, actionRef),
    })));
    return {
      ...scope,
      preparation: closedProjectionUnavailable(
        "preparation-id-not-projected",
      ),
      completion,
      evidence,
      recoveries,
      providerRoute: closedProjectionUnavailable(
        "operator-route-projection-unavailable",
      ),
      capabilityFreshness: closedProjectionUnavailable(
        "operator-route-projection-unavailable",
      ),
    };
  }

  async #readReviewCompletion(scope: Readonly<{
    projectSessionId: ProjectSessionId;
    coordinationRunId: CoordinationRunId;
  }>): Promise<ConsoleClosedProjectionRead<ReviewCompletionV1>> {
    if (!this.#binding.ok) {
      return closedProjectionUnavailable("operation-not-negotiated");
    }
    const read = this.#binding.port.reviewCompletionRead ?? null;
    if (read === null) {
      return closedProjectionUnavailable("operation-not-negotiated");
    }
    try {
      const result = await read({ schemaVersion: 1, ...scope });
      const code = failureCode(result);
      return code !== null
        ? closedProjectionUnavailable("read-error", code)
        : { state: "current", value: result };
    } catch (error) {
      return closedProjectionUnavailable(
        "read-error",
        failureCode(error) ?? "transport-failure",
      );
    }
  }

  async #readReviewEvidence(scope: Readonly<{
    projectSessionId: ProjectSessionId;
    coordinationRunId: CoordinationRunId;
  }>): Promise<ConsoleClosedProjectionRead<readonly ReviewEvidenceReadV1[]>> {
    if (!this.#binding.ok) {
      return closedProjectionUnavailable("operation-not-negotiated");
    }
    const read = this.#binding.port.reviewEvidenceList ?? null;
    if (read === null) {
      return closedProjectionUnavailable("operation-not-negotiated");
    }
    const entries: ReviewEvidenceReadV1[] = [];
    const seen = new Set<string>();
    let cursor: string | null = null;
    try {
      for (let page = 0; page < MAX_CLOSED_PROJECTION_PAGES; page += 1) {
        const result = await read({
          schemaVersion: 1,
          ...scope,
          targetGeneration: null,
          slot: null,
          pageSize: 100,
          cursor,
        });
        const code = failureCode(result);
        if (code !== null) {
          return closedProjectionUnavailable("read-error", code);
        }
        const pageResult = result as unknown as ReviewEvidenceListPageShape;
        entries.push(...pageResult.entries);
        const nextCursor = pageResult.nextCursor;
        if (nextCursor === null) return { state: "current", value: entries };
        if (nextCursor === cursor || seen.has(nextCursor)) {
          return closedProjectionUnavailable("contract-invalid", "cursor-cycle");
        }
        seen.add(nextCursor);
        cursor = nextCursor;
      }
      return closedProjectionUnavailable("contract-invalid", "page-limit");
    } catch (error) {
      return closedProjectionUnavailable(
        "read-error",
        failureCode(error) ?? "transport-failure",
      );
    }
  }

  async #readRouteRecovery(
    scope: Readonly<{
      projectSessionId: ProjectSessionId;
      coordinationRunId: CoordinationRunId;
    }>,
    actionRef: ProviderActionRefV1,
  ): Promise<ConsoleClosedProjectionRead<ProviderRouteIntegrityRecoveryProjectionV1>> {
    if (!this.#binding.ok) {
      return closedProjectionUnavailable("operation-not-negotiated");
    }
    const read = this.#binding.port.providerRouteIntegrityRecoveryRead ?? null;
    if (read === null) {
      return closedProjectionUnavailable("operation-not-negotiated");
    }
    try {
      const result = await read({ schemaVersion: 1, ...scope, actionRef });
      const code = failureCode(result);
      return code !== null
        ? closedProjectionUnavailable("read-error", code)
        : { state: "current", value: result };
    } catch (error) {
      return closedProjectionUnavailable(
        "read-error",
        failureCode(error) ?? "transport-failure",
      );
    }
  }

  async #readTopology(
    scope: Readonly<{
      projectSessionId: ProjectSessionId;
      coordinationRunId: CoordinationRunId;
    }>,
    taskId: TaskId,
  ): Promise<ConsoleTopologyProjection> {
    const read = this.#binding.ok
      ? this.#binding.port.topologyWaveCurrentRead ?? null
      : null;
    if (read === null) {
      return {
        taskId,
        coordinationRunId: scope.coordinationRunId,
        read: closedProjectionUnavailable("operation-not-negotiated"),
      };
    }
    try {
      return {
        taskId,
        coordinationRunId: scope.coordinationRunId,
        read: {
          state: "current",
          value: await read({ schemaVersion: 1, ...scope, taskId }),
        },
      };
    } catch (error) {
      return {
        taskId,
        coordinationRunId: scope.coordinationRunId,
        read: closedProjectionUnavailable(
          "read-error",
          failureCode(error) ?? "transport-failure",
        ),
      };
    }
  }

  async #readContextPressure(
    scope: Readonly<{
      projectSessionId: ProjectSessionId;
      coordinationRunId: CoordinationRunId;
    }>,
    agentId: AgentId,
  ): Promise<ConsoleContextPressureProjection> {
    const read = this.#binding.ok
      ? this.#binding.port.providerContextPressureRead ?? null
      : null;
    if (read === null) {
      return {
        agentId,
        coordinationRunId: scope.coordinationRunId,
        read: closedProjectionUnavailable("operation-not-negotiated"),
      };
    }
    try {
      return {
        agentId,
        coordinationRunId: scope.coordinationRunId,
        read: {
          state: "current",
          value: await read({ schemaVersion: 1, ...scope, agentId }),
        },
      };
    } catch (error) {
      return {
        agentId,
        coordinationRunId: scope.coordinationRunId,
        read: closedProjectionUnavailable(
          "read-error",
          failureCode(error) ?? "transport-failure",
        ),
      };
    }
  }

  async #loadPages(snapshotRevision: number): Promise<ConsoleViewPages> {
    const loaded = new Map<FabricView, ConsoleViewPage>();
    for (const view of FABRIC_VIEWS) {
      loaded.set(view, await this.#loadView(view, snapshotRevision));
    }
    return Object.fromEntries(loaded) as unknown as ConsoleViewPages;
  }

  async #loadView(
    view: FabricView,
    snapshotRevision: number,
  ): Promise<ConsoleViewPage> {
    if (!this.#binding.ok) {
      throw new ProjectionInvalidError("Console protocol is unavailable");
    }
    let cursor = 0;
    let readTransactionId: string | null = null;
    const rows: ConsoleRow[] = [];
    for (let pageNumber = 0; pageNumber < this.#maxPagesPerView; pageNumber += 1) {
      const result = await this.#binding.port.viewPage({
        ...this.#readScope(),
        view,
        snapshotRevision,
        cursor,
        limit: this.#pageLimit,
      });
      if (result.status === "resnapshot-required") {
        throw new ResnapshotRequiredError();
      }
      if (result.view !== view || result.snapshotRevision !== snapshotRevision) {
        throw new ProjectionInvalidError("projection page identity changed");
      }
      readTransactionId = result.readTransactionId;
      for (const row of result.rows) {
        try {
          rows.push(mapProtocolRow(
            view,
            row,
            this.#now(),
            this.#binding.nativeNotificationProjection,
          ));
        } catch (error: unknown) {
          // A row that violates a negotiated-shape invariant is an invalid
          // projection, not a transport failure.
          if (error instanceof TypeError) {
            throw new ProjectionInvalidError(error.message);
          }
          throw error;
        }
      }
      if (!result.hasMore) {
        return {
          view,
          rows: rankConsoleRows(rows),
          nextCursor: result.nextCursor,
          hasMore: false,
          snapshotRevision: revisionFromProtocol(snapshotRevision),
          readTransactionId,
        };
      }
      if (result.nextCursor <= cursor) {
        throw new ProjectionInvalidError("projection cursor did not advance");
      }
      cursor = result.nextCursor;
    }
    throw new ProjectionInvalidError("projection page limit exceeded");
  }

  #unsupported(): FabricConsoleDataset {
    const missingFeatures = this.#binding.ok
      ? []
      : this.#binding.missingFeatures;
    return {
      connection: { state: "unsupported", missingFeatures },
      snapshot: null,
      snapshotRevision: null,
      cursor: 0,
      pages: createEmptyViewPages(),
      loadedAtMs: this.#now(),
      canMutate: false,
    };
  }

  #fallbackFor(error: unknown): FabricConsoleDataset {
    const code = failureCode(error);
    if (code === "PROTOCOL_INCOMPATIBLE" || code === "CONSOLE_PROTOCOL_INCOMPATIBLE") {
      const cause = isRecord(error) && isRecord(error.cause) ? error.cause : null;
      const result = isRecord(error) && isRecord(error.result) ? error.result : null;
      const operation = result?.operation ?? cause?.operation;
      const closedReason = result?.closedReason ?? cause?.reason;
      const primaryValue = isRecord(error) && isRecord(error.primary) ? error.primary : null;
      const primary = primaryValue !== null &&
          typeof primaryValue.code === "string" &&
          typeof primaryValue.message === "string"
        ? { code: primaryValue.code, message: primaryValue.message }
        : {
            code,
            message: error instanceof Error ? error.message : "protocol result is incompatible",
          };
      return {
        connection: {
          state: "protocol-incompatible",
          code,
          message: error instanceof Error ? error.message : "protocol result is incompatible",
          operation: typeof operation === "string" ? operation : null,
          closedReason: typeof closedReason === "string" ? closedReason : null,
          primary,
        },
        snapshot: null,
        snapshotRevision: null,
        cursor: 0,
        pages: createEmptyViewPages(),
        loadedAtMs: this.#now(),
        canMutate: false,
      };
    }
    const reason =
      error instanceof ProjectionInvalidError
        ? error.message === "resnapshot attempts exhausted"
          ? "resnapshot-exhausted"
          : "projection-invalid"
        : "transport-failure";
    if (this.#lastGood === null) {
      return {
        connection: { state: "unavailable", reason },
        snapshot: null,
        snapshotRevision: null,
        cursor: 0,
        pages: createEmptyViewPages(),
        loadedAtMs: this.#now(),
        canMutate: false,
      };
    }
    return {
      ...this.#lastGood,
      connection: { state: "degraded", reason },
      loadedAtMs: this.#now(),
      canMutate: false,
    };
  }
}
