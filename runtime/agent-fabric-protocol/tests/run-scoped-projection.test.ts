import { describe, expect, it } from "vitest";

import {
  FABRIC_OPERATIONS,
  PROTOCOL_FEATURES,
  assertOperationResultFeatureShape,
  operationsForFeatures,
  parseOperationInput,
  parseOperationResult,
  parseOperationResultForInput,
} from "../src/index.js";

const observedAt = "2026-07-26T00:00:00.000Z";
const coordinationTarget = {
  kind: "coordination-run",
  coordinationRunId: "run_01",
} as const;
const deliveryTarget = {
  kind: "delivery-workstream",
  coordinationRunId: "run_01",
  deliveryRunId: "delivery_01",
  workstreamId: "workstream_01",
} as const;
const composition = {
  projectSessionId: "session_01",
  target: coordinationTarget,
  identity: {
    freshness: "live",
    source: "fabric",
    revision: 17,
    observedAt,
    value: {
      acceptedScope: { observation: "Unobserved" },
      currentPlan: { observation: "Unobserved" },
      lead: { observation: "Observed", value: "agent_chair" },
      phase: { observation: "Observed", value: "active" },
      health: { observation: "Observed", value: "healthy" },
      currentMilestone: { observation: "Unobserved" },
      nextMilestone: { observation: "Observed", value: "verify" },
      lastEventAt: { observation: "Unobserved" },
    },
  },
  declaredProgress: {
    freshness: "live",
    source: "fabric",
    revision: 17,
    observedAt,
    value: {
      observation: "Observed",
      value: {
        plan: "open",
        counts: {
          blocked: 1,
          ready: 2,
          active: 1,
          complete: 3,
          cancelled: 0,
          degraded: 0,
        },
      },
    },
  },
} as const;

function issuesPage(
  entryScope: unknown = coordinationTarget,
  projectedComposition: unknown = composition,
) {
  return {
    status: "page",
    projectSessionId: "session_01",
    target: coordinationTarget,
    section: "issues",
    entries: [{
      runScope: entryScope,
      value: {
        kind: "task",
        scope: coordinationTarget,
        taskId: "task_blocked",
        taskRevision: 4,
        state: "blocked",
        detailRef: { kind: "task", taskId: "task_blocked", expectedRevision: 4 },
      },
    }],
    nextCursor: 1,
    hasMore: false,
    snapshotRevision: 17,
    readTransactionId: "read_run_01",
    composition: projectedComposition,
  };
}

describe("run-scoped-projection.v1", () => {
  it("is an explicit negotiated result-shape feature", () => {
    expect(PROTOCOL_FEATURES).toContain("run-scoped-projection.v1");
  });

  it("has a dedicated operator projection page operation", () => {
    expect(FABRIC_OPERATIONS).toHaveProperty(
      "projectionRunPage",
      "fabric.v1.operator-projection.run-page",
    );
    expect(operationsForFeatures(["operator-projection.v2"])).not.toContain(
      FABRIC_OPERATIONS.projectionRunPage,
    );
    expect(operationsForFeatures(["run-scoped-projection.v1"])).toContain(
      FABRIC_OPERATIONS.projectionRunPage,
    );
  });

  it("accepts only exact session-bound discriminated targets", () => {
    const base = {
      credential: {
        capabilityId: "capability_01",
        token: "capability-token",
      },
      projectId: "project_01",
      projectSessionId: "session_01",
      snapshotRevision: 17,
      section: "work",
      cursor: 0,
      limit: 50,
    };
    expect(parseOperationInput(
      FABRIC_OPERATIONS.projectionRunPage,
      { ...base, target: coordinationTarget },
    )).toMatchObject({ target: coordinationTarget });
    expect(parseOperationInput(
      FABRIC_OPERATIONS.projectionRunPage,
      { ...base, target: deliveryTarget },
    )).toMatchObject({ target: deliveryTarget });
    for (const invalid of [
      { ...base, target: { runId: "run_01" } },
      { ...base, target: { kind: "delivery-workstream", deliveryRunId: "delivery_01" } },
      { ...base, target: { ...coordinationTarget, workstreamId: "workstream_01" } },
      { ...base, target: coordinationTarget, projectSessionId: undefined },
      { ...base, target: coordinationTarget, limit: 101 },
    ]) {
      expect(() => parseOperationInput(
        FABRIC_OPERATIONS.projectionRunPage,
        invalid,
      )).toThrowError();
    }
  });

  it("enforces the closed negotiated shape with all three incompatibility reasons", () => {
    const extended = parseOperationResult(
      FABRIC_OPERATIONS.projectionRunPage,
      issuesPage(),
    );
    const legacyValue = issuesPage();
    delete (legacyValue as { composition?: unknown }).composition;
    delete (legacyValue.entries[0] as { runScope?: unknown }).runScope;
    const legacy = parseOperationResult(
      FABRIC_OPERATIONS.projectionRunPage,
      legacyValue,
    );
    const mixedValue = issuesPage();
    delete (mixedValue.entries[0] as { runScope?: unknown }).runScope;
    const mixed = parseOperationResult(
      FABRIC_OPERATIONS.projectionRunPage,
      mixedValue,
    );
    const feature = ["operator-projection.v2", "run-scoped-projection.v1"] as const;

    expect(assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.projectionRunPage,
      feature,
      extended,
    )).toBe(extended);
    expect(() => assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.projectionRunPage,
      feature,
      legacy,
    )).toThrow(expect.objectContaining({
      code: "PROTOCOL_INCOMPATIBLE",
      reason: "missing-negotiated-field",
    }));
    expect(() => assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.projectionRunPage,
      ["operator-projection.v2"],
      extended,
    )).toThrow(expect.objectContaining({ reason: "unnegotiated-field" }));
    expect(() => assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.projectionRunPage,
      feature,
      mixed,
    )).toThrow(expect.objectContaining({ reason: "mixed-presence" }));
  });

  it("rejects target drift between the page, composition, entries and issue references", () => {
    for (const invalid of [
      { ...issuesPage(), target: deliveryTarget },
      { ...issuesPage(), composition: { ...composition, target: deliveryTarget } },
      issuesPage(deliveryTarget),
      {
        ...issuesPage(),
        entries: [{
          ...issuesPage().entries[0],
          value: { ...issuesPage().entries[0]!.value, scope: deliveryTarget },
        }],
      },
      { ...issuesPage(), inferredRemainingCount: 1 },
      {
        ...issuesPage(),
        entries: [{
          ...issuesPage().entries[0],
          value: {
            ...issuesPage().entries[0]!.value,
            detailRef: {
              ...issuesPage().entries[0]!.value.detailRef,
              expectedRevision: 3,
            },
          },
        }],
      },
    ]) {
      expect(() => parseOperationResult(
        FABRIC_OPERATIONS.projectionRunPage,
        invalid,
      )).toThrowError();
    }
  });

  it("represents conflicting task and evidence facts without selecting a candidate", () => {
    for (const value of [
      {
        kind: "task-fact-conflict",
        scope: coordinationTarget,
        taskId: "task_conflict",
        taskRevision: 5,
        detailRef: { kind: "task", taskId: "task_conflict", expectedRevision: 5 },
      },
      {
        kind: "evidence-conflict",
        scope: coordinationTarget,
        evidenceId: "evidence_conflict",
        evidenceRevision: 6,
        detailRef: {
          kind: "evidence",
          evidenceId: "evidence_conflict",
          expectedRevision: 6,
        },
      },
    ] as const) {
      expect(parseOperationResult(FABRIC_OPERATIONS.projectionRunPage, {
        ...issuesPage(),
        entries: [{ runScope: coordinationTarget, value }],
      })).toMatchObject({ entries: [{ value }] });
    }
  });

  it("correlates every addressing field and page revision to the exact request", () => {
    const request = parseOperationInput(FABRIC_OPERATIONS.projectionRunPage, {
      credential: { capabilityId: "capability_01", token: "capability-token" },
      projectId: "project_01",
      projectSessionId: "session_01",
      target: coordinationTarget,
      snapshotRevision: 17,
      section: "issues",
      cursor: 0,
      limit: 50,
    });
    expect(parseOperationResultForInput(
      FABRIC_OPERATIONS.projectionRunPage,
      request,
      issuesPage(),
    )).toMatchObject({ projectSessionId: "session_01", target: coordinationTarget });
    const resnapshot = {
      status: "resnapshot-required",
      projectSessionId: "session_01",
      target: coordinationTarget,
      section: "issues",
      reason: "snapshot-mismatch",
      currentSnapshotRevision: 18,
      snapshotCursor: 9,
    } as const;
    for (const invalid of [
      { ...resnapshot, projectSessionId: "session_02" },
      { ...resnapshot, target: { ...coordinationTarget, coordinationRunId: "run_02" } },
      { ...resnapshot, section: "work" },
      { ...issuesPage(), snapshotRevision: 18 },
    ]) {
      expect(() => parseOperationResultForInput(
        FABRIC_OPERATIONS.projectionRunPage,
        request,
        invalid,
      )).toThrowError(/addressing|snapshot revision/iu);
    }
  });
});
