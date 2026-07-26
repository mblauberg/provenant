import type { ProjectionFact } from "@local/agent-fabric-protocol";
import {
  declaredProgressCompactLabel,
  runDetailLines,
  runIdentityCompactLabel,
} from "./run-presentation.js";
import type { ConsoleControllerState } from "./controller.js";
import type {
  ConsoleFreshness,
  ConsoleRow,
  FabricView,
} from "./model.js";
import type { FabricConsoleDataset } from "./protocol-adapter.js";
import { connectionDiagnosisDetailLines } from "./connection-diagnosis.js";
import type {
  PresentedDetail,
  PresentedHeader,
  PresentedRow,
} from "./presenter-model.js";
import { urgencyMarkerFor } from "./theme.js";
import { isNeedsYouRow, recordAttentionLane } from "./attention-classification.js";
import {
  activityGroupDetailLines,
  activityGroupSecondary,
} from "./activity-presentation.js";
import { capacityLabel } from "./header-presentation.js";
export { isNeedsYouRow, isNeedsYouUrgency } from "./attention-classification.js";
export function titleCase(view: FabricView): string {
  return `${view.slice(0, 1).toUpperCase()}${view.slice(1)}`;
}
function factState<T>(
  fact: ProjectionFact<T> | undefined,
): PresentedHeader["freshness"] {
  return fact?.freshness ?? "unavailable";
}
function factValue<T>(fact: ProjectionFact<T> | undefined): T | null {
  return fact !== undefined &&
    (fact.freshness === "live" ||
      fact.freshness === "snapshot" ||
      fact.freshness === "stale")
    ? fact.value
    : null;
}

const FACT_SEVERITY: Readonly<Record<PresentedHeader["freshness"], number>> = {
  live: 0,
  snapshot: 1,
  stale: 2,
  conflict: 3,
  unavailable: 4,
};

function headerFreshness(
  dataset: FabricConsoleDataset,
): PresentedHeader["freshness"] {
  if (dataset.connection.state === "unsupported" || dataset.snapshot === null) {
    return "unavailable";
  }
  const states = [
    factState(dataset.snapshot.project),
    factState(dataset.snapshot.session),
    factState(dataset.snapshot.runs),
  ];
  if (dataset.connection.state !== "live") states.push("stale");
  return states.sort((left, right) => FACT_SEVERITY[right] - FACT_SEVERITY[left])[0] ?? "unavailable";
}

export function presentHeader(
  dataset: FabricConsoleDataset,
  attentionCounts: Readonly<{ needsYou: number; watch: number }>,
  selectedRun: ConsoleRow<"runs"> | null,
): PresentedHeader {
  const project = factValue(dataset.snapshot?.project);
  const session = factValue(dataset.snapshot?.session);
  const runs = factValue(dataset.snapshot?.runs) ?? [];
  const selectedSummary = selectedRun?.summary?.kind === "run"
    ? selectedRun.summary
    : null;
  const soleRun = runs.length === 1 ? runs[0] : undefined;
  const attentionCount = attentionCounts.needsYou;
  const sessionChoices = dataset.projectSessions?.choices ?? [];
  return {
    project: project?.projectId ?? "unavailable",
    session: session?.projectSessionId ?? (
      sessionChoices.length === 0
        ? "none"
        : `choose:${String(sessionChoices.length)}`
    ),
    run: selectedRun?.stableId ?? soleRun?.runId ?? (
      runs.length === 0 ? "none" : `choose:${String(runs.length)}`
    ),
    revision: dataset.snapshotRevision,
    freshness: headerFreshness(dataset),
    phase: selectedSummary?.phase ?? soleRun?.phase ?? (runs.length > 1 ? "not selected" : "unknown"),
    owner: selectedSummary === null
      ? soleRun?.chairAgentId ?? "unassigned"
      : String(selectedSummary.identity.chairAgentId),
    nextMilestone: selectedSummary?.nextMilestone ?? soleRun?.nextMilestone ?? "not declared",
    health: selectedSummary?.health ?? soleRun?.health ?? "unknown",
    attentionCount,
    needsYouCount: attentionCounts.needsYou,
    watchCount: attentionCounts.watch,
    runCount: runs.length,
    capacity: capacityLabel(dataset),
  };
}

function ageLabel(ageMs: number): string {
  const boundedAgeMs = Number.isFinite(ageMs) ? Math.max(0, ageMs) : 0;
  if (boundedAgeMs < 1_000) return "now";
  const seconds = Math.floor(boundedAgeMs / 1_000);
  if (seconds < 60) return `${String(seconds)}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${String(minutes)}m`;
  const hours = Math.floor(minutes / 60);
  return `${String(hours)}h`;
}

function ageAtRender(
  observedAt: string,
  minimumAgeMs: number,
  dataset: FabricConsoleDataset,
): number {
  const observedAtMs = Date.parse(observedAt);
  const derivedAgeMs = Number.isFinite(observedAtMs) &&
      Number.isFinite(dataset.loadedAtMs)
    ? dataset.loadedAtMs - observedAtMs
    : 0;
  return Math.max(0, minimumAgeMs, derivedAgeMs);
}

function freshnessLabel(
  freshness: ConsoleFreshness,
  dataset: FabricConsoleDataset,
): string {
  return `${freshness.state.toUpperCase()} ${
    ageLabel(ageAtRender(freshness.observedAt, freshness.ageMs, dataset))
  }`;
}

function attentionGroupingLabel(
  row: ConsoleRow,
  dataset: FabricConsoleDataset,
): string | null {
  if (row.view !== "attention") return null;
  const item = factValue(dataset.snapshot?.attention)?.find(
    (candidate) => candidate.itemId === row.stableId,
  );
  if (item === undefined) return null;
  return `x${String(item.duplicateCount)} grouped | source ${item.sourceFreshness} | last event ${
    ageLabel(ageAtRender(item.lastEventAt, 0, dataset))
  }`;
}

function summaryText(
  row: ConsoleRow,
  dataset: FabricConsoleDataset,
): readonly [string, string] {
  const summary = row.summary;
  if (row.view === "attention") {
    recordAttentionLane(row, summary?.kind === "attention" ? summary.label : null);
  }
  if (summary === null) {
    const reason =
      row.freshness.state === "unavailable"
        ? row.freshness.reason
        : row.freshness.state === "conflict"
          ? `${String(row.freshness.candidateCount)} conflicting candidates`
          : "detail unavailable";
    return [row.stableId, reason];
  }
  switch (summary.kind) {
    case "attention":
      const grouping = attentionGroupingLabel(row, dataset);
      const groupingSuffix = grouping === null ? "" : ` | ${grouping}`;
      if (summary.nativeNotification.kind === "feature-unavailable") {
        return [
          summary.title,
          `${summary.label} | ${summary.priority} | notify unavailable/feature-not-negotiated${groupingSuffix}`,
        ];
      }
      return [
        summary.title,
        `${summary.label} | ${summary.priority} | notify ${summary.nativeNotification.status}/${summary.nativeNotification.journalState}${groupingSuffix}`,
      ];
    case "project":
      return [
        summary.goal,
        summary.acceptedScopeRef === null
          ? `scope unaccepted | repository ${summary.repositoryRevision}`
          : `scope ${summary.acceptedScopeRef.path}@${summary.acceptedScopeRef.digest} | repository ${summary.repositoryRevision}`,
      ];
    case "run":
      if (summary.projectSessionId === undefined) {
        throw new TypeError("exact run projection has no project-session identity");
      }
      return [
        `${summary.projectSessionId} | ${summary.phase}`,
        `${runIdentityCompactLabel(summary.identity)} | ${summary.health} | next ${summary.nextMilestone} | ${
          declaredProgressCompactLabel(summary.declaredProgress)
        }`,
      ];
    case "work":
      return [summary.state, `checks ${summary.checkState}`];
    case "agent":
      return [summary.role, `${summary.lifecycle} | context ${summary.contextPressure}`];
    case "evidence":
      return [summary.evidenceKind, `${summary.status} | ${summary.provenance}`];
    case "activity":
      return [summary.summary, activityGroupSecondary(summary)];
    case "system":
      return [summary.systemKind, `${summary.state} | ${summary.detail}`];
  }
}

export function presentRow(
  row: ConsoleRow,
  selected: boolean,
  canMutate: boolean,
  dataset: FabricConsoleDataset,
): PresentedRow {
  const [primary, secondary] = summaryText(row, dataset);
  return {
    view: row.view,
    stableId: row.stableId,
    revision: row.revision,
    selected,
    urgencyMarker: urgencyMarkerFor(row.urgency, row.freshness.state),
    primary,
    secondary,
    freshness: freshnessLabel(row.freshness, dataset),
    actionable:
      canMutate &&
      row.freshness.state === "live" &&
      row.actionAvailability.state === "available",
  };
}

type DetailLine = Readonly<{ label: string; value: string }>;

type ReviewPreparationShape = Readonly<{
  state: string;
  phase: string;
  revision: number;
  accepted: Readonly<{
    preparationId: string;
    taskId: string;
    reservedTargetGeneration: number;
  }>;
  progress:
    | Readonly<{ kind: "phase-only" }>
    | Readonly<{
        kind: "finite";
        completed: number;
        total: number;
        planDigest: string;
      }>;
  terminal:
    | null
    | Readonly<{ kind: "succeeded"; targetRef: number }>
    | Readonly<{
        kind: "conflicted" | "failed";
        code: string;
        evidenceDigest: string;
      }>;
}>;

type ReviewCompletionShape = Readonly<{
  blockers: readonly string[];
  targetGeneration: number | null;
  targetChair: null | Readonly<{
    agentId: string;
    bindingGeneration: number;
    principalGeneration: number;
    providerSessionGeneration: number;
    modelFamily: string;
    model: string;
  }>;
  reviewedArtifactRef: string | null;
  publicationLineageDigest: string | null;
  bundleDigest: string | null;
  manifestRootDigest: string | null;
  coverageDigest: string | null;
  riskReadMapDigest: string | null;
  mandatoryReadSetDigest: string | null;
  profileDigest: string | null;
  unavailableSlots: readonly Readonly<{
    slot: string;
    reason: string;
    endpointProvider: string;
    providerFamily: string;
    model: string;
    availabilityRevision: number;
  }>[];
  slots: readonly Readonly<{
    slot: string;
    verdict: string | null;
    certifying: boolean;
    endpointProvider: string;
    providerFamily: string;
    model: string;
    actualRouteIdentityDigest: string | null;
    blockers: readonly string[];
  }>[];
  finalReviewComplete: boolean;
}>;

type TopologyCurrentShape =
  | Readonly<{ currency: "unavailable"; plan: null; pointer: null }>
  | Readonly<{
      currency: "current" | "stale";
      pointer: Readonly<{ revision: number; planDigest: string }>;
      plan: Readonly<{
        waveId: string;
        waveRevision: number;
        state: string;
        predecessor: null | Readonly<{
          waveId: string;
          waveRevision: number;
          planDigest: string;
        }>;
        dependencies: readonly Readonly<{
          dependencyTaskId: string;
          requiredState: string;
          evidenceRef: string;
        }>[];
        decomposability: Readonly<{ kind: string; evidenceRef: string }>;
        topology: Readonly<{
          executionShape: string;
          mode: string;
          maximumConcurrentAgents: number;
        }>;
        chair: Readonly<{
          agentId: string;
          principalGeneration: number;
          chairLeaseGeneration: number;
        }>;
        stageOwners: readonly Readonly<{
          stageId: string;
          taskId: string;
          ownerAgentId: string;
          writePartitionId: string | null;
        }>[];
        writePartitions: readonly Readonly<{
          partitionId: string;
          ownerAgentId: string;
          mode: string;
          pathSetDigest: string;
          authorityRef: string;
        }>[];
        contention: Readonly<{
          mode: string;
          serializationOwnerAgentId: string | null;
          evidenceRef: string;
        }>;
        budget: Readonly<{
          providerTurns: number;
          toolCalls: number;
          wallClockSeconds: number;
          maximumParallelAgents: number;
        }>;
        stopConditions: readonly Readonly<{
          conditionId: string;
          kind: string;
          predicateRef: string;
        }>[];
        authority: Readonly<{
          authorityRevision: number;
          authorityRef: string;
          authorityDigest: string;
        }>;
        policy: Readonly<{
          policyRevision: number;
          policyRef: string;
          policyDigest: string;
        }>;
        rationaleRef: string;
        planDigest: string;
      }>;
    }>;

type ContextPressureCurrentShape =
  | Readonly<{
      currency: "unavailable";
      pressure: null;
      ageSeconds: null;
      readAt: string;
    }>
  | Readonly<{
      currency: "current" | "stale";
      ageSeconds: number;
      readAt: string;
      pressure: Readonly<{
        pressure: string;
        source: string;
        confidence: string;
        windowTokens: number | null;
        usedTokens: number | null;
        remainingTokens: number | null;
        providerGeneration: number;
        contextRevision: number;
        revision: number;
        observedAt: string;
        expiresAt: string;
        evidenceDigest: string;
      }>;
    }>;

type ReviewEvidenceShape = Readonly<{
  record: Readonly<{
    evidenceId: string;
    targetGeneration: number;
    slot: string;
    endpointProvider: string;
    providerFamily: string;
    model: string;
    routeReceiptDigest: string;
    routeObservationDigest: string | null;
    actualRouteIdentityDigest: string | null;
  }>;
  currency: Readonly<{
    target: string;
    source: string;
    chair: string;
    profile: string;
    certifying: boolean;
    blockerCodes: readonly string[];
  }>;
}>;

function unavailableProjectionValue(read: Readonly<{
  state: "unavailable";
  reason: string;
  code: string | null;
}>): string {
  return `unavailable | ${read.reason}${read.code === null ? "" : ` | ${read.code}`}`;
}

function observedNullable(value: string | number | null): string {
  return value === null ? "observed null" : String(value);
}

function reviewRunDetailLines(
  row: ConsoleRow,
  dataset: FabricConsoleDataset,
): readonly DetailLine[] {
  if (row.view !== "runs") return [];
  const projection = dataset.review?.reviewRuns.find(
    ({ coordinationRunId }) => coordinationRunId === row.stableId,
  );
  if (projection === undefined) return [];
  const lines: DetailLine[] = [];
  if (projection.preparation.state === "unavailable") {
    lines.push({
      label: "Review preparation",
      value: unavailableProjectionValue(projection.preparation),
    });
  } else {
    const preparation = projection.preparation.value as unknown as ReviewPreparationShape;
    lines.push(
      {
        label: "Review preparation",
        value: `${preparation.state.toUpperCase()} | ${preparation.phase} | r${String(preparation.revision)}`,
      },
      {
        label: "Review preparation identity",
        value: `${preparation.accepted.preparationId} | task ${preparation.accepted.taskId} | target ${String(preparation.accepted.reservedTargetGeneration)}`,
      },
      {
        label: "Review preparation progress",
        value: preparation.progress.kind === "phase-only"
          ? "phase-only"
          : `${String(preparation.progress.completed)}/${String(preparation.progress.total)} verified-build-items | ${preparation.progress.planDigest}`,
      },
      {
        label: "Review preparation terminal",
        value: preparation.terminal === null
          ? "observed null"
          : preparation.terminal.kind === "succeeded"
            ? `succeeded | target ${String(preparation.terminal.targetRef)}`
            : `${preparation.terminal.kind} | ${preparation.terminal.code} | ${preparation.terminal.evidenceDigest}`,
      },
    );
  }
  if (projection.completion.state === "unavailable") {
    lines.push({
      label: "Review completion",
      value: unavailableProjectionValue(projection.completion),
    });
  } else {
    const completion = projection.completion.value as unknown as ReviewCompletionShape;
    lines.push(
      {
        label: "Review target generation",
        value: observedNullable(completion.targetGeneration),
      },
      {
        label: "Review completion",
        value: completion.finalReviewComplete ? "COMPLETE" : "INCOMPLETE",
      },
      {
        label: "Review blockers",
        value: completion.blockers.length === 0
          ? "none observed"
          : completion.blockers.join(", "),
      },
      {
        label: "Reviewed artifact",
        value: observedNullable(completion.reviewedArtifactRef),
      },
      {
        label: "Review target chair",
        value: completion.targetChair === null
          ? "observed null"
          : `${completion.targetChair.agentId} | binding g${String(completion.targetChair.bindingGeneration)} | principal g${String(completion.targetChair.principalGeneration)} | provider g${String(completion.targetChair.providerSessionGeneration)} | ${completion.targetChair.modelFamily}/${completion.targetChair.model}`,
      },
      {
        label: "Review target digests",
        value: `publication ${observedNullable(completion.publicationLineageDigest)} | bundle ${observedNullable(completion.bundleDigest)} | manifest ${observedNullable(completion.manifestRootDigest)} | coverage ${observedNullable(completion.coverageDigest)} | risk ${observedNullable(completion.riskReadMapDigest)} | mandatory ${observedNullable(completion.mandatoryReadSetDigest)} | profile ${observedNullable(completion.profileDigest)}`,
      },
    );
    for (const unavailable of completion.unavailableSlots) {
      lines.push({
        label: `Review slot ${unavailable.slot}`,
        value: `unavailable | ${unavailable.reason} | ${unavailable.endpointProvider}/${unavailable.providerFamily}/${unavailable.model} | availability r${String(unavailable.availabilityRevision)}`,
      });
    }
    for (const slot of completion.slots) {
      lines.push(
        {
          label: `Review slot ${slot.slot}`,
          value: `${observedNullable(slot.verdict)} | ${slot.certifying ? "certifying" : "noncertifying"}`,
        },
        {
          label: `Review slot ${slot.slot} provider`,
          value: `${slot.endpointProvider}/${slot.providerFamily}/${slot.model}`,
        },
        {
          label: `Review slot ${slot.slot} route proof`,
          value: slot.actualRouteIdentityDigest === null
            ? `observed null | blockers ${slot.blockers.join(", ") || "none observed"}`
            : `proved | ${slot.actualRouteIdentityDigest}`,
        },
      );
    }
  }
  lines.push({
    label: "Review evidence",
    value: projection.evidence.state === "unavailable"
      ? unavailableProjectionValue(projection.evidence)
      : `${String(projection.evidence.value.length)} record(s)`,
  });
  for (const recovery of projection.recoveries) {
    lines.push({
      label: `Route recovery ${recovery.actionRef.adapterId}:${recovery.actionRef.actionId}`,
      value: recovery.read.state === "unavailable"
        ? unavailableProjectionValue(recovery.read)
        : `${recovery.read.value.state} | route ${recovery.read.value.routeState} | lookup ${recovery.read.value.lookupState} | retirement ${recovery.read.value.retirementEligible ? "eligible" : "ineligible"}`,
    });
  }
  lines.push(
    {
      label: "Provider route",
      value: unavailableProjectionValue(projection.providerRoute),
    },
    {
      label: "Capability freshness",
      value: unavailableProjectionValue(projection.capabilityFreshness),
    },
  );
  return lines;
}

function topologyDetailLines(
  row: ConsoleRow,
  dataset: FabricConsoleDataset,
): readonly DetailLine[] {
  if (row.view !== "work") return [];
  const projection = dataset.review?.topology.find(
    ({ taskId }) => taskId === row.stableId,
  );
  if (projection === undefined) return [];
  if (projection.read.state === "unavailable") {
    return [{
      label: "Topology current",
      value: unavailableProjectionValue(projection.read),
    }];
  }
  const current = projection.read.value as unknown as TopologyCurrentShape;
  if (current.currency === "unavailable") {
    return [
      { label: "Topology currency", value: "UNAVAILABLE" },
      { label: "Topology current", value: "observed null" },
    ];
  }
  const plan = current.plan;
  const lines: DetailLine[] = [
    { label: "Topology currency", value: current.currency.toUpperCase() },
    {
      label: "Topology wave",
      value: `${plan.waveId}@r${String(plan.waveRevision)} | ${plan.state}`,
    },
    {
      label: "Topology pointer",
      value: `r${String(current.pointer.revision)} | ${current.pointer.planDigest}`,
    },
    {
      label: "Topology predecessor",
      value: plan.predecessor === null
        ? "observed null"
        : `${plan.predecessor.waveId}@r${String(plan.predecessor.waveRevision)} | ${plan.predecessor.planDigest}`,
    },
    {
      label: "Topology decomposability",
      value: `${plan.decomposability.kind} | ${plan.decomposability.evidenceRef}`,
    },
    {
      label: "Topology execution",
      value: `${plan.topology.executionShape} | ${plan.topology.mode} | max ${String(plan.topology.maximumConcurrentAgents)}`,
    },
    {
      label: "Topology chair",
      value: `${plan.chair.agentId} | principal g${String(plan.chair.principalGeneration)} | lease g${String(plan.chair.chairLeaseGeneration)}`,
    },
    {
      label: "Topology contention",
      value: `${plan.contention.mode} | owner ${observedNullable(plan.contention.serializationOwnerAgentId)} | ${plan.contention.evidenceRef}`,
    },
    {
      label: "Topology budget",
      value: `turns ${String(plan.budget.providerTurns)} | tools ${String(plan.budget.toolCalls)} | wall ${String(plan.budget.wallClockSeconds)}s | parallel ${String(plan.budget.maximumParallelAgents)}`,
    },
    {
      label: "Topology authority",
      value: `${plan.authority.authorityRef}@r${String(plan.authority.authorityRevision)} | ${plan.authority.authorityDigest}`,
    },
    {
      label: "Topology policy",
      value: `${plan.policy.policyRef}@r${String(plan.policy.policyRevision)} | ${plan.policy.policyDigest}`,
    },
    { label: "Topology rationale", value: plan.rationaleRef },
    { label: "Topology plan digest", value: plan.planDigest },
  ];
  for (const dependency of plan.dependencies) {
    lines.push({
      label: "Topology dependency",
      value: `${dependency.dependencyTaskId} | ${dependency.requiredState} | ${dependency.evidenceRef}`,
    });
  }
  for (const owner of plan.stageOwners) {
    lines.push({
      label: `Topology stage ${owner.stageId}`,
      value: `${owner.taskId} | owner ${owner.ownerAgentId} | partition ${observedNullable(owner.writePartitionId)}`,
    });
  }
  for (const partition of plan.writePartitions) {
    lines.push({
      label: `Topology partition ${partition.partitionId}`,
      value: `${partition.mode} | owner ${partition.ownerAgentId} | paths ${partition.pathSetDigest} | authority ${partition.authorityRef}`,
    });
  }
  for (const stop of plan.stopConditions) {
    lines.push({
      label: `Topology stop ${stop.conditionId}`,
      value: `${stop.kind} | ${stop.predicateRef}`,
    });
  }
  return lines;
}

function contextPressureDetailLines(
  row: ConsoleRow,
  dataset: FabricConsoleDataset,
): readonly DetailLine[] {
  if (row.view !== "agents") return [];
  const projection = dataset.review?.contextPressure.find(
    ({ agentId }) => agentId === row.stableId,
  );
  if (projection === undefined) return [];
  if (projection.read.state === "unavailable") {
    return [{
      label: "Context pressure",
      value: unavailableProjectionValue(projection.read),
    }];
  }
  const current = projection.read.value as unknown as ContextPressureCurrentShape;
  if (current.currency === "unavailable") {
    return [
      { label: "Context pressure", value: "observed null | UNAVAILABLE" },
      { label: "Context age", value: "observed null" },
    ];
  }
  const pressure = current.pressure;
  const tokens = pressure.windowTokens === null
    ? "observed null"
    : `window ${String(pressure.windowTokens)} | used ${String(pressure.usedTokens)} | remaining ${String(pressure.remainingTokens)}`;
  return [
    {
      label: "Context pressure",
      value: `${pressure.pressure.toUpperCase()} | ${current.currency.toUpperCase()} | age ${String(current.ageSeconds)}s`,
    },
    {
      label: "Context source",
      value: `${pressure.source} | ${pressure.confidence}`,
    },
    { label: "Context tokens", value: tokens },
    {
      label: "Context generations",
      value: `provider g${String(pressure.providerGeneration)} | context r${String(pressure.contextRevision)} | projection r${String(pressure.revision)}`,
    },
    {
      label: "Context observation",
      value: `${pressure.observedAt} -> ${pressure.expiresAt} | read ${current.readAt}`,
    },
    { label: "Context evidence", value: pressure.evidenceDigest },
  ];
}

function evidenceReviewDetailLines(
  row: ConsoleRow,
  dataset: FabricConsoleDataset,
): readonly DetailLine[] {
  if (row.view !== "evidence") return [];
  const reviewRuns = dataset.review?.reviewRuns ?? [];
  const matchingRuns = reviewRuns.flatMap((run) =>
    run.evidence.state !== "current"
      ? []
      : (run.evidence.value as unknown as readonly ReviewEvidenceShape[])
        .filter(({ record }) => record.evidenceId === row.stableId)
        .map((evidence) => ({
          coordinationRunId: String(run.coordinationRunId),
          evidence,
        }))
  );
  const inspectionRunId =
    dataset.inspection?.kind === "artifact" &&
      dataset.inspection.state === "current" &&
      dataset.inspection.binding.view === "evidence" &&
      dataset.inspection.binding.itemId === row.stableId &&
      dataset.inspection.result.coordinationRunId !== null
      ? String(dataset.inspection.result.coordinationRunId)
      : null;
  const evidence = inspectionRunId === null
    ? matchingRuns.length === 1
      ? matchingRuns[0]?.evidence
      : undefined
    : matchingRuns.find(
      ({ coordinationRunId }) => coordinationRunId === inspectionRunId,
    )?.evidence;
  if (evidence === undefined && matchingRuns.length > 0) {
    return [{
      label: "Review evidence",
      value: "unavailable | coordination-run-binding-unavailable",
    }];
  }
  if (evidence === undefined) return [];
  const record = evidence.record;
  const currency = evidence.currency;
  const routeProof = currency.blockerCodes.includes("actual-route-mismatch")
    ? "Unknown | actual-route-mismatch"
    : currency.blockerCodes.includes("actual-route-unproved")
      ? "Unknown | actual-route-unproved"
      : record.actualRouteIdentityDigest !== null &&
          record.routeObservationDigest !== null
        ? `proved | ${record.actualRouteIdentityDigest}`
        : "Unknown | actual-route-unproved";
  return [
    {
      label: "Review target",
      value: `generation ${String(record.targetGeneration)} | slot ${record.slot}`,
    },
    {
      label: "Admitted review route",
      value: `${record.endpointProvider} | ${record.providerFamily} | ${record.model}`,
    },
    { label: "Actual endpoint identity", value: routeProof },
    {
      label: "Actual route observation",
      value: observedNullable(record.routeObservationDigest),
    },
    { label: "Route receipt", value: record.routeReceiptDigest },
    {
      label: "Review currency",
      value: `target ${currency.target} | source ${currency.source} | chair ${currency.chair} | profile ${currency.profile}`,
    },
    {
      label: "Review certification",
      value: `${currency.certifying ? "certifying" : "noncertifying"} | blockers ${currency.blockerCodes.join(", ") || "none observed"}`,
    },
  ];
}

export function detailLines(
  row: ConsoleRow,
  dataset: FabricConsoleDataset,
): PresentedDetail {
  const [primary, secondary] = summaryText(row, dataset);
  const lines: Array<Readonly<{ label: string; value: string }>> = [
    { label: "ID", value: row.stableId },
    { label: "Revision", value: row.revision },
    { label: "Kind", value: row.summary?.kind ?? "unavailable" },
    { label: "Summary", value: primary },
    { label: "State", value: secondary },
    { label: "Source", value: row.freshness.source },
    { label: "Freshness", value: freshnessLabel(row.freshness, dataset) },
  ];
  if (row.summary?.kind === "attention") {
    const grouping = attentionGroupingLabel(row, dataset);
    const notification = row.summary.nativeNotification;
    if (notification.kind === "feature-unavailable") {
      lines.push({
        label: "Native notification",
        value: "unavailable | feature-not-negotiated",
      });
    } else {
      lines.push(
        {
          label: "Native notification",
          value: `${notification.status} | journal ${notification.journalState}`,
        },
        {
          label: "Notification basis",
          value: `integration ${notification.integrationState} | delivery ${
            notification.deliveryItemRevision === null
              ? "missing"
              : `r${String(notification.deliveryItemRevision)}`
          } | claim ${
            notification.claimGeneration === null
              ? "none"
              : `g${String(notification.claimGeneration)}`
          } | observed ${notification.observedAt}`,
        },
      );
    }
    if (grouping !== null) {
      lines.push({ label: "Attention grouping", value: grouping });
    }
  }
  if (row.view === "system" && dataset.connectionDiagnosis !== undefined) {
    lines.push(...connectionDiagnosisDetailLines(dataset.connectionDiagnosis));
  }
  if (row.actionAvailability.state === "read-only") {
    lines.push({ label: "Actions", value: `read-only: ${row.actionAvailability.reason}` });
  } else {
    lines.push({ label: "Actions", value: row.actionAvailability.actions.join(", ") });
  }
  if (row.summary?.kind === "project") {
    lines.push({
      label: "Accepted scope",
      value: row.summary.acceptedScopeRef === null
        ? "unaccepted"
        : `${row.summary.acceptedScopeRef.path}@${row.summary.acceptedScopeRef.digest}`,
    });
  }
  if (row.summary?.kind === "run") {
    lines.push(...runDetailLines(row.summary));
  }
  if (row.summary?.kind === "activity") {
    lines.push(...activityGroupDetailLines(row.summary));
  }
  lines.push(
    ...reviewRunDetailLines(row, dataset),
    ...topologyDetailLines(row, dataset),
    ...contextPressureDetailLines(row, dataset),
    ...evidenceReviewDetailLines(row, dataset),
  );
  return { stableId: row.stableId, revision: row.revision, lines };
}
type PresentedRows = Readonly<{
  masterRows: readonly PresentedRow[];
  needsYouRows: readonly PresentedRow[];
  watchRows: readonly PresentedRow[];
  watchCollapsed: true;
  topAttention: PresentedRow | null;
  selectedRow: ConsoleRow | null;
}>;

type PresentedRowsCache = Readonly<{
  masterRows: readonly PresentedRow[];
  needsYouRows: readonly PresentedRow[];
  watchRows: readonly PresentedRow[];
  indexByStableId: ReadonlyMap<string, number>;
}>;

const presentedRowsByDataset = new WeakMap<
  FabricConsoleDataset,
  WeakMap<readonly ConsoleRow[], PresentedRowsCache>
>();

function invariantRows(
  dataset: FabricConsoleDataset,
  activeRows: readonly ConsoleRow[],
): PresentedRowsCache {
  let byRows = presentedRowsByDataset.get(dataset);
  if (byRows === undefined) {
    byRows = new WeakMap();
    presentedRowsByDataset.set(dataset, byRows);
  }
  const cached = byRows.get(activeRows);
  if (cached !== undefined) return cached;

  const canMutate = dataset.canMutate && dataset.connection.state === "live";
  const masterRows = activeRows.map((candidate) =>
    presentRow(candidate, false, canMutate, dataset)
  );
  const needsYouRows = activeRows.flatMap((candidate, index) =>
    candidate.view === "attention" && !isNeedsYouRow(candidate)
      ? []
      : [masterRows[index] as PresentedRow]
  );
  const watchRows = activeRows.flatMap((candidate, index) =>
    candidate.view === "attention" && !isNeedsYouRow(candidate)
      ? [masterRows[index] as PresentedRow]
      : []
  );
  const value = {
    masterRows,
    needsYouRows,
    watchRows,
    indexByStableId: new Map(
      activeRows.map((candidate, index) => [candidate.stableId, index]),
    ),
  } satisfies PresentedRowsCache;
  byRows.set(activeRows, value);
  return value;
}

function selectPresentedRow(row: PresentedRow, selected: boolean): PresentedRow {
  return row.selected === selected ? row : { ...row, selected };
}

export function presentRows(
  dataset: FabricConsoleDataset,
  controller: ConsoleControllerState,
  activeRows: readonly ConsoleRow[],
  selected: ConsoleControllerState["selectionByView"][FabricView],
): PresentedRows {
  const invariant = invariantRows(dataset, activeRows);
  const selectedIndex = selected === null
    ? undefined
    : invariant.indexByStableId.get(selected.stableId);
  const selectedRow = selectedIndex === undefined ? undefined : activeRows[selectedIndex];
  const selectedPresentation = selectedIndex === undefined
    ? undefined
    : invariant.masterRows[selectedIndex];
  const presentedMasterRows = selectedIndex === undefined || selectedPresentation === undefined
    ? invariant.masterRows
    : invariant.masterRows.with(
        selectedIndex,
        selectPresentedRow(selectedPresentation, true),
      );
  const attentionRows = dataset.pages.attention.rows;
  const attentionInvariant = activeRows === attentionRows
    ? invariant
    : invariantRows(dataset, attentionRows);
  const firstAttention = attentionRows.find(isNeedsYouRow);
  const firstAttentionIndex = firstAttention === undefined
    ? -1
    : attentionRows.findIndex((row) => row.stableId === firstAttention.stableId);
  const topAttention = firstAttentionIndex < 0
    ? null
    : selectPresentedRow(
        attentionInvariant.masterRows[firstAttentionIndex] as PresentedRow,
        controller.selectionByView.attention?.stableId === firstAttention?.stableId,
      );
  const presentedNeedsYouRows = attentionInvariant.needsYouRows.map((row) =>
    controller.selectionByView.attention?.stableId === row.stableId
      ? selectPresentedRow(row, true)
      : row
  );
  return {
    masterRows: presentedMasterRows,
    needsYouRows: presentedNeedsYouRows,
    watchRows: attentionInvariant.watchRows,
    watchCollapsed: true,
    selectedRow: selectedRow ?? null,
    topAttention,
  };
}
