import { describe, expect, it } from "vitest";

import * as protocol from "../src/index.js";
import {
  FABRIC_OPERATIONS,
  OPERATION_CONTRACT_FIXTURES,
  OPERATION_REGISTRY,
  ProtocolResultShapeError,
  assertOperationResultFeatureShape,
  createOperatorClient,
  parseOperationInput,
  parseOperationResult,
} from "../src/index.js";

const credential = { capabilityId: "capability_01", token: "test-capability-token" } as const;
const digestA = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const digestB = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const artifact = { path: "artifacts/evidence.json", digest: digestA } as const;
const operatorCommand = {
  credential,
  commandId: "command_preview_01",
  expectedRevision: 1,
  actor: "operator_01",
  provenance: { kind: "console-direct-input", clientId: "client_01", inputEventId: "input_01" },
  evidenceRefs: [artifact],
} as const;

describe("closed operator Console surface", () => {
  it("publishes the eight additive operator-only operations", () => {
    expect(FABRIC_OPERATIONS).toMatchObject({
      scopedGateRead: "fabric.v1.scoped-gate.read",
      projectionViewPage: "fabric.v1.operator-projection.view-page",
      projectionRunPage: "fabric.v1.operator-projection.run-page",
      projectionDetailRead: "fabric.v1.operator-projection.detail.read",
      operatorActionPreview: "fabric.v1.operator-action.preview",
      operatorActionCommit: "fabric.v1.operator-action.commit",
      operatorActionStatus: "fabric.v1.operator-action.status",
      operatorActionReconcile: "fabric.v1.operator-action.reconcile",
    });
  });

  it("registers every additive operation for the operator principal only", () => {
    const operations = [
      FABRIC_OPERATIONS.scopedGateRead,
      FABRIC_OPERATIONS.projectionViewPage,
      FABRIC_OPERATIONS.projectionRunPage,
      FABRIC_OPERATIONS.projectionDetailRead,
      FABRIC_OPERATIONS.operatorActionPreview,
      FABRIC_OPERATIONS.operatorActionCommit,
      FABRIC_OPERATIONS.operatorActionStatus,
      FABRIC_OPERATIONS.operatorActionReconcile,
    ] as const;
    expect(operations.map((operation) => OPERATION_REGISTRY[operation].principals)).toStrictEqual(
      operations.map(() => ["operator"]),
    );
  });
});

describe("canonical scoped-gate read", () => {
  const gate = OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.scopedGateCreate].result as Record<string, unknown>;

  it("reads an exact project/session/gate revision without adding a mutation surface", () => {
    const input = {
      credential,
      projectId: "project_01",
      projectSessionId: "ps_01",
      gateId: gate.gateId,
      expectedRevision: gate.revision,
    };
    expect(parseOperationInput(FABRIC_OPERATIONS.scopedGateRead, input)).toStrictEqual(input);
    expect(parseOperationResult(FABRIC_OPERATIONS.scopedGateRead, {
      status: "current",
      gate,
      readTransactionId: "read_tx_01",
      stateDigest: digestA,
    })).toMatchObject({ status: "current", gate });
  });

  it("rejects a changed result that echoes the allegedly stale revision", () => {
    expect(() => parseOperationResult(FABRIC_OPERATIONS.scopedGateRead, {
      status: "changed",
      expectedRevision: gate.revision,
      gate,
      readTransactionId: "read_tx_01",
      stateDigest: digestA,
    })).toThrowError(/changed.*revision/iu);
  });
});

const observedAt = "2026-07-11T10:00:00.000Z";
const nativeNotification = {
  targetIntegration: "native-desktop",
  status: "available",
  journalState: "sent",
  deliveryItemRevision: 1,
  claimGeneration: null,
  integrationState: "available",
  observedAt,
} as const;
const rowCases = [
  ["attention", {
    kind: "attention",
    label: "Decision",
    priority: "critical-path",
    title: "Choose",
    gateBinding: {
      gateId: "gate_01",
      gateRevision: 3,
      coordinationRunId: "run_01",
    },
    nativeNotification,
  }, { kind: "task", taskId: "task_01", expectedRevision: 1 }],
  ["project", { kind: "project", goal: "Ship", acceptedScopeRef: null, repositoryRevision: "revision_01" }, { kind: "project", projectId: "project_01", expectedRevision: 1 }],
  ["runs", { kind: "run", phase: "reviewing", health: "healthy", nextMilestone: "acceptance" }, { kind: "run", coordinationRunId: "run_01", expectedRevision: 1 }],
  ["work", { kind: "work", state: "active", checkState: "passing" }, { kind: "task", taskId: "task_01", expectedRevision: 1 }],
  ["agents", { kind: "agent", role: "worker", lifecycle: "active", contextPressure: "low" }, { kind: "agent", agentId: "agent_01", expectedRevision: 1 }],
  ["evidence", { kind: "evidence", evidenceKind: "test", status: "pass", provenance: "vitest" }, { kind: "evidence", evidenceId: "evidence_01", expectedRevision: 1 }],
  ["activity", { kind: "activity", activityKind: "operation", summary: "Checked", occurredAt: observedAt }, { kind: "activity", eventId: "event_01", expectedRevision: 1 }],
  ["system", { kind: "system", systemKind: "daemon", state: "healthy", detail: "ready" }, { kind: "system", componentId: "daemon_01", expectedRevision: 1 }],
] as const;

function row(summary: unknown, detailRef: unknown) {
  return {
    itemId: "item_01",
    itemRevision: 1,
    fact: {
      freshness: "live",
      source: "fabric",
      revision: 1,
      observedAt,
      value: {
        summary,
        detailRef,
        actionAvailability: { state: "read-only", reason: "feature-unavailable" },
      },
    },
  };
}

describe("rich operator projection v2", () => {
  it.each(rowCases)("carries semantic %s row revision, source, detail and action availability", (view, summary, detailRef) => {
    expect(parseOperationResult(FABRIC_OPERATIONS.projectionViewPage, {
      status: "page",
      view,
      rows: [row(summary, detailRef)],
      nextCursor: 1,
      hasMore: false,
      snapshotRevision: 4,
      readTransactionId: "read_tx_01",
    })).toMatchObject({ status: "page", view });
  });

  it("keeps the closed native notification summary optional only in the context-free codec", () => {
    const [, validSummary, detailRef] = rowCases[0];
    const { nativeNotification: _omitted, ...missingNotification } = validSummary;
    const page = (summary: unknown) => ({
      status: "page",
      view: "attention",
      rows: [row(summary, detailRef)],
      nextCursor: 1,
      hasMore: false,
      snapshotRevision: 4,
      readTransactionId: "read_tx_01",
    });

    expect(parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      page(missingNotification),
    )).toMatchObject({ status: "page", view: "attention" });
    expect(() =>
      parseOperationResult(
        FABRIC_OPERATIONS.projectionViewPage,
        page({
          ...validSummary,
          nativeNotification: {
            ...validSummary.nativeNotification,
            status: "unknown",
          },
        }),
      ),
    ).toThrowError(/nativeNotification.status/u);
  });

  it("rejects an Attention gate binding without an exact positive gate revision", () => {
    const [, validSummary, detailRef] = rowCases[0];
    expect(() => parseOperationResult(FABRIC_OPERATIONS.projectionViewPage, {
      status: "page",
      view: "attention",
      rows: [row({
        ...validSummary,
        gateBinding: { ...validSummary.gateBinding, gateRevision: 0 },
      }, detailRef)],
      nextCursor: 1,
      hasMore: false,
      snapshotRevision: 4,
      readTransactionId: "read_tx_gate_invalid",
    })).toThrowError(/gateRevision/u);
  });

  it("rejects a row whose stable item revision differs from its fact revision", () => {
    expect(() => parseOperationResult(FABRIC_OPERATIONS.projectionViewPage, {
      status: "page",
      view: "work",
      rows: [{ ...row(rowCases[3][1], rowCases[3][2]), itemRevision: 2 }],
      nextCursor: 1,
      hasMore: false,
      snapshotRevision: 4,
      readTransactionId: "read_tx_01",
    })).toThrowError(/item revision does not match fact revision/iu);
  });

  it("represents snapshot mismatch only as an explicit resnapshot requirement", () => {
    expect(parseOperationResult(FABRIC_OPERATIONS.projectionViewPage, {
      status: "resnapshot-required",
      view: "attention",
      reason: "snapshot-mismatch",
      currentSnapshotRevision: 5,
      snapshotCursor: 9,
    })).toMatchObject({ status: "resnapshot-required", reason: "snapshot-mismatch" });
  });
});

describe("negotiated notification result shape", () => {
  const legacyAttention = {
    itemId: "attention_01",
    revision: 1,
    label: "Decision",
    priority: "critical-path",
    title: "Choose",
    sourceFreshness: "live",
    lastEventAt: observedAt,
    duplicateCount: 1,
  } as const;
  const extendedAttention = { ...legacyAttention, nativeNotification };
  const legacyFeatures = ["operator-projection.v1", "operator-projection.v2"] as const;
  const extendedFeatures = [...legacyFeatures, "native-notification-projection.v1"] as const;

  it("covers negotiated, legacy, wrong-mode and malformed frames for all three Attention operations", () => {
    const snapshotBase = OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.projectionSnapshot].result as Record<string, unknown>;
    const [, validSummary, detailRef] = rowCases[0];
    const { nativeNotification: _omitted, ...legacySummary } = validSummary;
    const fact = (value: unknown) => ({
      freshness: "live",
      source: "fabric",
      revision: 1,
      observedAt,
      value,
    });
    const malformedAttention = {
      ...extendedAttention,
      nativeNotification: { ...nativeNotification, status: "unknown" },
    };
    const malformedSummary = {
      ...validSummary,
      nativeNotification: { ...nativeNotification, status: "unknown" },
    };
    const cases = [
      {
        operation: FABRIC_OPERATIONS.projectionSnapshot,
        legacy: { ...snapshotBase, attention: fact([legacyAttention]) },
        extended: { ...snapshotBase, attention: fact([extendedAttention]) },
        malformed: { ...snapshotBase, attention: fact([malformedAttention]) },
      },
      {
        operation: FABRIC_OPERATIONS.projectionPage,
        legacy: { view: "attention", page: fact({ items: [legacyAttention], nextCursor: 1, hasMore: false }) },
        extended: { view: "attention", page: fact({ items: [extendedAttention], nextCursor: 1, hasMore: false }) },
        malformed: { view: "attention", page: fact({ items: [malformedAttention], nextCursor: 1, hasMore: false }) },
      },
      {
        operation: FABRIC_OPERATIONS.projectionViewPage,
        legacy: {
          status: "page",
          view: "attention",
          rows: [row(legacySummary, detailRef)],
          nextCursor: 1,
          hasMore: false,
          snapshotRevision: 1,
          readTransactionId: "read_legacy",
        },
        extended: {
          status: "page",
          view: "attention",
          rows: [row(validSummary, detailRef)],
          nextCursor: 1,
          hasMore: false,
          snapshotRevision: 1,
          readTransactionId: "read_extended",
        },
        malformed: {
          status: "page",
          view: "attention",
          rows: [row(malformedSummary, detailRef)],
          nextCursor: 1,
          hasMore: false,
          snapshotRevision: 1,
          readTransactionId: "read_malformed",
        },
      },
    ] as const;

    for (const fixture of cases) {
      const legacy = parseOperationResult(fixture.operation, fixture.legacy);
      const extended = parseOperationResult(fixture.operation, fixture.extended);
      expect(assertOperationResultFeatureShape(fixture.operation, legacyFeatures, legacy)).toBe(legacy);
      expect(assertOperationResultFeatureShape(fixture.operation, extendedFeatures, extended)).toBe(extended);
      expect(() => assertOperationResultFeatureShape(
        fixture.operation,
        extendedFeatures,
        legacy,
      )).toThrow(expect.objectContaining({ reason: "missing-negotiated-field" }));
      expect(() => assertOperationResultFeatureShape(
        fixture.operation,
        legacyFeatures,
        extended,
      )).toThrow(expect.objectContaining({ reason: "unnegotiated-field" }));
      expect(() => parseOperationResult(fixture.operation, fixture.malformed)).toThrow(/nativeNotification.status/u);
    }
  });

  it("enforces negotiated presence for every snapshot Attention conflict candidate", () => {
    const base = OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.projectionSnapshot].result as Record<string, unknown>;
    const mixed = parseOperationResult(FABRIC_OPERATIONS.projectionSnapshot, {
      ...base,
      attention: {
        freshness: "conflict",
        source: "fabric",
        revision: 1,
        observedAt,
        candidates: [[extendedAttention], [legacyAttention]],
      },
    });

    expect(() => assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.projectionSnapshot,
      extendedFeatures,
      mixed,
    )).toThrow(expect.objectContaining({ code: "PROTOCOL_INCOMPATIBLE" }));
    expect(() => assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.projectionSnapshot,
      legacyFeatures,
      mixed,
    )).toThrow(ProtocolResultShapeError);
  });

  it("enforces negotiated presence for every projection-page Attention candidate", () => {
    const base = OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.projectionPage].result as Record<string, unknown>;
    const extended = parseOperationResult(FABRIC_OPERATIONS.projectionPage, {
      ...base,
      page: {
        freshness: "conflict",
        source: "fabric",
        revision: 1,
        observedAt,
        candidates: [
          { items: [extendedAttention], nextCursor: 1, hasMore: false },
          { items: [extendedAttention], nextCursor: 1, hasMore: false },
        ],
      },
    });
    expect(assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.projectionPage,
      extendedFeatures,
      extended,
    )).toBe(extended);
    expect(() => assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.projectionPage,
      legacyFeatures,
      extended,
    )).toThrow(ProtocolResultShapeError);
  });

  it("rejects the whole view-page result when one nested conflict candidate has the wrong mode", () => {
    const [, validSummary, detailRef] = rowCases[0];
    const { nativeNotification: _omitted, ...legacySummary } = validSummary;
    const result = parseOperationResult(FABRIC_OPERATIONS.projectionViewPage, {
      status: "page",
      view: "attention",
      rows: [{
        itemId: "attention_01",
        itemRevision: 1,
        fact: {
          freshness: "conflict",
          source: "fabric",
          revision: 1,
          observedAt,
          candidates: [
            {
              summary: validSummary,
              detailRef,
              actionAvailability: { state: "read-only", reason: "feature-unavailable" },
            },
            {
              summary: legacySummary,
              detailRef,
              actionAvailability: { state: "read-only", reason: "feature-unavailable" },
            },
          ],
        },
      }],
      nextCursor: 1,
      hasMore: false,
      snapshotRevision: 1,
      readTransactionId: "read_tx_01",
    });

    expect(() => assertOperationResultFeatureShape(
      FABRIC_OPERATIONS.projectionViewPage,
      extendedFeatures,
      result,
    )).toThrow(ProtocolResultShapeError);
  });
});

describe("negotiated run-session result shape", () => {
  const legacyFeatures = ["operator-projection.v1", "operator-projection.v2"] as const;
  const extendedFeatures = [...legacyFeatures, "run-session-projection.v1"] as const;
  const legacyRun = {
    runId: "run_01",
    phase: "active",
    chairAgentId: "chair_01",
    nextMilestone: "verification",
    health: "healthy",
  } as const;
  const extendedRun = { ...legacyRun, projectSessionId: "ps_01" } as const;
  const fact = (value: unknown) => ({
    freshness: "live",
    source: "fabric",
    revision: 1,
    observedAt,
    value,
  });

  it("requires exact project-session identity on snapshot, page, row and detail results", () => {
    const snapshotBase = OPERATION_CONTRACT_FIXTURES[
      FABRIC_OPERATIONS.projectionSnapshot
    ].result as Record<string, unknown>;
    const legacySummary = {
      kind: "run",
      phase: "active",
      health: "healthy",
      nextMilestone: "verification",
    } as const;
    const extendedSummary = { ...legacySummary, projectSessionId: "ps_01" } as const;
    const legacyRef = {
      kind: "run",
      coordinationRunId: "run_01",
      expectedRevision: 1,
    } as const;
    const extendedRef = { ...legacyRef, projectSessionId: "ps_01" } as const;
    const legacyDetail = {
      kind: "run",
      coordinationRunId: "run_01",
      phase: "active",
      chairAgentId: "chair_01",
      chairGeneration: 1,
      health: "healthy",
    } as const;
    const extendedDetail = { ...legacyDetail, projectSessionId: "ps_01" } as const;
    const fixtures = [
      {
        operation: FABRIC_OPERATIONS.projectionSnapshot,
        legacy: { ...snapshotBase, runs: fact([legacyRun]) },
        extended: { ...snapshotBase, runs: fact([extendedRun]) },
      },
      {
        operation: FABRIC_OPERATIONS.projectionPage,
        legacy: {
          view: "runs",
          page: fact({ items: [legacyRun], nextCursor: 1, hasMore: false }),
        },
        extended: {
          view: "runs",
          page: fact({ items: [extendedRun], nextCursor: 1, hasMore: false }),
        },
      },
      {
        operation: FABRIC_OPERATIONS.projectionViewPage,
        legacy: {
          status: "page",
          view: "runs",
          rows: [row(legacySummary, legacyRef)],
          nextCursor: 1,
          hasMore: false,
          snapshotRevision: 1,
          readTransactionId: "read_run_legacy",
        },
        extended: {
          status: "page",
          view: "runs",
          rows: [row(extendedSummary, extendedRef)],
          nextCursor: 1,
          hasMore: false,
          snapshotRevision: 1,
          readTransactionId: "read_run_extended",
        },
      },
      {
        operation: FABRIC_OPERATIONS.projectionDetailRead,
        legacy: {
          status: "current",
          detailRef: legacyRef,
          detail: fact(legacyDetail),
          snapshotRevision: 1,
          readTransactionId: "read_detail_legacy",
        },
        extended: {
          status: "current",
          detailRef: extendedRef,
          detail: fact(extendedDetail),
          snapshotRevision: 1,
          readTransactionId: "read_detail_extended",
        },
      },
    ] as const;

    for (const fixture of fixtures) {
      const legacy = parseOperationResult(fixture.operation, fixture.legacy);
      const extended = parseOperationResult(fixture.operation, fixture.extended);
      expect(assertOperationResultFeatureShape(
        fixture.operation,
        legacyFeatures,
        legacy,
      )).toBe(legacy);
      expect(assertOperationResultFeatureShape(
        fixture.operation,
        extendedFeatures,
        extended,
      )).toBe(extended);
      expect(() => assertOperationResultFeatureShape(
        fixture.operation,
        extendedFeatures,
        legacy,
      )).toThrow(expect.objectContaining({ reason: "missing-negotiated-field" }));
      expect(() => assertOperationResultFeatureShape(
        fixture.operation,
        legacyFeatures,
        extended,
      )).toThrow(expect.objectContaining({ reason: "unnegotiated-field" }));
    }
  });
});

describe("negotiated gate supersession result shape", () => {
  it("rejects a system-supersession arm on every gate result when the feature was not negotiated", () => {
    const base = OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.scopedGateCreate].result as Record<string, unknown>;
    const gate = parseOperationResult(FABRIC_OPERATIONS.scopedGateCreate, {
      ...base,
      status: "superseded",
      revision: 2,
      resolution: {
        kind: "system-supersession",
        cause: { kind: "operator-command", ref: "command_01" },
        reason: "acceptance cycle exited",
        decidedAt: observedAt,
      },
    });
    const read = parseOperationResult(FABRIC_OPERATIONS.scopedGateRead, {
      status: "current",
      gate,
      readTransactionId: "read_gate_01",
      stateDigest: digestA,
    });
    for (const [operation, result] of [
      [FABRIC_OPERATIONS.scopedGateCreate, gate],
      [FABRIC_OPERATIONS.scopedGateResolve, gate],
      [FABRIC_OPERATIONS.scopedGateRead, read],
    ] as const) {
      expect(() => assertOperationResultFeatureShape(operation, [], result as never))
        .toThrow(expect.objectContaining({ code: "PROTOCOL_INCOMPATIBLE", reason: "unnegotiated-field" }));
      expect(assertOperationResultFeatureShape(
        operation,
        ["gate-system-supersession.v1"],
        result as never,
      )).toBe(result);
    }
  });
});

describe("typed operator detail read", () => {
  const detailRef = { kind: "project", projectId: "project_01", expectedRevision: 1 } as const;

  it("returns a source-labelled detail that matches the exact typed reference", () => {
    expect(parseOperationResult(FABRIC_OPERATIONS.projectionDetailRead, {
      status: "current",
      detailRef,
      detail: {
        freshness: "live",
        source: "fabric",
        revision: 1,
        observedAt,
        value: {
          kind: "project",
          projectId: "project_01",
          canonicalRoot: "/workspace/project",
          goal: "Ship",
          acceptedScopeRef: null,
          repositoryRevision: "revision_01",
        },
      },
      snapshotRevision: 4,
      readTransactionId: "read_tx_01",
    })).toMatchObject({ status: "current", detailRef });
  });

  it("rejects a detail discriminant that does not match its reference", () => {
    expect(() => parseOperationResult(FABRIC_OPERATIONS.projectionDetailRead, {
      status: "current",
      detailRef,
      detail: {
        freshness: "live",
        source: "fabric",
        revision: 1,
        observedAt,
        value: {
          kind: "run",
          coordinationRunId: "run_01",
          phase: "active",
          chairAgentId: "agent_01",
          chairGeneration: 1,
          health: "healthy",
        },
      },
      snapshotRevision: 4,
      readTransactionId: "read_tx_01",
    })).toThrowError(/detail kind does not match reference/iu);
  });
});

describe("closed operator action capability mapping", () => {
  it("maps every intent family to one existing capability without generic payload dispatch", () => {
    const requiredAction: unknown = Reflect.get(protocol, "requiredOperatorActionForIntent");
    expect(typeof requiredAction).toBe("function");
    if (typeof requiredAction !== "function") return;

    const cases = [
      [{ kind: "control", action: "pause" }, "pause"],
      [{ kind: "control", action: "resume" }, "resume"],
      [{ kind: "control", action: "cancel" }, "cancel"],
      [{ kind: "control", action: "steer" }, "steer"],
      [{ kind: "project-session-launch" }, "launch"],
      [{ kind: "project-session-drain" }, "drain"],
      [{ kind: "project-session-stop" }, "stop"],
      [{ kind: "daemon-drain" }, "drain"],
      [{ kind: "daemon-stop" }, "stop"],
      [{ kind: "git" }, "git"],
      [{ kind: "registered-external-effect" }, "external-effect"],
      [{ kind: "promotion" }, "external-effect"],
    ] as const;
    expect(cases.map(([intent]) => Reflect.apply(requiredAction, undefined, [intent]))).toStrictEqual(
      cases.map(([, action]) => action),
    );
  });
});

const taskTarget = {
  kind: "task",
  projectSessionId: "ps_01",
  coordinationRunId: "run_01",
  taskId: "task_01",
  expectedRevision: 4,
} as const;
const controlIntent = { kind: "control", action: "pause", target: taskTarget } as const;

const gitRepository = {
  repositoryRoot: "/workspace/project",
  worktreePath: "/workspace/project/.worktrees/writer",
  gitCommonDir: "/workspace/project/.git",
  commonDirectoryIdentityDigest: digestA,
  repositoryStateDigest: digestA,
  headDigest: digestA,
  indexDigest: digestA,
  worktreeDigest: digestA,
  remoteStateDigest: digestA,
  configDigest: digestA,
  worktreeRegistryDigest: digestA,
} as const;
const gitExecutionProfile = {
  profileId: "sealed_git_v1",
  revision: 1,
  digest: digestA,
  gitBinaryDigest: digestA,
  objectFormat: "sha1",
} as const;
const gitResultRecipe = {
  schemaVersion: 1,
  executionProfileDigest: digestA,
  resultRecipeDigest: digestA,
  beforeRepositoryStateDigest: digestA,
  expectedSuccessRepositoryStateDigest: digestB,
  expectedConflict: null,
  refUpdates: [],
  configUpdates: [],
  commitMappings: [],
  affectedPaths: [{ path: "src/index.ts", beforeDigest: null, afterDigest: digestB }],
  bounds: {
    maximumRefOrConfigUpdates: 64,
    maximumCommitMappings: 128,
    maximumConflictPaths: 4096,
  },
} as const;
const gitIntent = {
  kind: "git",
  authorisation: {
    projectId: "project_01",
    projectSessionId: "ps_01",
    expectedSessionRevision: 4,
    expectedSessionGeneration: 2,
    coordinationRunId: "run_01",
    expectedRunRevision: 5,
    expectedDependencyRevision: 3,
    authorityRef: digestA,
    expectedAuthorityRevision: 1,
    expectedGitAllowlistEpoch: 1,
    gitAllowlistDigest: digestA,
    repositoryRoot: gitRepository.repositoryRoot,
    worktreePath: gitRepository.worktreePath,
    repositoryStateDigest: gitRepository.repositoryStateDigest,
    executionProfileId: gitExecutionProfile.profileId,
    executionProfileRevision: gitExecutionProfile.revision,
    executionProfileDigest: gitExecutionProfile.digest,
    operationVariant: "stage",
    remoteBinding: null,
    resultRecipeDigest: gitResultRecipe.resultRecipeDigest,
    operationId: "git_operation_01",
    effectBindingDigest: digestA,
    decision: {
      kind: "preauthorised",
      grantId: "git_grant_01",
      expectedGrantRevision: 1,
      grantDigest: digestA,
    },
  },
  repository: gitRepository,
  executionProfile: gitExecutionProfile,
  operation: { variant: "stage", paths: ["src/index.ts"] },
  resultRecipe: gitResultRecipe,
} as const;

const externalIntent = {
  kind: "registered-external-effect",
  integrationId: "integration_release_01",
  expectedIntegrationGeneration: 2,
  operationId: "deploy_release",
  contractDigest: digestA,
  requestArtifactRef: artifact,
  targetId: "environment_staging",
  expectedTargetRevision: 3,
  idempotencyKey: "deploy_release_01",
} as const;

const releaseBinding = {
  acceptedDeliveryReceiptRef: artifact,
  artifactDigest: digestA,
  promotionAction: "promote-staging",
  target: "staging",
} as const;
const promotionIntent = {
  kind: "promotion",
  projectSessionId: "ps_01",
  coordinationRunId: "run_01",
  gateId: "gate_release_01",
  expectedGateRevision: 5,
  expectedGateStatus: "approved",
  releaseBinding,
} as const;
const launchIntent = {
  kind: "project-session-launch",
  projectId: "project_01",
  projectSessionId: "ps_01",
  expectedProjectRevision: 3,
  expectedSessionRevision: 4,
  expectedSessionGeneration: 2,
  trustRecordDigest: digestA,
  launchPacketRef: artifact,
  authorityRef: digestA,
  budgetRef: "budget_01",
  resourcePlanRef: { path: "launch/resources.json", digest: digestA },
  providerAdapterId: "claude-agent-sdk",
  providerActionId: "provider_action_launch_01",
  providerContractDigest: digestA,
  resourceStateDigest: digestA,
} as const;

describe("closed two-phase action intents", () => {
  it.each([
    controlIntent,
    { kind: "control", action: "resume", target: taskTarget },
    { kind: "control", action: "cancel", target: taskTarget, reason: "Scope cancelled" },
    { kind: "control", action: "steer", target: taskTarget, instruction: "Use the accepted design.", evidenceRefs: [artifact] },
    launchIntent,
    { kind: "project-session-drain", projectSessionId: "ps_01", expectedSessionRevision: 4, expectedSessionGeneration: 2, expectedGlobalStateRevision: 8 },
    { kind: "project-session-stop", projectSessionId: "ps_01", expectedSessionRevision: 4, expectedSessionGeneration: 2, expectedGlobalStateRevision: 8, drainReceiptRef: artifact },
    { kind: "daemon-drain", expectedDaemonGeneration: 2, expectedGlobalStateRevision: 8 },
    { kind: "daemon-stop", expectedDaemonGeneration: 2, expectedGlobalStateRevision: 8, drainReceiptRef: artifact },
    gitIntent,
    externalIntent,
    promotionIntent,
  ] as const)("accepts the exact preview intent %# without a free-form payload", (intent) => {
    expect(parseOperationInput(FABRIC_OPERATIONS.operatorActionPreview, {
      command: operatorCommand,
      projectId: "project_01",
      intent,
    })).toMatchObject({ intent });
  });

  it("rejects shell, argv and a free Git operation name", () => {
    for (const extra of [
      { shell: "git push" },
      { argv: ["git", "push"] },
      { operationName: "arbitrary-git-effect" },
    ]) {
      expect(() => parseOperationInput(FABRIC_OPERATIONS.operatorActionPreview, {
        command: operatorCommand,
        projectId: "project_01",
        intent: { ...gitIntent, ...extra },
      })).toThrowError(/unknown field|allowed variant/iu);
    }
  });

  it.each([
    "headDigest",
    "indexDigest",
    "worktreeDigest",
    "remoteStateDigest",
  ] as const)("requires Git state fence %s", (field) => {
    const repository = { ...gitIntent.repository } as Record<string, unknown>;
    delete repository[field];
    expect(() => parseOperationInput(FABRIC_OPERATIONS.operatorActionPreview, {
      command: operatorCommand,
      projectId: "project_01",
      intent: { ...gitIntent, repository },
    })).toThrowError(new RegExp(field, "iu"));
  });

  it("rejects sibling operation fields and an unbound force push", () => {
    expect(() => parseOperationInput(FABRIC_OPERATIONS.operatorActionPreview, {
      command: operatorCommand,
      projectId: "project_01",
      intent: {
        ...gitIntent,
        operation: { ...gitIntent.operation, sourceRef: "refs/heads/main" },
      },
    })).toThrowError(/sourceRef|allowed variant/iu);
    expect(() => parseOperationInput(FABRIC_OPERATIONS.operatorActionPreview, {
      command: operatorCommand,
      projectId: "project_01",
      intent: {
        ...gitIntent,
        authorisation: { ...gitIntent.authorisation, operationVariant: "push-force-with-lease" },
        operation: {
          variant: "push-force-with-lease",
          remote: {
            registrationId: "remote_01",
            revision: 1,
            generation: 1,
            remoteName: "origin",
            targetDigest: digestA,
            adapterId: "local_git",
            adapterContractDigest: digestA,
          },
          sourceRef: "refs/heads/feature",
          destinationRef: "refs/heads/feature",
          sourceObjectDigest: digestA,
          destinationObjectDigest: digestA,
        },
      },
    })).toThrowError(/expectedRemoteObjectDigest|allowed variant/iu);
  });

  it("keeps promotion distinct from broad registered external-effect authority", () => {
    expect(() => parseOperationInput(FABRIC_OPERATIONS.operatorActionPreview, {
      command: operatorCommand,
      projectId: "project_01",
      intent: { ...externalIntent, releaseBinding },
    })).toThrowError(/releaseBinding|allowed variant/iu);
  });

  it("rejects a Git preview after any bound repository state changes", () => {
    const assertCurrent: unknown = Reflect.get(protocol, "assertGitIntentState");
    expect(typeof assertCurrent).toBe("function");
    if (typeof assertCurrent !== "function") return;

    expect(() => Reflect.apply(assertCurrent, undefined, [gitIntent, {
      revision: 4,
      projectId: "project_01",
      projectSessionId: "ps_01",
      sessionRevision: 4,
      sessionGeneration: 2,
      coordinationRunId: "run_01",
      runRevision: 5,
      dependencyRevision: 3,
      authorityRef: digestA,
      authorityRevision: 1,
      gitAllowlistEpoch: 1,
      gitAllowlistDigest: digestA,
      repository: { ...gitRepository, repositoryStateDigest: digestB },
      executionProfile: gitExecutionProfile,
      remoteBinding: null,
      grant: { grantId: "git_grant_01", revision: 1, grantDigest: digestA },
    }])).toThrowError(/repository or execution profile changed/iu);
  });

  it("rejects unknown or stale registered external-effect contracts", () => {
    const assertContract: unknown = Reflect.get(protocol, "assertRegisteredExternalEffectContract");
    expect(typeof assertContract).toBe("function");
    if (typeof assertContract !== "function") return;

    expect(() => Reflect.apply(assertContract, undefined, [externalIntent, {
      integrationId: "integration_release_01",
      integrationGeneration: 2,
      operationContracts: {},
      targetRevisions: { environment_staging: 3 },
    }])).toThrowError(/external operation is not registered/iu);
  });

  it("requires the exact approved release gate and binding for promotion", () => {
    const assertGate: unknown = Reflect.get(protocol, "assertPromotionIntentGate");
    expect(typeof assertGate).toBe("function");
    if (typeof assertGate !== "function") return;
    const gate = {
      ...(OPERATION_CONTRACT_FIXTURES[FABRIC_OPERATIONS.scopedGateCreate].result as Record<string, unknown>),
      gateId: "gate_release_01",
      projectSessionId: "ps_01",
      coordinationRunId: "run_01",
      revision: 5,
      scope: { kind: "release" },
      status: "approved",
      releaseBinding,
    };

    expect(() => Reflect.apply(assertGate, undefined, [
      { ...promotionIntent, releaseBinding: { ...releaseBinding, target: "production" } },
      gate,
    ])).toThrowError(/release binding does not match/iu);
  });
});

const preview = {
  previewId: "preview_01",
  previewRevision: 1,
  previewDigest: digestA,
  intent: controlIntent,
  intentDigest: digestB,
  beforeStateDigest: digestA,
  consequenceClass: "consequential",
  evidenceRefs: [artifact],
  gateIds: ["gate_01"],
  confirmationMode: "echo",
  expiresAt: "2026-07-11T11:00:00.000Z",
} as const;

describe("preview, confirmation, status and observe-only reconciliation", () => {
  it("returns an immutable intent-bound preview and exact commit receipt", () => {
    expect(parseOperationResult(FABRIC_OPERATIONS.operatorActionPreview, preview)).toStrictEqual(preview);
    const commit = {
      command: { ...operatorCommand, commandId: "command_commit_01" },
      projectId: "project_01",
      previewId: "preview_01",
      expectedPreviewRevision: 1,
      previewDigest: digestA,
      expectedIntentDigest: digestB,
      confirmation: { kind: "echo", echoedPreviewDigest: digestA },
    } as const;
    expect(parseOperationInput(FABRIC_OPERATIONS.operatorActionCommit, commit)).toStrictEqual(commit);
    expect(parseOperationResult(FABRIC_OPERATIONS.operatorActionCommit, {
      commandId: "command_commit_01",
      previewId: "preview_01",
      previewRevision: 1,
      intentDigest: digestB,
      beforeStateDigest: digestA,
      afterStateDigest: digestB,
      evidenceRefs: [artifact],
      committedAt: observedAt,
    })).toMatchObject({ commandId: "command_commit_01", intentDigest: digestB });
  });

  it("rejects an echoed confirmation for another preview digest", () => {
    expect(() => parseOperationInput(FABRIC_OPERATIONS.operatorActionCommit, {
      command: { ...operatorCommand, commandId: "command_commit_01" },
      projectId: "project_01",
      previewId: "preview_01",
      expectedPreviewRevision: 1,
      previewDigest: digestA,
      expectedIntentDigest: digestB,
      confirmation: { kind: "echo", echoedPreviewDigest: digestB },
    })).toThrowError(/echoed preview digest does not match/iu);
  });

  it("rejects commit after the preview revision, digest, intent or before-state changes", () => {
    const assertPreview: unknown = Reflect.get(protocol, "assertOperatorActionPreviewCurrent");
    expect(typeof assertPreview).toBe("function");
    if (typeof assertPreview !== "function") return;

    expect(() => Reflect.apply(assertPreview, undefined, [preview, {
      previewId: "preview_01",
      previewRevision: 2,
      previewDigest: digestA,
      intentDigest: digestB,
      beforeStateDigest: digestA,
      observedAt: "2026-07-11T10:30:00.000Z",
    }])).toThrowError(/preview revision is stale/iu);
  });

  it("preserves ambiguous state and permits only a new observe-only reconciliation command", () => {
    expect(parseOperationResult(FABRIC_OPERATIONS.operatorActionStatus, {
      status: "ambiguous",
      commandId: "command_commit_01",
      intentDigest: digestB,
      attemptGeneration: 2,
      effectRef: artifact,
    })).toMatchObject({ status: "ambiguous", attemptGeneration: 2 });
    const reconcile = {
      command: { ...operatorCommand, commandId: "command_reconcile_01" },
      projectId: "project_01",
      targetCommandId: "command_commit_01",
      expectedStatus: "ambiguous",
      expectedAttemptGeneration: 2,
      mode: "observe-only",
    } as const;
    expect(parseOperationInput(FABRIC_OPERATIONS.operatorActionReconcile, reconcile)).toStrictEqual(reconcile);
    expect(() => parseOperationInput(FABRIC_OPERATIONS.operatorActionReconcile, {
      ...reconcile,
      mode: "redispatch",
    })).toThrowError(/observe-only|allowed variant/iu);
    expect(() => parseOperationInput(FABRIC_OPERATIONS.operatorActionReconcile, {
      ...reconcile,
      command: { ...reconcile.command, commandId: reconcile.targetCommandId },
    })).toThrowError(/new command ID/iu);
  });

  it("represents changed-intent dedupe and external-contract rejection without effect redispatch", () => {
    for (const code of ["dedupe-conflict", "external-contract-unknown"] as const) {
      expect(parseOperationResult(FABRIC_OPERATIONS.operatorActionStatus, {
        status: "rejected",
        commandId: "command_commit_01",
        intentDigest: digestB,
        code,
        evidenceRefs: [artifact],
      })).toMatchObject({ status: "rejected", code });
    }
  });
});

function consoleClient(allowedOperations: readonly string[], features: readonly string[]) {
  return createOperatorClient({
    features,
    principal: {
      kind: "operator",
      operatorId: "operator_01",
      projectId: "project_01",
      projectAuthorityGeneration: 1,
      principalGeneration: 1,
    },
    allowedOperations: new Set(allowedOperations),
    call: () => Promise.reject(new Error("not called")),
    close: () => Promise.resolve(),
  } as never);
}

describe("complete-grant Console facade", () => {
  const reads = [
    FABRIC_OPERATIONS.scopedGateRead,
    FABRIC_OPERATIONS.projectionViewPage,
    FABRIC_OPERATIONS.projectionRunPage,
    FABRIC_OPERATIONS.projectionDetailRead,
  ] as const;
  const mutations = [
    FABRIC_OPERATIONS.operatorActionPreview,
    FABRIC_OPERATIONS.operatorActionCommit,
    FABRIC_OPERATIONS.operatorActionStatus,
    FABRIC_OPERATIONS.operatorActionReconcile,
  ] as const;
  const projectionFeatures = [
    "scoped-gate-read.v1",
    "operator-projection.v2",
    "run-scoped-projection.v1",
    "agent-topology-projection.v1",
    "work-facts-projection.v1",
  ] as const;

  it("degrades honestly to read-only when mutation grants are absent or partial", () => {
    const readOnly = consoleClient(reads, projectionFeatures);
    expect(readOnly.console).toMatchObject({ readOnly: true });
    expect(readOnly.console).not.toHaveProperty("actions");
    const partial = consoleClient([...reads, mutations[0]], [
      ...projectionFeatures,
      "operator-actions.v1",
    ]);
    expect(partial.console).toMatchObject({ readOnly: true });
    expect(partial.console).not.toHaveProperty("actions");
  });

  it("exposes mutations only after all four action operations are granted", () => {
    const full = consoleClient([...reads, ...mutations], [
      ...projectionFeatures,
      "operator-actions.v1",
    ]);
    expect(full.console).toMatchObject({ readOnly: false });
    expect(full.console?.actions?.preview).toBeTypeOf("function");
    expect(full.console?.actions?.commit).toBeTypeOf("function");
    expect(full.console?.actions?.status).toBeTypeOf("function");
    expect(full.console?.actions?.reconcile).toBeTypeOf("function");
  });

  it("omits the Console facade when any canonical read operation is missing", () => {
    expect(consoleClient(reads.slice(0, 3), projectionFeatures)).not.toHaveProperty(
      "console",
    );
  });

  it("rejects a peer that does not negotiate the run-scoped result shape", () => {
    for (const missing of [
      "run-scoped-projection.v1",
      "agent-topology-projection.v1",
      "work-facts-projection.v1",
    ]) {
      expect(consoleClient(
        reads,
        projectionFeatures.filter((feature) => feature !== missing),
      )).not.toHaveProperty("console");
    }
  });
});

describe("negotiated declared-run-progress result shape", () => {
  const legacyFeatures = [
    "operator-projection.v1",
    "operator-projection.v2",
    "run-session-projection.v1",
  ] as const;
  const extendedFeatures = [...legacyFeatures, "declared-run-progress.v2"] as const;
  const openProgress = {
    plan: "open",
    counts: { blocked: 0, ready: 1, active: 2, complete: 3, cancelled: 0, degraded: 0 },
  } as const;
  const unknownProgress = { plan: "unknown", reason: "unrecognised task state: parked" } as const;
  const legacySummary = {
    kind: "run",
    projectSessionId: "ps_01",
    phase: "active",
    health: "healthy",
    nextMilestone: "verification",
  } as const;
  const runRef = {
    kind: "run",
    projectSessionId: "ps_01",
    coordinationRunId: "run_01",
    expectedRevision: 1,
  } as const;
  const legacyDetail = {
    kind: "run",
    projectSessionId: "ps_01",
    coordinationRunId: "run_01",
    phase: "active",
    chairAgentId: "chair_01",
    chairGeneration: 1,
    health: "healthy",
  } as const;
  const runsPage = (summary: unknown) => ({
    status: "page",
    view: "runs",
    rows: [row(summary, runRef)],
    nextCursor: 1,
    hasMore: false,
    snapshotRevision: 1,
    readTransactionId: "read_declared_progress",
  });
  const detailRead = (detail: unknown) => ({
    status: "current",
    detailRef: runRef,
    detail: {
      freshness: "live",
      source: "fabric",
      revision: 1,
      observedAt,
      value: detail,
    },
    snapshotRevision: 4,
    readTransactionId: "read_declared_progress_detail",
  });

  it("accepts every tagged progress arm on run rows and run detail", () => {
    const finiteProgress = {
      plan: "finite",
      planRevision: 2,
      counts: openProgress.counts,
      declaredTaskDenominator: 6,
    } as const;
    for (const progress of [openProgress, unknownProgress, finiteProgress]) {
      expect(parseOperationResult(
        FABRIC_OPERATIONS.projectionViewPage,
        runsPage({ ...legacySummary, declaredProgress: progress }),
      )).toMatchObject({ status: "page", view: "runs" });
      expect(parseOperationResult(
        FABRIC_OPERATIONS.projectionDetailRead,
        detailRead({ ...legacyDetail, declaredProgress: progress }),
      )).toMatchObject({ status: "current" });
    }
  });

  it("rejects malformed finite arms, negative counts and denominator or percentage keys", () => {
    expect(() => parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      runsPage({
        ...legacySummary,
        declaredProgress: {
          plan: "finite",
          planRevision: 2,
          counts: openProgress.counts,
          declaredTaskDenominator: 0,
        },
      }),
    )).toThrowError();
    expect(() => parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      runsPage({
        ...legacySummary,
        declaredProgress: {
          plan: "finite",
          planRevision: 2,
          counts: openProgress.counts,
          declaredTaskDenominator: 1,
        },
      }),
    )).toThrow("counts exceed declaredTaskDenominator");
    expect(() => parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      runsPage({
        ...legacySummary,
        declaredProgress: {
          plan: "open",
          counts: { ...openProgress.counts, ready: -1 },
        },
      }),
    )).toThrowError();
    expect(() => parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      runsPage({
        ...legacySummary,
        declaredProgress: { ...openProgress, total: 6 },
      }),
    )).toThrowError();
    expect(() => parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      runsPage({
        ...legacySummary,
        declaredProgress: { ...openProgress, percentage: 50 },
      }),
    )).toThrowError();
  });

  it("requires uniform negotiated presence on run rows and run detail", () => {
    const legacyPage = parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      runsPage(legacySummary),
    );
    const extendedPage = parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      runsPage({ ...legacySummary, declaredProgress: openProgress }),
    );
    const legacyRead = parseOperationResult(
      FABRIC_OPERATIONS.projectionDetailRead,
      detailRead(legacyDetail),
    );
    const extendedRead = parseOperationResult(
      FABRIC_OPERATIONS.projectionDetailRead,
      detailRead({ ...legacyDetail, declaredProgress: openProgress }),
    );
    for (const [operation, legacy, extended] of [
      [FABRIC_OPERATIONS.projectionViewPage, legacyPage, extendedPage],
      [FABRIC_OPERATIONS.projectionDetailRead, legacyRead, extendedRead],
    ] as const) {
      expect(assertOperationResultFeatureShape(operation, legacyFeatures, legacy as never)).toBe(legacy);
      expect(assertOperationResultFeatureShape(operation, extendedFeatures, extended as never)).toBe(extended);
      expect(() => assertOperationResultFeatureShape(operation, extendedFeatures, legacy as never))
        .toThrow(expect.objectContaining({ reason: "missing-negotiated-field" }));
      expect(() => assertOperationResultFeatureShape(operation, legacyFeatures, extended as never))
        .toThrow(expect.objectContaining({ reason: "unnegotiated-field" }));
    }
  });
});

describe("negotiated run-identity result shape", () => {
  const legacyFeatures = [
    "operator-projection.v1",
    "operator-projection.v2",
    "run-session-projection.v1",
  ] as const;
  const extendedFeatures = [...legacyFeatures, "run-identity-projection.v2"] as const;
  const workstream = {
    workstreamId: "ws_01",
    deliveryRunId: "delivery_01",
    leadAgentId: "lead_01",
    state: "active",
    updatedAt: observedAt,
  } as const;
  const coordinationIdentity = {
    runKind: "coordination",
    chairAgentId: "chair_01",
    acceptedScopeRef: { path: "scope.md", digest: `sha256:${"1".repeat(64)}` },
    currentPlanRef: { path: "plan.md", digest: `sha256:${"2".repeat(64)}` },
    planRevision: 2,
    workstreams: [workstream],
    lastEventAt: observedAt,
  } as const;
  const legacySummary = {
    kind: "run",
    projectSessionId: "ps_01",
    phase: "active",
    health: "healthy",
    nextMilestone: "verification",
  } as const;
  const runRef = {
    kind: "run",
    projectSessionId: "ps_01",
    coordinationRunId: "run_01",
    expectedRevision: 1,
  } as const;
  const legacyDetail = {
    kind: "run",
    projectSessionId: "ps_01",
    coordinationRunId: "run_01",
    phase: "active",
    chairAgentId: "chair_01",
    chairGeneration: 1,
    health: "healthy",
  } as const;
  const runsPage = (summary: unknown) => ({
    status: "page",
    view: "runs",
    rows: [row(summary, runRef)],
    nextCursor: 1,
    hasMore: false,
    snapshotRevision: 1,
    readTransactionId: "read_run_identity",
  });
  const detailRead = (detail: unknown) => ({
    status: "current",
    detailRef: runRef,
    detail: {
      freshness: "live",
      source: "fabric",
      revision: 1,
      observedAt,
      value: detail,
    },
    snapshotRevision: 4,
    readTransactionId: "read_run_identity_detail",
  });

  it("accepts the coordination identity arm with and without workstreams on rows and detail", () => {
    for (const identity of [
      coordinationIdentity,
      { ...coordinationIdentity, workstreams: [] },
      { ...coordinationIdentity, lastEventAt: null },
    ]) {
      expect(parseOperationResult(
        FABRIC_OPERATIONS.projectionViewPage,
        runsPage({ ...legacySummary, identity }),
      )).toMatchObject({ status: "page", view: "runs" });
      expect(parseOperationResult(
        FABRIC_OPERATIONS.projectionDetailRead,
        detailRead({ ...legacyDetail, identity }),
      )).toMatchObject({ status: "current" });
    }
  });

  it("rejects a detail identity whose chair contradicts the enclosing run", () => {
    expect(() => parseOperationResult(
      FABRIC_OPERATIONS.projectionDetailRead,
      detailRead({
        ...legacyDetail,
        identity: { ...coordinationIdentity, chairAgentId: "chair_other" },
      }),
    )).toThrow("identity chair must match the enclosing run chair");
  });

  it("rejects duplicate workstream and delivery-run identities on rows and detail", () => {
    for (const duplicate of [
      { ...workstream, deliveryRunId: "delivery_02" },
      { ...workstream, workstreamId: "ws_02" },
    ]) {
      const identity = { ...coordinationIdentity, workstreams: [workstream, duplicate] };
      expect(() => parseOperationResult(
        FABRIC_OPERATIONS.projectionViewPage,
        runsPage({ ...legacySummary, identity }),
      )).toThrow("workstreams must have unique workstreamId and deliveryRunId values");
      expect(() => parseOperationResult(
        FABRIC_OPERATIONS.projectionDetailRead,
        detailRead({ ...legacyDetail, identity }),
      )).toThrow("workstreams must have unique workstreamId and deliveryRunId values");
    }
  });

  it("accepts explicit absent plan bindings and rejects premature run kinds or unclosed workstream states", () => {
    expect(parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      runsPage({
        ...legacySummary,
        identity: {
          ...coordinationIdentity,
          acceptedScopeRef: null,
          currentPlanRef: null,
          planRevision: null,
        },
      }),
    )).toMatchObject({ status: "page", view: "runs" });
    expect(() => parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      runsPage({
        ...legacySummary,
        identity: { ...coordinationIdentity, runKind: "delivery-workstream" },
      }),
    )).toThrowError();
    expect(() => parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      runsPage({
        ...legacySummary,
        identity: { ...coordinationIdentity, acceptedScopeRef: null },
      }),
    )).toThrow("acceptedScopeRef is required for a declared plan");
    expect(() => parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      runsPage({
        ...legacySummary,
        identity: {
          ...coordinationIdentity,
          workstreams: [{ ...workstream, state: "paused" }],
        },
      }),
    )).toThrowError();
    expect(() => parseOperationResult(
      FABRIC_OPERATIONS.projectionDetailRead,
      detailRead({
        ...legacyDetail,
        identity: { ...coordinationIdentity, chairAgentId: undefined },
      }),
    )).toThrowError();
  });

  it("requires finite progress to name the same plan revision as run identity", () => {
    const finiteProgress = {
      plan: "finite",
      planRevision: 1,
      counts: { blocked: 0, ready: 0, active: 0, complete: 1, cancelled: 0, degraded: 0 },
      declaredTaskDenominator: 2,
    } as const;
    for (const result of [
      [FABRIC_OPERATIONS.projectionViewPage, runsPage({
        ...legacySummary,
        declaredProgress: finiteProgress,
        identity: coordinationIdentity,
      })],
      [FABRIC_OPERATIONS.projectionDetailRead, detailRead({
        ...legacyDetail,
        declaredProgress: finiteProgress,
        identity: coordinationIdentity,
      })],
    ] as const) {
      expect(() => parseOperationResult(result[0], result[1] as never))
        .toThrow("finite progress planRevision must match run identity planRevision");
    }
  });

  it("requires uniform negotiated presence on run rows and run detail", () => {
    const legacyPage = parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      runsPage(legacySummary),
    );
    const extendedPage = parseOperationResult(
      FABRIC_OPERATIONS.projectionViewPage,
      runsPage({ ...legacySummary, identity: coordinationIdentity }),
    );
    const legacyRead = parseOperationResult(
      FABRIC_OPERATIONS.projectionDetailRead,
      detailRead(legacyDetail),
    );
    const extendedRead = parseOperationResult(
      FABRIC_OPERATIONS.projectionDetailRead,
      detailRead({ ...legacyDetail, identity: coordinationIdentity }),
    );
    for (const [operation, legacy, extended] of [
      [FABRIC_OPERATIONS.projectionViewPage, legacyPage, extendedPage],
      [FABRIC_OPERATIONS.projectionDetailRead, legacyRead, extendedRead],
    ] as const) {
      expect(assertOperationResultFeatureShape(operation, legacyFeatures, legacy as never)).toBe(legacy);
      expect(assertOperationResultFeatureShape(operation, extendedFeatures, extended as never)).toBe(extended);
      expect(() => assertOperationResultFeatureShape(operation, extendedFeatures, legacy as never))
        .toThrow(expect.objectContaining({ reason: "missing-negotiated-field" }));
      expect(() => assertOperationResultFeatureShape(operation, legacyFeatures, extended as never))
        .toThrow(expect.objectContaining({ reason: "unnegotiated-field" }));
    }
  });
});
