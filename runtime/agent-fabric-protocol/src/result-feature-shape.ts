import {
  ACTIVITY_NARRATIVE_GROUPING_FEATURE,
  GATE_SYSTEM_SUPERSESSION_FEATURE,
} from "./features.js";
import type { ScopedGate } from "./gates.js";
import { FABRIC_OPERATIONS } from "./operations.js";
import type {
  AttentionItem,
  OperatorDetail,
  OperatorDetailRef,
  OperatorDetailReadResult,
  OperatorProjectionSnapshot,
  OperatorViewPageResult,
  OperatorViewSummaryMap,
  ProjectionFact,
  ProjectionPageResult,
  RunProjectionPageResult,
} from "./projection.js";
import type { OperationResultMap, ProtocolOperation } from "./rpc-contract.js";

export const NATIVE_NOTIFICATION_PROJECTION_FEATURE =
  "native-notification-projection.v1" as const;
export const RUN_SESSION_PROJECTION_FEATURE = "run-session-projection.v1" as const;
export const DECLARED_RUN_PROGRESS_FEATURE = "declared-run-progress.v2" as const;
export const RUN_IDENTITY_PROJECTION_FEATURE = "run-identity-projection.v2" as const;
export const AGENT_TOPOLOGY_PROJECTION_FEATURE = "agent-topology-projection.v1" as const;
export const WORK_FACTS_PROJECTION_FEATURE = "work-facts-projection.v1" as const;
export const RUN_SCOPED_PROJECTION_FEATURE = "run-scoped-projection.v1" as const;
export const RUN_LIFECYCLE_FACTS_FEATURE = "run-lifecycle-facts.v1" as const;
export const MCP_BOOTSTRAP_CREDENTIALS_FEATURE =
  "mcp-bootstrap-credentials.v2" as const;
export const MCP_BOOTSTRAP_RESULT_SHAPE_FEATURE =
  "mcp-bootstrap-result-shape.v1" as const;

export class ProtocolResultShapeFeatureError extends TypeError {
  readonly code = "PROTOCOL_INCOMPATIBLE" as const;
  readonly missingFeatures: readonly string[];

  constructor(missingFeatures: readonly string[]) {
    super(`protocol is missing required result-shape features: ${missingFeatures.join(", ")}`);
    this.name = "ProtocolResultShapeFeatureError";
    this.missingFeatures = [...missingFeatures];
  }
}

export function assertRequiredResultShapeFeatures(
  features: readonly string[],
  requiredFeatures: readonly string[],
): void {
  const available = new Set(features);
  const missing = requiredFeatures.filter((feature) => !available.has(feature));
  if (missing.length > 0) throw new ProtocolResultShapeFeatureError(missing);
}

export type ProtocolResultShapeFailureReason =
  | "missing-negotiated-field"
  | "unnegotiated-field"
  | "mixed-presence";

export class ProtocolResultShapeError extends TypeError {
  readonly code = "PROTOCOL_INCOMPATIBLE" as const;
  readonly operation: ProtocolOperation;
  readonly reason: ProtocolResultShapeFailureReason;

  constructor(
    operation: ProtocolOperation,
    reason: ProtocolResultShapeFailureReason,
  ) {
    super(`protocol result for ${operation} has ${reason}`);
    this.name = "ProtocolResultShapeError";
    this.operation = operation;
    this.reason = reason;
  }
}

function factValues<Value>(fact: ProjectionFact<Value>): readonly Value[] {
  if (fact.freshness === "unavailable") return [];
  if (fact.freshness === "conflict") return fact.candidates;
  return [fact.value];
}

function snapshotAttention(result: OperatorProjectionSnapshot): readonly AttentionItem[] {
  return factValues(result.attention).flatMap((items) => items);
}

function projectionPageAttention(
  result: ProjectionPageResult,
): readonly AttentionItem[] {
  if (result.view !== "attention") return [];
  const page = result as ProjectionPageResult<"attention">;
  return factValues(page.page).flatMap((value) => value.items);
}

function viewPageAttention(
  result: OperatorViewPageResult,
): readonly OperatorViewSummaryMap["attention"][] {
  if (result.status !== "page" || result.view !== "attention") return [];
  const page = result as Extract<OperatorViewPageResult<"attention">, { status: "page" }>;
  return page.rows.flatMap((row) =>
    factValues(row.fact).map((candidate) => candidate.summary)
  );
}

function notificationPresence(
  operation: ProtocolOperation,
  result: OperationResultMap[ProtocolOperation],
): readonly boolean[] {
  if (operation === FABRIC_OPERATIONS.projectionSnapshot) {
    return snapshotAttention(
      result as OperationResultMap[typeof FABRIC_OPERATIONS.projectionSnapshot],
    ).map((item) => item.nativeNotification !== undefined);
  }
  if (operation === FABRIC_OPERATIONS.projectionPage) {
    return projectionPageAttention(
      result as OperationResultMap[typeof FABRIC_OPERATIONS.projectionPage],
    ).map((item) => item.nativeNotification !== undefined);
  }
  if (operation === FABRIC_OPERATIONS.projectionViewPage) {
    return viewPageAttention(
      result as OperationResultMap[typeof FABRIC_OPERATIONS.projectionViewPage],
    ).map((summary) => summary.nativeNotification !== undefined);
  }
  return [];
}

function runReferencePresence(reference: OperatorDetailRef): boolean[] {
  return reference.kind === "run"
    ? [reference.projectSessionId !== undefined]
    : [];
}

function runDetailPresence(detail: OperatorDetail): boolean[] {
  return detail.kind === "run"
    ? [detail.projectSessionId !== undefined]
    : [];
}

function runSessionPresence(
  operation: ProtocolOperation,
  result: OperationResultMap[ProtocolOperation],
): readonly boolean[] {
  if (operation === FABRIC_OPERATIONS.projectionSnapshot) {
    return factValues(
      (result as OperatorProjectionSnapshot).runs,
    ).flatMap((runs) => runs.map((run) => run.projectSessionId !== undefined));
  }
  if (operation === FABRIC_OPERATIONS.projectionPage) {
    const page = result as ProjectionPageResult;
    if (page.view !== "runs") return [];
    return factValues((page as ProjectionPageResult<"runs">).page)
      .flatMap((value) => value.items.map((run) => run.projectSessionId !== undefined));
  }
  if (operation === FABRIC_OPERATIONS.projectionViewPage) {
    const page = result as OperatorViewPageResult;
    if (page.status !== "page") return [];
    if (page.view === "runs") {
      const runs = page as Extract<OperatorViewPageResult<"runs">, { status: "page" }>;
      return runs.rows.flatMap((row) => factValues(row.fact).flatMap((value) => [
        value.detailRef.projectSessionId !== undefined,
        value.summary.projectSessionId !== undefined,
      ]));
    }
    if (page.view === "attention") {
      const attention = page as Extract<OperatorViewPageResult<"attention">, { status: "page" }>;
      return attention.rows.flatMap((row) => factValues(row.fact)
        .flatMap((value) => runReferencePresence(value.detailRef)));
    }
    return [];
  }
  if (operation === FABRIC_OPERATIONS.projectionDetailRead) {
    const read = result as OperatorDetailReadResult;
    if (read.status !== "current") return [];
    return [
      ...runReferencePresence(read.detailRef),
      ...factValues(read.detail).flatMap(runDetailPresence),
    ];
  }
  return [];
}

function declaredProgressPresence(
  operation: ProtocolOperation,
  result: OperationResultMap[ProtocolOperation],
): readonly boolean[] {
  if (operation === FABRIC_OPERATIONS.projectionViewPage) {
    const page = result as OperatorViewPageResult;
    if (page.status !== "page" || page.view !== "runs") return [];
    const runs = page as Extract<OperatorViewPageResult<"runs">, { status: "page" }>;
    return runs.rows.flatMap((row) => factValues(row.fact)
      .map((value) => value.summary.declaredProgress !== undefined));
  }
  if (operation === FABRIC_OPERATIONS.projectionDetailRead) {
    const read = result as OperatorDetailReadResult;
    if (read.status !== "current") return [];
    return factValues(read.detail).flatMap((detail) => (
      detail.kind === "run" ? [detail.declaredProgress !== undefined] : []
    ));
  }
  return [];
}

function runIdentityPresence(
  operation: ProtocolOperation,
  result: OperationResultMap[ProtocolOperation],
): readonly boolean[] {
  if (operation === FABRIC_OPERATIONS.projectionViewPage) {
    const page = result as OperatorViewPageResult;
    if (page.status !== "page" || page.view !== "runs") return [];
    const runs = page as Extract<OperatorViewPageResult<"runs">, { status: "page" }>;
    return runs.rows.flatMap((row) => factValues(row.fact)
      .map((value) => value.summary.identity !== undefined));
  }
  if (operation === FABRIC_OPERATIONS.projectionDetailRead) {
    const read = result as OperatorDetailReadResult;
    if (read.status !== "current") return [];
    return factValues(read.detail).flatMap((detail) => (
      detail.kind === "run" ? [detail.identity !== undefined] : []
    ));
  }
  return [];
}

function agentTopologyPresence(
  operation: ProtocolOperation,
  result: OperationResultMap[ProtocolOperation],
): readonly boolean[] {
  if (operation === FABRIC_OPERATIONS.projectionViewPage) {
    const page = result as OperatorViewPageResult;
    if (page.status !== "page" || page.view !== "agents") return [];
    const agents = page as Extract<OperatorViewPageResult<"agents">, { status: "page" }>;
    return agents.rows.flatMap((row) => factValues(row.fact)
      .map((value) => value.summary.topology !== undefined));
  }
  if (operation === FABRIC_OPERATIONS.projectionDetailRead) {
    const read = result as OperatorDetailReadResult;
    if (read.status !== "current") return [];
    return factValues(read.detail).flatMap((detail) => (
      detail.kind === "agent" ? [detail.topology !== undefined] : []
    ));
  }
  if (operation === FABRIC_OPERATIONS.projectionRunPage) {
    const page = result as RunProjectionPageResult;
    if (page.status !== "page" || page.section !== "agents") return [];
    const agents = page as Extract<RunProjectionPageResult<"agents">, { status: "page" }>;
    return agents.entries.flatMap((entry) => factValues(entry.value.fact)
      .map((value) => value.summary.topology !== undefined));
  }
  return [];
}

function workFactsPresence(
  operation: ProtocolOperation,
  result: OperationResultMap[ProtocolOperation],
): readonly boolean[] {
  if (operation === FABRIC_OPERATIONS.projectionViewPage) {
    const page = result as OperatorViewPageResult;
    if (page.status !== "page" || page.view !== "work") return [];
    const work = page as Extract<OperatorViewPageResult<"work">, { status: "page" }>;
    return work.rows.flatMap((row) => factValues(row.fact)
      .map((value) => value.summary.workflow !== undefined));
  }
  if (operation === FABRIC_OPERATIONS.projectionDetailRead) {
    const read = result as OperatorDetailReadResult;
    if (read.status !== "current") return [];
    return factValues(read.detail).flatMap((detail) => (
      detail.kind === "task" ? [detail.workflow !== undefined] : []
    ));
  }
  if (operation === FABRIC_OPERATIONS.projectionRunPage) {
    const page = result as RunProjectionPageResult;
    if (page.status !== "page" || page.section !== "work") return [];
    const work = page as Extract<RunProjectionPageResult<"work">, { status: "page" }>;
    return work.entries.flatMap((entry) => factValues(entry.value.fact)
      .map((value) => value.summary.workflow !== undefined));
  }
  return [];
}

function activityNarrativeGroupingPresence(
  operation: ProtocolOperation,
  result: OperationResultMap[ProtocolOperation],
): readonly boolean[] {
  if (operation === FABRIC_OPERATIONS.projectionPage) {
    const page = result as ProjectionPageResult;
    if (page.view !== "activity") return [];
    return factValues((page as ProjectionPageResult<"activity">).page)
      .flatMap((value) => value.items.map((item) => item.group !== undefined));
  }
  if (operation === FABRIC_OPERATIONS.projectionViewPage) {
    const page = result as OperatorViewPageResult;
    if (page.status !== "page" || page.view !== "activity") return [];
    const activity = page as Extract<
      OperatorViewPageResult<"activity">,
      { status: "page" }
    >;
    return activity.rows.flatMap((row) =>
      factValues(row.fact).flatMap((value) => [
        "group" in value.summary,
        "groupId" in value.detailRef,
      ])
    );
  }
  if (operation === FABRIC_OPERATIONS.projectionRunPage) {
    const page = result as RunProjectionPageResult;
    if (page.status !== "page" || page.section !== "activity") return [];
    const activity = page as Extract<
      RunProjectionPageResult<"activity">,
      { status: "page" }
    >;
    return activity.entries.flatMap((entry) =>
      factValues(entry.value.fact).flatMap((value) => [
        "group" in value.summary,
        "groupId" in value.detailRef,
      ])
    );
  }
  if (operation === FABRIC_OPERATIONS.projectionDetailRead) {
    const read = result as OperatorDetailReadResult;
    if (read.status !== "current" || read.detailRef.kind !== "activity") return [];
    if ("eventId" in read.detailRef && "groupId" in read.detailRef) {
      return factValues(read.detail).flatMap((detail) =>
        detail.kind === "activity"
          ? ["groupId" in detail && "eventId" in detail]
          : []
      );
    }
    return factValues(read.detail).flatMap((detail) =>
      detail.kind === "activity" ? ["group" in detail] : []
    );
  }
  return [];
}

function runScopedProjectionPresence(
  operation: ProtocolOperation,
  result: OperationResultMap[ProtocolOperation],
): readonly boolean[] {
  if (operation !== FABRIC_OPERATIONS.projectionRunPage) return [];
  const page = result as RunProjectionPageResult;
  if (page.status !== "page") return [];
  return [
    page.composition !== undefined,
    ...page.entries.map((entry) => entry.runScope !== undefined),
  ];
}

function runLifecycleFactsPresence(
  operation: ProtocolOperation,
  result: OperationResultMap[ProtocolOperation],
): readonly boolean[] {
  if (operation !== FABRIC_OPERATIONS.projectionRunPage) return [];
  const page = result as RunProjectionPageResult;
  if (
    page.status !== "page" ||
    page.composition === undefined ||
    page.composition.identity === undefined
  ) return [];
  const identity = factValues(page.composition.identity);
  const revisionPresence = identity.flatMap((value) => [
    value.lifecycleRevision !== undefined,
    value.progressRevision !== undefined,
  ]);
  if (page.section !== "agents") return revisionPresence;
  const agents = page as Extract<RunProjectionPageResult<"agents">, { status: "page" }>;
  return [
    ...revisionPresence,
    ...agents.entries.map((entry) => entry.classification !== undefined),
  ];
}

function assertUniformFeaturePresence(
  operation: ProtocolOperation,
  featureNegotiated: boolean,
  presence: readonly boolean[],
): void {
  if (presence.length === 0) return;
  const presentCount = presence.filter(Boolean).length;
  if (presentCount !== 0 && presentCount !== presence.length) {
    throw new ProtocolResultShapeError(operation, "mixed-presence");
  }
  if (featureNegotiated && presentCount === 0) {
    throw new ProtocolResultShapeError(operation, "missing-negotiated-field");
  }
  if (!featureNegotiated && presentCount > 0) {
    throw new ProtocolResultShapeError(operation, "unnegotiated-field");
  }
}

function gateResult(
  operation: ProtocolOperation,
  result: OperationResultMap[ProtocolOperation],
): ScopedGate | undefined {
  if (operation === FABRIC_OPERATIONS.scopedGateCreate || operation === FABRIC_OPERATIONS.scopedGateResolve) {
    return result as ScopedGate;
  }
  if (operation === FABRIC_OPERATIONS.scopedGateRead) {
    return (result as OperationResultMap[typeof FABRIC_OPERATIONS.scopedGateRead]).gate;
  }
  return undefined;
}

/** Restores the negotiated closed result shape after context-free wire decoding. */
export function assertOperationResultFeatureShape<Operation extends ProtocolOperation>(
  operation: Operation,
  features: readonly string[],
  result: OperationResultMap[Operation],
): OperationResultMap[Operation] {
  const gate = gateResult(operation, result as OperationResultMap[ProtocolOperation]);
  if (
    gate?.status === "superseded" &&
    gate.resolution.kind === "system-supersession" &&
    !features.includes(GATE_SYSTEM_SUPERSESSION_FEATURE)
  ) {
    throw new ProtocolResultShapeError(operation, "unnegotiated-field");
  }
  const presence = notificationPresence(
    operation,
    result as OperationResultMap[ProtocolOperation],
  );
  assertUniformFeaturePresence(
    operation,
    features.includes(NATIVE_NOTIFICATION_PROJECTION_FEATURE),
    presence,
  );
  assertUniformFeaturePresence(
    operation,
    features.includes(RUN_SESSION_PROJECTION_FEATURE),
    runSessionPresence(operation, result as OperationResultMap[ProtocolOperation]),
  );
  assertUniformFeaturePresence(
    operation,
    features.includes(DECLARED_RUN_PROGRESS_FEATURE),
    declaredProgressPresence(operation, result as OperationResultMap[ProtocolOperation]),
  );
  assertUniformFeaturePresence(
    operation,
    features.includes(RUN_IDENTITY_PROJECTION_FEATURE),
    runIdentityPresence(operation, result as OperationResultMap[ProtocolOperation]),
  );
  assertUniformFeaturePresence(
    operation,
    features.includes(AGENT_TOPOLOGY_PROJECTION_FEATURE),
    agentTopologyPresence(operation, result as OperationResultMap[ProtocolOperation]),
  );
  assertUniformFeaturePresence(
    operation,
    features.includes(WORK_FACTS_PROJECTION_FEATURE),
    workFactsPresence(operation, result as OperationResultMap[ProtocolOperation]),
  );
  assertUniformFeaturePresence(
    operation,
    features.includes(ACTIVITY_NARRATIVE_GROUPING_FEATURE),
    activityNarrativeGroupingPresence(
      operation,
      result as OperationResultMap[ProtocolOperation],
    ),
  );
  assertUniformFeaturePresence(
    operation,
    features.includes(RUN_SCOPED_PROJECTION_FEATURE),
    runScopedProjectionPresence(operation, result as OperationResultMap[ProtocolOperation]),
  );
  assertUniformFeaturePresence(
    operation,
    features.includes(RUN_LIFECYCLE_FACTS_FEATURE),
    runLifecycleFactsPresence(operation, result as OperationResultMap[ProtocolOperation]),
  );
  return result;
}
