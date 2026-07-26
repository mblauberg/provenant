import type {
  ConsoleView as ProtocolConsoleView,
  DeclaredRunProgress,
  RunIdentity,
  OperatorActionAvailability,
  NativeNotificationDeliverySummary,
  OperatorViewDetailRefMap,
  OperatorViewRow,
  OperatorViewSummaryMap,
  ProjectionSource,
  Timestamp,
} from "@local/agent-fabric-protocol";

export const FABRIC_VIEWS = Object.freeze([
  "attention",
  "project",
  "runs",
  "work",
  "agents",
  "evidence",
  "activity",
  "system",
] as const satisfies readonly ProtocolConsoleView[]);

export type FabricView = (typeof FABRIC_VIEWS)[number];

declare const revisionBrand: unique symbol;
export type Revision = string & { readonly [revisionBrand]: "Revision" };

export function revisionFromProtocol(value: number): Revision {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new TypeError("protocol revision must be a safe non-negative integer");
  }
  return String(value) as Revision;
}

export function revisionToProtocol(value: Revision): number {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < 0 || String(parsed) !== value) {
    throw new TypeError("Console revision is not representable by the public protocol");
  }
  return parsed;
}

export function compareRevision(left: Revision, right: Revision): number {
  if (left.length !== right.length) {
    return left.length < right.length ? -1 : 1;
  }
  return left === right ? 0 : left < right ? -1 : 1;
}

export type ConsoleFreshness =
  | Readonly<{
      state: "live" | "snapshot" | "stale";
      source: ProjectionSource;
      revision: Revision;
      observedAt: Timestamp;
      ageMs: number;
    }>
  | Readonly<{
      state: "unavailable";
      source: ProjectionSource;
      revision: Revision;
      observedAt: Timestamp;
      ageMs: number;
      reason: string;
    }>
  | Readonly<{
      state: "conflict";
      source: ProjectionSource;
      revision: Revision;
      observedAt: Timestamp;
      ageMs: number;
      candidateCount: number;
    }>;

export type ConsoleUrgency =
  | "safety-integrity"
  | "critical-path"
  | "expiring-authority"
  | "acceptance-ready"
  | "advisory"
  | "normal";

export type ConsoleActionAvailability =
  | OperatorActionAvailability
  | Readonly<{
      state: "read-only";
      reason: "fact-unavailable" | "fact-conflict";
    }>;

export const GUIDED_WORKFLOW_ACTIONS = Object.freeze([
  "discuss",
  "accept",
  "request-changes",
  "defer",
  "implement",
  "launch",
  "git",
  "promotion",
] as const);

export type GuidedWorkflowAction = (typeof GUIDED_WORKFLOW_ACTIONS)[number];

export type ConsoleWorkflowCapability =
  | Readonly<{ state: "available" }>
  | Readonly<{ state: "unavailable"; reason: string }>;

export type ConsoleWorkflowCapabilities = Readonly<{
  intake: ConsoleWorkflowCapability;
  gate: ConsoleWorkflowCapability;
  implement?: ConsoleWorkflowCapability;
  launch: ConsoleWorkflowCapability;
  git: ConsoleWorkflowCapability;
  promotion: ConsoleWorkflowCapability;
}>;

export type ConsoleNativeNotification =
  | Readonly<NativeNotificationDeliverySummary & { kind: "daemon-journal" }>
  | Readonly<{
      kind: "feature-unavailable";
      status: "unavailable";
      reason: "feature-not-negotiated";
    }>;

export type ConsoleAttentionSummary = Readonly<
  Omit<OperatorViewSummaryMap["attention"], "nativeNotification"> & {
    nativeNotification: ConsoleNativeNotification;
  }
>;

/**
 * The Console maps rows only from a peer that negotiated
 * `declared-run-progress.v2`, `run-identity-projection.v2`, and
 * `activity-narrative-grouping.v1`, so past the mapping boundary these facts
 * are invariants, never optional fields.
 */
export type ConsoleRunSummary = Readonly<
  Omit<OperatorViewSummaryMap["runs"], "declaredProgress" | "identity"> & {
    declaredProgress: DeclaredRunProgress;
    identity: RunIdentity;
  }
>;

export type ConsoleViewSummaryMap = {
  [View in FabricView]: View extends "attention"
    ? ConsoleAttentionSummary
    : View extends "runs"
      ? ConsoleRunSummary
      : OperatorViewSummaryMap[View];
};

export type NativeNotificationProjectionMode =
  | "daemon-journal"
  | "feature-unavailable";

export type ConsoleRow<View extends FabricView = FabricView> = Readonly<{
  view: View;
  stableId: string;
  revision: Revision;
  urgency: ConsoleUrgency;
  freshness: ConsoleFreshness;
  summary: ConsoleViewSummaryMap[View] | null;
  detailRef: OperatorViewDetailRefMap[View] | null;
  actionAvailability: ConsoleActionAvailability;
}>;

export type ConsoleViewPage<View extends FabricView = FabricView> = Readonly<{
  view: View;
  rows: readonly ConsoleRow<View>[];
  nextCursor: number;
  hasMore: boolean;
  snapshotRevision: Revision | null;
  readTransactionId: string | null;
}>;

export type ConsoleViewPages = Readonly<{
  [View in FabricView]: ConsoleViewPage<View>;
}>;

export function createEmptyViewPages(): ConsoleViewPages {
  return Object.fromEntries(
    FABRIC_VIEWS.map((view) => [
      view,
      {
        view,
        rows: [],
        nextCursor: 0,
        hasMore: false,
        snapshotRevision: null,
        readTransactionId: null,
      },
    ]),
  ) as unknown as ConsoleViewPages;
}

function ageMs(observedAt: Timestamp, nowMs: number): number {
  const observedMs = Date.parse(observedAt);
  if (!Number.isFinite(observedMs)) {
    return 0;
  }
  return Math.max(0, Math.trunc(nowMs - observedMs));
}

function urgencyFor(
  view: FabricView,
  summary: ConsoleViewSummaryMap[FabricView],
): ConsoleUrgency {
  if (view !== "attention") {
    return "normal";
  }
  const attention = summary as OperatorViewSummaryMap["attention"];
  return attention.priority;
}

function consoleSummary<View extends FabricView>(
  view: View,
  summary: OperatorViewSummaryMap[View],
  nativeNotificationProjection: NativeNotificationProjectionMode,
): ConsoleViewSummaryMap[View] {
  if (
    view === "runs" &&
    (summary as OperatorViewSummaryMap["runs"]).projectSessionId === undefined
  ) {
    throw new TypeError("exact run projection has no project-session identity");
  }
  if (
    view === "runs" &&
    (summary as OperatorViewSummaryMap["runs"]).declaredProgress === undefined
  ) {
    throw new TypeError("exact run projection has no declared progress");
  }
  if (
    view === "runs" &&
    (summary as OperatorViewSummaryMap["runs"]).identity === undefined
  ) {
    throw new TypeError("exact run projection has no run identity");
  }
  if (
    view === "activity" &&
    !("group" in (summary as OperatorViewSummaryMap["activity"]))
  ) {
    throw new TypeError("exact activity projection has no narrative group");
  }
  if (view !== "attention") return summary as ConsoleViewSummaryMap[View];
  const attention = summary as OperatorViewSummaryMap["attention"];
  if (nativeNotificationProjection === "daemon-journal") {
    if (attention.nativeNotification === undefined) {
      throw new TypeError("negotiated Attention row has no native notification summary");
    }
    return {
      ...attention,
      nativeNotification: {
        kind: "daemon-journal",
        ...attention.nativeNotification,
      },
    } as ConsoleViewSummaryMap[View];
  }
  if (attention.nativeNotification !== undefined) {
    throw new TypeError("unnegotiated Attention row unexpectedly has a native notification summary");
  }
  return {
    ...attention,
    nativeNotification: {
      kind: "feature-unavailable",
      status: "unavailable",
      reason: "feature-not-negotiated",
    },
  } as ConsoleViewSummaryMap[View];
}

export function mapProtocolRow<View extends FabricView>(
  view: View,
  row: OperatorViewRow<View>,
  nowMs: number,
  nativeNotificationProjection: NativeNotificationProjectionMode,
): ConsoleRow<View> {
  const revision = revisionFromProtocol(row.itemRevision);
  const factRevision = revisionFromProtocol(row.fact.revision);
  const common = {
    view,
    stableId: row.itemId,
    revision,
  } as const;
  const commonFreshness = {
    source: row.fact.source,
    revision: factRevision,
    observedAt: row.fact.observedAt,
    ageMs: ageMs(row.fact.observedAt, nowMs),
  } as const;

  if (row.fact.freshness === "unavailable") {
    return {
      ...common,
      urgency: "normal",
      freshness: {
        state: "unavailable",
        ...commonFreshness,
        reason: row.fact.reason,
      },
      summary: null,
      detailRef: null,
      actionAvailability: { state: "read-only", reason: "fact-unavailable" },
    };
  }
  if (row.fact.freshness === "conflict") {
    return {
      ...common,
      urgency: "normal",
      freshness: {
        state: "conflict",
        ...commonFreshness,
        candidateCount: row.fact.candidates.length,
      },
      summary: null,
      detailRef: null,
      actionAvailability: { state: "read-only", reason: "fact-conflict" },
    };
  }

  const summary = consoleSummary(
    view,
    row.fact.value.summary as OperatorViewSummaryMap[View],
    nativeNotificationProjection,
  );
  const detailRef = row.fact.value.detailRef as OperatorViewDetailRefMap[View];
  return {
    ...common,
    urgency: urgencyFor(view, summary),
    freshness: { state: row.fact.freshness, ...commonFreshness },
    summary,
    detailRef,
    actionAvailability: row.fact.value.actionAvailability,
  };
}

const URGENCY_ORDER: Readonly<Record<ConsoleUrgency, number>> = Object.freeze({
  "safety-integrity": 0,
  "critical-path": 1,
  "expiring-authority": 2,
  "acceptance-ready": 3,
  advisory: 4,
  normal: 5,
});

export function rankConsoleRows<View extends FabricView>(
  rows: readonly ConsoleRow<View>[],
): readonly ConsoleRow<View>[] {
  return [...rows].sort((left, right) => {
    if (
      left.view === "activity" &&
      right.view === "activity" &&
      left.summary?.kind === "activity" &&
      right.summary?.kind === "activity" &&
      "group" in left.summary &&
      "group" in right.summary
    ) {
      return left.summary.group.ordinal - right.summary.group.ordinal;
    }
    const urgency = URGENCY_ORDER[left.urgency] - URGENCY_ORDER[right.urgency];
    if (urgency !== 0) {
      return urgency;
    }
    return left.stableId.localeCompare(right.stableId);
  });
}
