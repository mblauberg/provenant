import { readFile } from "node:fs/promises";

import { describe, expect, it, vi } from "vitest";

import type {
  NegotiatedOperatorClient,
  OperatorActionClient,
  OperatorActionCommitRequest,
  OperatorActionPreviewRequest,
  OperatorCapabilityCredential,
  OperatorProjectionSnapshot,
  ProjectId,
  ProjectSessionId,
  ScopedGate,
  Sha256Digest,
  Timestamp,
} from "@local/agent-fabric-protocol";

import {
  evaluateUsabilityManifest,
  parseUsabilityManifest,
  REQUIRED_USABILITY_ACTION_IDS,
} from "../src/evaluation.js";
import { reduceFabricPointer, renderFabricConsoleFrame } from "../src/index.js";
import { ConsoleController } from "../src/controller.js";
import { createEmptyViewPages, revisionFromProtocol } from "../src/model.js";
import { createProductionConsoleActionPlanner } from "../src/production-composition.js";
import type { FabricConsoleDataset } from "../src/protocol-adapter.js";
import { createProductionConsoleWorkflowPlanner } from "../src/workflow.js";

const dependencies = {
  render: renderFabricConsoleFrame,
  reducePointer: reduceFabricPointer,
  identify: async ({ fixture, repetition }: {
    fixture: ReturnType<typeof parseUsabilityManifest>["fixtures"][number];
    repetition: number;
  }) => ({
    observer: "automated-proxy" as const,
    durationMs: 1_500 + repetition * 100,
    topAttentionId: fixture.expectedTopAttentionId,
    answers: fixture.expectedAnswers,
  }),
};

const fixtureUrl = new URL(
  "../evals/usability-fixtures.v1.json",
  import.meta.url,
);

async function manifestValue(): Promise<unknown> {
  return JSON.parse(await readFile(fixtureUrl, "utf8")) as unknown;
}

const flowDigest = (`sha256:${"a".repeat(64)}`) as Sha256Digest;
const flowObservedAt = "2026-07-12T00:00:00.000Z" as Timestamp;
const flowProjectId = "project_evaluation_flow" as ProjectId;
const flowSessionId = "session_evaluation_flow" as ProjectSessionId;
const flowCredential = {
  capabilityId: "capability_evaluation_flow",
  token: "evaluation-flow-secret",
} as OperatorCapabilityCredential;

function mutationFlowDataset(): FabricConsoleDataset {
  const pages = createEmptyViewPages();
  const snapshot: OperatorProjectionSnapshot = {
    schemaVersion: 1,
    snapshotRevision: 11,
    readTransactionId: "evaluation_flow_snapshot",
    project: {
      freshness: "live",
      source: "fabric",
      revision: 3,
      observedAt: flowObservedAt,
      value: { projectId: flowProjectId, canonicalRoot: "/repo" },
    },
    session: {
      freshness: "live",
      source: "fabric",
      revision: 8,
      observedAt: flowObservedAt,
      value: {
        projectSessionId: flowSessionId,
        projectId: flowProjectId,
        mode: "coordinated",
        state: "active",
        revision: 8,
        generation: 2,
        authorityRef: flowDigest,
        budgetRef: "budget_evaluation_flow",
        launchPacketRef: { path: "launch/packet.json" as never, digest: flowDigest },
        membershipRevision: 1,
        origin: { kind: "operator-launch", operatorId: "operator_evaluation_flow" as never },
      },
    },
    runs: {
      freshness: "live",
      source: "fabric",
      revision: 4,
      observedAt: flowObservedAt,
      value: [],
    },
    attention: {
      freshness: "live",
      source: "fabric",
      revision: 4,
      observedAt: flowObservedAt,
      value: [],
    },
    capacity: {
      freshness: "live",
      source: "fabric",
      revision: 11,
      observedAt: flowObservedAt,
      value: {},
    },
    cursor: 0,
    stateDigest: flowDigest,
  };
  return {
    connection: { state: "live", compatibility: { mode: "current" } },
    snapshot,
    snapshotRevision: revisionFromProtocol(11),
    cursor: 0,
    loadedAtMs: Date.parse(flowObservedAt),
    canMutate: true,
    pages: {
      ...pages,
      runs: {
        view: "runs",
        rows: [{
          view: "runs",
          stableId: "run_evaluation_flow",
          revision: revisionFromProtocol(4),
          urgency: "normal",
          freshness: {
            state: "live",
            source: "fabric",
            revision: revisionFromProtocol(4),
            observedAt: flowObservedAt,
            ageMs: 0,
          },
          summary: {
            kind: "run",
            projectSessionId: flowSessionId,
            phase: "active",
            health: "healthy",
            nextMilestone: "pause safely",
            declaredProgress: { plan: "open", counts: { blocked: 0, ready: 0, active: 1, complete: 0, cancelled: 0, degraded: 0 } },
            identity: {
              runKind: "coordination",
              chairAgentId: "agent_evaluation_chair" as never,
              acceptedScopeRef: null,
              currentPlanRef: null,
              planRevision: null,
              workstreams: [],
              lastEventAt: flowObservedAt,
            },
          },
          detailRef: {
            kind: "run",
            projectSessionId: flowSessionId,
            coordinationRunId: "run_evaluation_flow" as never,
            expectedRevision: 4,
          },
          actionAvailability: {
            state: "available",
            actions: ["pause", "cancel"],
            requiresPreview: true,
          },
        }],
        nextCursor: 1,
        hasMore: false,
        snapshotRevision: revisionFromProtocol(11),
        readTransactionId: "evaluation_flow_runs",
      },
      attention: {
        view: "attention",
        rows: [{
          view: "attention",
          stableId: "attention_evaluation_gate",
          revision: revisionFromProtocol(7),
          urgency: "safety-integrity",
          freshness: {
            state: "live",
            source: "fabric",
            revision: revisionFromProtocol(7),
            observedAt: flowObservedAt,
            ageMs: 0,
          },
          summary: {
            kind: "attention",
            label: "Approval",
            priority: "safety-integrity",
            title: "Approve exact evaluation gate",
            gateBinding: {
              gateId: "gate_evaluation_flow" as never,
              gateRevision: 3,
              coordinationRunId: "run_evaluation_flow" as never,
            },
            nativeNotification: {
              kind: "feature-unavailable",
              status: "unavailable",
              reason: "feature-not-negotiated",
            },
          },
          detailRef: {
            kind: "run",
            projectSessionId: flowSessionId,
            coordinationRunId: "run_evaluation_flow" as never,
            expectedRevision: 4,
          },
          actionAvailability: { state: "read-only", reason: "state-ineligible" },
        }],
        nextCursor: 1,
        hasMore: false,
        snapshotRevision: revisionFromProtocol(11),
        readTransactionId: "evaluation_flow_attention",
      },
    },
  };
}

describe("versioned Console usability evaluation", () => {
  it("scripts lifecycle and gate preview, distinct confirmation and exact commit", async () => {
    const dataset = mutationFlowDataset();
    const preview = vi.fn(async (request: OperatorActionPreviewRequest) => ({
      previewId: "preview_evaluation_flow",
      previewRevision: 1,
      previewDigest: flowDigest,
      intent: request.intent,
      intentDigest: (`sha256:${"b".repeat(64)}`) as Sha256Digest,
      beforeStateDigest: (`sha256:${"c".repeat(64)}`) as Sha256Digest,
      consequenceClass: "consequential" as const,
      evidenceRefs: [],
      gateIds: [],
      confirmationMode: "explicit" as const,
      expiresAt: "2099-01-01T00:00:00.000Z" as Timestamp,
    }));
    const commit = vi.fn(async (request: OperatorActionCommitRequest) => ({
      commandId: request.command.commandId,
      previewId: request.previewId,
      previewRevision: request.expectedPreviewRevision,
      intentDigest: request.expectedIntentDigest,
      beforeStateDigest: (`sha256:${"c".repeat(64)}`) as Sha256Digest,
      afterStateDigest: (`sha256:${"d".repeat(64)}`) as Sha256Digest,
      evidenceRefs: [],
      committedAt: flowObservedAt,
    }));
    const actions: OperatorActionClient = {
      preview,
      commit,
      status: async (request) => ({ status: "not-found", commandId: request.commandId }),
      reconcile: async (request) => ({ status: "not-found", commandId: request.targetCommandId }),
    };
    const actionPlanner = createProductionConsoleActionPlanner({
      credential: flowCredential,
      operatorId: "operator_evaluation_flow" as never,
      clientId: "console_evaluation_flow" as never,
    });
    const actionController = new ConsoleController({
      dataset,
      actions,
      credential: flowCredential,
      projectId: flowProjectId,
      projectSessionId: flowSessionId,
      confirmationId: () => "confirmation_evaluation_flow",
      now: () => Date.parse(flowObservedAt),
    });
    actionController.select("runs", "run_evaluation_flow");
    const activation = {
      regionId: "action:pause",
      provenance: "keyboard" as const,
      eventId: "evaluation_pause_open",
      binding: {
        view: "runs" as const,
        itemId: "run_evaluation_flow",
        itemRevision: revisionFromProtocol(4),
        projectionRevision: revisionFromProtocol(11),
      },
    };
    const planned = await actionPlanner.plan({
      activation,
      dataset,
      state: actionController.state,
      draft: "",
    });
    if (planned === null) throw new Error("evaluation pause intent was not planned");
    await actionController.beginAction(planned);
    expect(preview).toHaveBeenCalledTimes(1);
    expect(commit).not.toHaveBeenCalled();
    expect(() => actionController.armConfirmation({
      eventId: activation.eventId,
      source: "keyboard",
    })).toThrow(/distinct input event/u);
    actionController.armConfirmation({ eventId: "evaluation_pause_arm", source: "keyboard" });
    const confirmationActivation = {
      ...activation,
      eventId: "evaluation_pause_confirm",
    };
    const confirmation = await actionPlanner.confirmation({
      activation: confirmationActivation,
      dataset,
      state: actionController.state,
      draft: "",
    });
    await expect(actionController.confirmAction({
      eventId: confirmationActivation.eventId,
      source: "keyboard",
    }, confirmation.command)).resolves.toMatchObject({ status: "committed" });
    expect(commit).toHaveBeenCalledTimes(1);
    expect(commit).toHaveBeenCalledWith(expect.objectContaining({
      expectedPreviewRevision: 1,
      previewDigest: flowDigest,
      confirmation: { kind: "explicit", confirmationId: "confirmation_evaluation_flow" },
    }));

    const gate: ScopedGate = {
      gateId: "gate_evaluation_flow" as never,
      projectSessionId: flowSessionId,
      coordinationRunId: "run_evaluation_flow" as never,
      scope: { kind: "run" },
      affectedTaskIds: [],
      dependencyRevision: 1,
      blockedOperationIds: [],
      enforcementPoints: ["scoped-barrier"],
      question: "Approve exact evaluation gate?",
      reason: "Evaluation must prove a bound decision.",
      options: ["Approve", "Request changes"],
      recommendation: "Approve",
      consequences: ["The scripted gate closes."],
      evidenceRefs: [],
      revision: 3,
      createdByRef: "chair_evaluation_flow",
      expectedApproverRef: "operator_evaluation_flow",
      status: "pending",
    };
    const gateRead = vi.fn(async () => ({
      status: "current" as const,
      gate,
      readTransactionId: "evaluation_gate_read",
      stateDigest: flowDigest,
    }));
    const resolve = vi.fn(async () => gate);
    const client: NegotiatedOperatorClient = {
      kind: "operator",
      features: [],
      operations: {},
      close: async () => {},
      gates: { create: vi.fn(), resolve },
      console: {
        readOnly: false,
        launchAvailable: false,
        actions,
        gates: { read: gateRead },
        projection: { viewPage: vi.fn(), runPage: vi.fn(), readDetail: vi.fn() },
      },
    };
    const workflow = createProductionConsoleWorkflowPlanner({
      client,
      credential: flowCredential,
      operatorId: "operator_evaluation_flow" as never,
      clientId: "console_evaluation_flow" as never,
      projectId: flowProjectId,
    });
    const gateBinding = {
      view: "attention" as const,
      itemId: "attention_evaluation_gate",
      itemRevision: revisionFromProtocol(7),
      projectionRevision: revisionFromProtocol(11),
    };
    const gateReview = await workflow.prepareGuided({
      action: "accept",
      binding: gateBinding,
      raw: "",
      dataset,
      eventId: "evaluation_gate_open",
    });
    expect(gateRead).toHaveBeenCalledWith(expect.objectContaining({
      gateId: gate.gateId,
      expectedRevision: gate.revision,
    }));
    expect(resolve).not.toHaveBeenCalled();
    expect(() => workflow.arm(gateReview, "evaluation_gate_open"))
      .toThrow(/distinct confirmation gesture/u);
    const armedGate = workflow.arm(gateReview, "evaluation_gate_arm");
    await workflow.commit({ review: armedGate, eventId: "evaluation_gate_confirm" });
    expect(resolve).toHaveBeenCalledWith(expect.objectContaining({
      gateId: gate.gateId,
      status: "approved",
      command: expect.objectContaining({ expectedRevision: gate.revision }),
      decisionEvidence: expect.objectContaining({ kind: "typed-console" }),
    }));
  });

  it("passes interaction checks while withholding the human timing claim from proxy repetitions", async () => {
    const manifest = parseUsabilityManifest(await manifestValue());
    const report = await evaluateUsabilityManifest(manifest, dependencies);

    expect(manifest).toMatchObject({
      schemaVersion: 1,
      referenceViewport: { columns: 80, rows: 24 },
      repetitions: 3,
      minimumFieldSuccessRate: 0.95,
    });
    expect(manifest.fixtures.map(({ id }) => id)).toStrictEqual([
      "empty-healthy-work",
      "concurrent-multi-run",
      "gate-degraded-stale-conflict",
      "optional-notification-unavailable",
    ]);
    expect(report).toMatchObject({
      schemaVersion: 1,
      passed: false,
      interactionPassed: true,
      recordedIdentificationPassed: true,
      humanIdentificationPassed: false,
      topItemSuccessRate: 1,
      fieldSuccessRate: 1,
    });
    expect(report.observations).toHaveLength(12);
    expect(
      report.observations.every(
        (observation) =>
          observation.durationMs <= manifest.maximumIdentificationMs &&
          observation.visibleFreshness &&
          observation.allViewsReachable &&
          observation.focusVisible &&
          !observation.containsInferredPercentage &&
          observation.consequentialReviewRequired &&
          observation.optionalIntegrationIndependent &&
          observation.nativeNotificationVisible &&
          observation.dynamicResizeSafe &&
          observation.artifactReviewSafe &&
          observation.actionMatrixSafe &&
          observation.actionMatrixFailures.length === 0 &&
          observation.scrollAndSelectionSafe &&
          observation.exactViewport,
      ),
    ).toBe(true);
    expect(report.observations.every((observation) =>
      observation.identificationObserver === "automated-proxy" &&
      observation.keyboardEventCount >= 8 &&
      observation.mouseEventCount >= 2 &&
      observation.scrollEventCount >= 2 &&
      observation.resizeEventCount >= 3
    )).toBe(true);
    expect([
      ...new Set(report.observations.flatMap(({ actionIdsCovered }) => actionIdsCovered)),
    ].sort()).toStrictEqual([...REQUIRED_USABILITY_ACTION_IDS].sort());
    expect(report.observations.every(({ keyboardActionIds, mouseActionIds }) =>
      keyboardActionIds.every((action) => mouseActionIds.includes(action)) &&
      mouseActionIds.every((action) => keyboardActionIds.includes(action))
    )).toBe(true);
  });

  it("exercises the exact invalid, inert, minimum, reference, and wide resize ladder", async () => {
    const seen = new Set<string>();
    const manifest = parseUsabilityManifest(await manifestValue());
    await evaluateUsabilityManifest(manifest, {
      ...dependencies,
      render: (dataset, controller, ui, viewport) => {
        seen.add(`${String(viewport.columns)}x${String(viewport.rows)}`);
        return renderFabricConsoleFrame(dataset, controller, ui, viewport);
      },
    });

    expect([...seen]).toEqual(expect.arrayContaining([
      "0x0",
      "29x5",
      "30x6",
      "80x24",
      "120x32",
      "200x10",
    ]));
  });

  it("fails dynamic resize safety when a non-inert frame has no enabled visible focus", async () => {
    const manifest = parseUsabilityManifest(await manifestValue());
    const report = await evaluateUsabilityManifest(manifest, {
      ...dependencies,
      render: (dataset, controller, ui, viewport) => {
        const frame = renderFabricConsoleFrame(dataset, controller, ui, viewport);
        if (
          frame.columns !== 30 ||
          frame.rows.length !== 6 ||
          frame.presentation.focusId === null
        ) return frame;
        const focused = frame.hitRegions.find(
          ({ enabled, id }) => enabled && id === frame.presentation.focusId,
        );
        if (focused === undefined) return frame;
        const rows = [...frame.rows];
        for (let y = focused.rect.y1 - 1; y < focused.rect.y2; y += 1) {
          const row = rows[y];
          if (row === undefined) continue;
          const start = focused.rect.x1 - 1;
          const end = focused.rect.x2;
          const segment = row.slice(start, end).replaceAll(">", " ");
          rows[y] = `${row.slice(0, start)}${segment}${row.slice(end)}`;
        }
        return {
          ...frame,
          rows,
        };
      },
    });

    expect(report.observations.every(({ dynamicResizeSafe }) => !dynamicResizeSafe))
      .toBe(true);
    expect(report.interactionPassed).toBe(false);
  });

  it("proves ordering, duplicate grouping and optional GitHub degradation", async () => {
    const manifest = parseUsabilityManifest(await manifestValue());
    const report = await evaluateUsabilityManifest(manifest, dependencies);
    const concurrent = report.observations.filter(
      ({ fixtureId }) => fixtureId === "concurrent-multi-run",
    );
    const degraded = report.observations.filter(
      ({ fixtureId }) => fixtureId === "gate-degraded-stale-conflict",
    );

    expect(concurrent.map(({ topAttentionId }) => topAttentionId)).toStrictEqual([
      "attention-critical-path",
      "attention-critical-path",
      "attention-critical-path",
    ]);
    expect(degraded.map(({ topAttentionId }) => topAttentionId)).toStrictEqual([
      "attention-safety-gate",
      "attention-safety-gate",
      "attention-safety-gate",
    ]);
    expect(degraded.every(({ optionalIntegrationIndependent }) => optionalIntegrationIndependent)).toBe(true);
    expect(degraded.every(({ dynamicResizeSafe, spec17ProjectionSafe, artifactReviewSafe }) => (
      dynamicResizeSafe && spec17ProjectionSafe && artifactReviewSafe
    ))).toBe(true);
    expect(report.observations.every(({ spec17ProjectionSafe }) =>
      spec17ProjectionSafe
    )).toBe(true);
    expect(manifest.fixtures.find(({ id }) => id === "gate-degraded-stale-conflict"))
      .toMatchObject({
        evidenceReview: {
          transformation: "terminal-neutralised",
          expectedDisposition: "confirm-terminal-neutralised",
        },
      });
    expect(report.observations
      .filter(({ fixtureId }) => fixtureId === "optional-notification-unavailable")
      .every(({ nativeNotificationVisible }) => nativeNotificationVisible)).toBe(true);
    expect(
      manifest.fixtures
        .flatMap(({ attention }) => attention)
        .some(({ duplicateCount }) => duplicateCount > 1),
    ).toBe(true);
  });

  it.each([
    ["review projection", "Review preparation", "current | synthetic"],
    ["route mismatch", "Actual endpoint identity", `proved | ${flowDigest}`],
    ["capability freshness", "Capability freshness", "available | synthetic"],
    ["topology", "Topology execution", "serial | synthetic"],
    ["context geometry", "Context tokens", "observed null"],
  ] as const)(
    "fails the Spec 17 %s geometry oracle when its exact detail is corrupted",
    async (_case, targetLabel, replacement) => {
      const manifest = parseUsabilityManifest(await manifestValue());
      let mutationCount = 0;
      const report = await evaluateUsabilityManifest(manifest, {
        ...dependencies,
        render: (dataset, controller, ui, viewport) => {
          const frame = renderFabricConsoleFrame(dataset, controller, ui, viewport);
          if (frame.presentation.detail === null) return frame;
          const lines = frame.presentation.detail.lines.map((line) => {
            if (line.label !== targetLabel) return line;
            mutationCount += 1;
            return { ...line, value: replacement };
          });
          return {
            ...frame,
            presentation: {
              ...frame.presentation,
              detail: { ...frame.presentation.detail, lines },
            },
          };
        },
      });

      expect(mutationCount).toBeGreaterThan(0);
      expect(report.observations.some(
        ({ spec17ProjectionSafe }) => !spec17ProjectionSafe,
      )).toBe(true);
      expect(report.interactionPassed).toBe(false);
    },
  );

  it("rejects vacuous or ambiguous manifests before evaluation", async () => {
    const value = (await manifestValue()) as Record<string, unknown>;
    expect(() =>
      parseUsabilityManifest({ ...value, repetitions: 2 }),
    ).toThrow(/repetitions/);
    expect(() =>
      parseUsabilityManifest({ ...value, minimumFieldSuccessRate: 0.5 }),
    ).toThrow(/minimumFieldSuccessRate/);

    const fixtures = structuredClone(value.fixtures) as Array<Record<string, unknown>>;
    if (fixtures[1] !== undefined) fixtures[1].id = fixtures[0]?.id;
    expect(() => parseUsabilityManifest({ ...value, fixtures })).toThrow(
      /fixture IDs must be unique/,
    );
  });

  it("fails recorded identification attempts that are late or wrong instead of inferring answers from rendered text", async () => {
    const manifest = parseUsabilityManifest(await manifestValue());
    const report = await evaluateUsabilityManifest(manifest, {
      ...dependencies,
      identify: async ({ fixture, repetition }) => ({
        observer: "automated-proxy" as const,
        durationMs: repetition === 1 ? manifest.maximumIdentificationMs + 1 : 500,
        topAttentionId: fixture.expectedTopAttentionId,
        answers: {
          ...fixture.expectedAnswers,
          owner: "wrong-owner",
        },
      }),
    });

    expect(report.passed).toBe(false);
    expect(report.fieldSuccessRate).toBeLessThan(1);
    expect(report.observations.some(
      ({ durationMs }) => durationMs > manifest.maximumIdentificationMs,
    )).toBe(true);
  });
});
