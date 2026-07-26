import { createHash } from "node:crypto";

import type {
  ArtifactRef,
  OperatorViewRow,
  ProjectSessionId,
  ProjectionSnapshotRequest,
  RunIssueReference,
  RunProjectionSection,
  RunProjectionTarget,
  RunScopedComposition,
} from "@local/agent-fabric-protocol";

import type { ConsoleProtocolPort } from "./protocol-adapter.js";

export type ConsoleRunProjection = Readonly<{
  projectSessionId: ProjectSessionId;
  target: RunProjectionTarget;
  composition: RunScopedComposition;
  readTransactionId: string;
  work: readonly OperatorViewRow<"work">[];
  agents: readonly OperatorViewRow<"agents">[];
  evidence: readonly OperatorViewRow<"evidence">[];
  activity: readonly OperatorViewRow<"activity">[];
  issues: readonly RunIssueReference[];
  evidencePaths: readonly ArtifactRef[];
  evidencePathObservation: "Observed" | "Unobserved" | "Unknown";
}>;

export class RunProjectionInvalidError extends Error {}

export type RunInspectionResult =
  | Readonly<{ state: "current"; result: ConsoleRunProjection }>
  | Readonly<{
      state: "unavailable";
      reason:
        | "feature-unavailable"
        | "projection-changed"
        | "contract-invalid"
        | "transport-failure";
    }>;

export function sameRunProjectionTarget(
  left: RunProjectionTarget,
  right: RunProjectionTarget,
): boolean {
  return left.kind === right.kind &&
    left.coordinationRunId === right.coordinationRunId &&
    (left.kind !== "delivery-workstream" || (
      right.kind === "delivery-workstream" &&
      left.deliveryRunId === right.deliveryRunId &&
      left.workstreamId === right.workstreamId
    ));
}

function contentDigest(value: string): string {
  return `sha256:${createHash("sha256").update(value).digest("hex")}`;
}

export async function loadRunProjection(options: Readonly<{
  port: ConsoleProtocolPort;
  readScope: ProjectionSnapshotRequest;
  projectSessionId: ProjectSessionId;
  target: RunProjectionTarget;
  snapshotRevision: number;
  pageLimit: number;
  maximumPagesPerSection: number;
}>): Promise<ConsoleRunProjection> {
  const sections = [
    "work",
    "agents",
    "evidence",
    "activity",
    "issues",
  ] as const satisfies readonly RunProjectionSection[];
  const collected: Record<RunProjectionSection, unknown[]> = {
    work: [],
    agents: [],
    evidence: [],
    activity: [],
    issues: [],
  };
  let composition: RunScopedComposition | null = null;
  let compositionDigest: string | null = null;
  let readTransactionId: string | null = null;
  for (const section of sections) {
    let cursor = 0;
    for (let pageNumber = 0; pageNumber < options.maximumPagesPerSection; pageNumber += 1) {
      const result = await options.port.runPage({
        ...options.readScope,
        projectSessionId: options.projectSessionId,
        target: options.target,
        snapshotRevision: options.snapshotRevision,
        section,
        cursor,
        limit: options.pageLimit,
      });
      if (result.status === "resnapshot-required") {
        throw Object.assign(new Error("run projection changed"), {
          code: "PROJECTION_RESNAPSHOT_REQUIRED",
        });
      }
      if (
        result.projectSessionId !== options.projectSessionId ||
        result.section !== section ||
        result.snapshotRevision !== options.snapshotRevision ||
        !sameRunProjectionTarget(result.target, options.target) ||
        result.composition === undefined
      ) {
        throw new RunProjectionInvalidError("run projection page identity changed");
      }
      const nextCompositionDigest = contentDigest(JSON.stringify(result.composition));
      if (
        (compositionDigest !== null && compositionDigest !== nextCompositionDigest) ||
        (readTransactionId !== null && readTransactionId !== result.readTransactionId)
      ) {
        throw new RunProjectionInvalidError("run projection composition changed between pages");
      }
      composition = result.composition;
      compositionDigest = nextCompositionDigest;
      readTransactionId = result.readTransactionId;
      for (const entry of result.entries) {
        if (
          entry.runScope === undefined ||
          !sameRunProjectionTarget(entry.runScope, options.target)
        ) {
          throw new RunProjectionInvalidError("run projection entry lost its exact target");
        }
        collected[section].push(entry.value);
      }
      if (!result.hasMore) break;
      if (result.nextCursor <= cursor) {
        throw new RunProjectionInvalidError("run projection cursor did not advance");
      }
      cursor = result.nextCursor;
      if (pageNumber === options.maximumPagesPerSection - 1) {
        throw new RunProjectionInvalidError("run projection exceeded the bounded page count");
      }
    }
  }
  if (composition === null || readTransactionId === null) {
    throw new RunProjectionInvalidError("run projection composition is unavailable");
  }
  const evidence = collected.evidence as OperatorViewRow<"evidence">[];
  const evidencePaths: ArtifactRef[] = [];
  let evidencePathObservation: ConsoleRunProjection["evidencePathObservation"] = "Unobserved";
  for (const evidenceRow of evidence) {
    if (evidenceRow.fact.freshness === "conflict") {
      evidencePathObservation = "Unknown";
      continue;
    }
    if (evidenceRow.fact.freshness === "unavailable") {
      continue;
    }
    const detailRef = evidenceRow.fact.value.detailRef;
    const detail = await options.port.readDetail({
      ...options.readScope,
      projectSessionId: options.projectSessionId,
      snapshotRevision: options.snapshotRevision,
      detailRef,
    });
    if (detail.status === "resnapshot-required") {
      throw Object.assign(new Error("run evidence changed"), {
        code: "PROJECTION_RESNAPSHOT_REQUIRED",
      });
    }
    if (
      detail.detailRef.kind !== "evidence" ||
      detail.detailRef.evidenceId !== detailRef.evidenceId ||
      detail.detail.revision !== detailRef.expectedRevision
    ) {
      throw new RunProjectionInvalidError("run evidence detail is not exact");
    }
    if (detail.detail.freshness === "conflict") {
      evidencePathObservation = "Unknown";
      continue;
    }
    if (detail.detail.freshness === "unavailable") continue;
    if (detail.detail.value.kind !== "evidence") {
      throw new RunProjectionInvalidError("run evidence detail is not exact");
    }
    evidencePaths.push(detail.detail.value.artifactRef);
    if (evidencePathObservation !== "Unknown") evidencePathObservation = "Observed";
  }
  return {
    projectSessionId: options.projectSessionId,
    target: options.target,
    composition,
    readTransactionId,
    work: collected.work as OperatorViewRow<"work">[],
    agents: collected.agents as OperatorViewRow<"agents">[],
    evidence,
    activity: collected.activity as OperatorViewRow<"activity">[],
    issues: collected.issues as RunIssueReference[],
    evidencePaths,
    evidencePathObservation,
  };
}

export async function inspectRunProjection(options: Readonly<{
  currentSnapshotRevision: unknown;
  selectedProjectSessionId: ProjectSessionId | undefined;
  bindingProjectSessionId: ProjectSessionId | undefined;
  bindingTarget: RunProjectionTarget;
  bindingRevision: unknown;
  protocolSnapshotRevision: number;
  port: ConsoleProtocolPort | null;
  readScope: ProjectionSnapshotRequest;
  pageLimit: number;
  maximumPagesPerSection: number;
}>): Promise<RunInspectionResult> {
  if (
    options.currentSnapshotRevision !== options.bindingRevision ||
    options.bindingProjectSessionId === undefined ||
    options.selectedProjectSessionId !== options.bindingProjectSessionId
  ) {
    return { state: "unavailable", reason: "projection-changed" };
  }
  if (options.port === null) return { state: "unavailable", reason: "feature-unavailable" };
  try {
    return {
      state: "current",
      result: await loadRunProjection({
        port: options.port,
        readScope: options.readScope,
        projectSessionId: options.bindingProjectSessionId,
        target: options.bindingTarget,
        snapshotRevision: options.protocolSnapshotRevision,
        pageLimit: options.pageLimit,
        maximumPagesPerSection: options.maximumPagesPerSection,
      }),
    };
  } catch (error: unknown) {
    const code = typeof error === "object" && error !== null && "code" in error
      ? Reflect.get(error, "code")
      : null;
    return {
      state: "unavailable",
      reason: code === "PROJECTION_RESNAPSHOT_REQUIRED" || code === "STALE_REVISION"
        ? "projection-changed"
        : error instanceof RunProjectionInvalidError
          ? "contract-invalid"
          : "transport-failure",
    };
  }
}
