import { describe, expect, it } from "vitest";

import type {
  AgentId,
  OperatorActionPreview,
  ProjectId,
  ProjectSession,
  ProjectSessionId,
  ReviewEvidenceReadV1,
  RunProjection,
  Sha256Digest,
  Timestamp,
} from "@local/agent-fabric-protocol";
import {
  cellWidth,
  createFabricUiState,
  graphemes,
  renderFabricConsoleFrame,
  reduceFabricPointer,
  responsiveModeFor,
  writeFixedCells,
  type FabricPointerState,
} from "../src/index.js";
import type {
  ActionReview,
  ConsoleControllerState,
} from "../src/controller.js";
import {
  FABRIC_VIEWS,
  createEmptyViewPages,
  rankConsoleRows,
  revisionFromProtocol,
  type ConsoleRow,
  type FabricView,
} from "../src/model.js";
import { presentFabricConsole } from "../src/presenter.js";
import type { FabricConsoleDataset } from "../src/protocol-adapter.js";
import { activityGroupDetailLines } from "../src/activity-presentation.js";
import { renderConsoleSnapshot } from "../src/snapshot.js";
import type { ConsoleWorkflowReview } from "../src/workflow.js";

const timestamp = "2026-07-11T12:00:00.000Z" as Timestamp;
const digestA = (`sha256:${"a".repeat(64)}`) as Sha256Digest;
const digestB = (`sha256:${"b".repeat(64)}`) as Sha256Digest;
const projectId = "project-1" as ProjectId;
const sessionId = "session-1" as ProjectSessionId;

function cellAt(value: string, target: number): string | null {
  let offset = 1;
  for (const grapheme of graphemes(value)) {
    const width = cellWidth(grapheme);
    if (target >= offset && target < offset + width) return grapheme;
    offset += width;
  }
  return null;
}

function row(
  view: FabricView,
  stableId: string,
  summary: ConsoleRow["summary"],
  urgency: ConsoleRow["urgency"] = "normal",
  freshness: ConsoleRow["freshness"]["state"] = "live",
): ConsoleRow {
  return {
    view,
    stableId,
    revision: revisionFromProtocol(7),
    urgency,
    freshness:
      freshness === "unavailable"
        ? {
            state: "unavailable",
            source: "github",
            revision: revisionFromProtocol(7),
            observedAt: timestamp,
            ageMs: 5_000,
            reason: "adapter disabled",
          }
        : {
            state: freshness,
            source: "fabric",
            revision: revisionFromProtocol(7),
            observedAt: timestamp,
            ageMs: 5_000,
          },
    summary,
    detailRef:
      view === "attention"
        ? { kind: "system", componentId: stableId, expectedRevision: 7 }
        : null,
    actionAvailability:
      view === "attention" && freshness === "live"
        ? {
            state: "available",
            actions: ["resume"],
            requiresPreview: true,
          }
        : { state: "read-only", reason: "state-ineligible" },
  } as ConsoleRow;
}

function richDataset(
  snapshotRevision = 11,
  systemFreshness: ConsoleRow["freshness"]["state"] = "live",
): FabricConsoleDataset {
  const session: ProjectSession = {
    projectSessionId: sessionId,
    projectId,
    mode: "coordinated",
    state: "active",
    revision: 8,
    generation: 2,
    authorityRef: digestA,
    budgetRef: "budget-1",
    launchPacketRef: { path: "launch/packet.json" as never, digest: digestB },
    membershipRevision: 4,
    origin: { kind: "operator-launch", operatorId: "operator-1" as never },
  };
  const run: RunProjection = {
    runId: "AFAB-004" as never,
    phase: "implement",
    chairAgentId: "codex-chair" as AgentId,
    nextMilestone: "Console GREEN",
    health: "blocked",
  };
  const base = createEmptyViewPages();
  const rows: Record<FabricView, readonly ConsoleRow[]> = {
    attention: [
      row(
        "attention",
        "attention:safety",
        {
          kind: "attention",
          label: "Approval",
          priority: "safety-integrity",
          title: "Approve quarantine recovery",
          gateBinding: {
            gateId: "gate_quarantine_recovery" as never,
            gateRevision: 3,
            coordinationRunId: "AFAB-004" as never,
          },
          nativeNotification: {
            kind: "daemon-journal",
            targetIntegration: "native-desktop",
            status: "stale",
            journalState: "ambiguous",
            deliveryItemRevision: 7,
            claimGeneration: 3,
            integrationState: "available",
            observedAt: timestamp,
          },
        },
        "safety-integrity",
      ),
      row(
        "attention",
        "attention:fyi",
        {
          kind: "attention",
          label: "FYI",
          priority: "advisory",
          title: "Routine evaluation complete",
          nativeNotification: {
            kind: "daemon-journal",
            targetIntegration: "native-desktop",
            status: "unavailable",
            journalState: "missing",
            deliveryItemRevision: null,
            claimGeneration: null,
            integrationState: "absent",
            observedAt: timestamp,
          },
        },
        "advisory",
      ),
    ],
    project: [
      row("project", "project-1", {
        kind: "project",
        goal: "Ship the project Console",
        acceptedScopeRef: null,
        repositoryRevision: "c2fc623",
      }),
    ],
    runs: [
      row("runs", "AFAB-004", {
        kind: "run",
        projectSessionId: "session-1" as never,
        phase: "implement",
        health: "blocked",
        nextMilestone: "Console GREEN",
        declaredProgress: { plan: "open", counts: { blocked: 0, ready: 0, active: 1, complete: 0, cancelled: 0, degraded: 0 } },
        identity: {
          runKind: "coordination",
          chairAgentId: "agent-chair" as never,
          acceptedScopeRef: null,
          currentPlanRef: null,
          planRevision: null,
          workstreams: [],
          lastEventAt: timestamp,
        },
      }),
    ],
    work: [
      row("work", "task-1", {
        kind: "work",
        state: "active",
        checkState: "passing",
      }),
    ],
    agents: [
      row("agents", "codex-chair", {
        kind: "agent",
        role: "chair",
        lifecycle: "working",
        contextPressure: "medium",
      }),
    ],
    evidence: [
      row("evidence", "evidence-1", {
        kind: "evidence",
        evidenceKind: "test",
        status: "pass",
        provenance: "native harness",
      }),
    ],
    activity: [
      row("activity", "activity-group-1", {
        kind: "activity",
        summary: "Review decision context",
        occurredAt: timestamp,
        group: {
          groupId: "activity-group-1",
          ordinal: 1,
          kind: "task",
          actorIds: ["codex-chair"],
          target: { kind: "task", id: "task-1" },
          eventKinds: ["message-persisted", "tool-invoked", "gate-resolved"],
          occurredAtRange: { first: timestamp, last: timestamp },
          sourceRange: { first: 11, last: 13 },
          count: 3,
          evidenceLinkCount: 0,
          evidenceLinksDigest:
            "sha256:4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945" as never,
          evidenceLinksTruncated: false,
          evidenceLinks: [],
          members: [
            {
              ordinal: 1,
              eventId: "event-1",
              eventKind: "message-persisted",
              actorId: "codex-chair",
              target: { kind: "task", id: "task-1" },
              occurredAt: timestamp,
              sourceRevision: 11,
              detailAvailability: "available",
              evidenceLinkCount: 0,
              evidenceLinksDigest:
                "sha256:4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945" as never,
            },
            {
              ordinal: 2,
              eventId: "event-2",
              eventKind: "tool-invoked",
              actorId: "codex-chair",
              target: { kind: "task", id: "task-1" },
              occurredAt: timestamp,
              sourceRevision: 12,
              detailAvailability: "available",
              evidenceLinkCount: 0,
              evidenceLinksDigest:
                "sha256:4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945" as never,
            },
            {
              ordinal: 3,
              eventId: "event-3",
              eventKind: "gate-resolved",
              actorId: "codex-chair",
              target: { kind: "task", id: "task-1" },
              occurredAt: timestamp,
              sourceRevision: 13,
              detailAvailability: "available",
              evidenceLinkCount: 0,
              evidenceLinksDigest:
                "sha256:4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945" as never,
            },
          ],
        },
      }),
    ],
    system: [
      row(
        "system",
        "github",
        systemFreshness === "unavailable"
          ? null
          : {
              kind: "system",
              systemKind: "integration",
              state: "healthy",
              detail: "optional GitHub adapter",
            },
        "normal",
        systemFreshness,
      ),
    ],
  };
  const pages = Object.fromEntries(
    FABRIC_VIEWS.map((view) => [
      view,
      {
        ...base[view],
        rows: rows[view],
        snapshotRevision: revisionFromProtocol(snapshotRevision),
        readTransactionId: `read-${view}`,
      },
    ]),
  ) as never;
  return {
    connection: { state: "live", compatibility: { mode: "current" } },
    snapshot: {
      schemaVersion: 1,
      snapshotRevision,
      readTransactionId: `snapshot-${String(snapshotRevision)}`,
      project: {
        freshness: "live",
        source: "fabric",
        revision: snapshotRevision,
        observedAt: timestamp,
        value: { projectId, canonicalRoot: "/workspace/project" },
      },
      session: {
        freshness: "live",
        source: "fabric",
        revision: snapshotRevision,
        observedAt: timestamp,
        value: session,
      },
      runs: {
        freshness: "live",
        source: "fabric",
        revision: snapshotRevision,
        observedAt: timestamp,
        value: [run],
      },
      attention: {
        freshness: "live",
        source: "fabric",
        revision: snapshotRevision,
        observedAt: timestamp,
        value: [],
      },
      capacity: {
        freshness: "live",
        source: "fabric",
        revision: snapshotRevision,
        observedAt: timestamp,
        value: { tasks: { used: 3, reserved: 1, limit: 8 } },
      },
      cursor: snapshotRevision,
      stateDigest: digestA,
    },
    snapshotRevision: revisionFromProtocol(snapshotRevision),
    cursor: snapshotRevision,
    pages,
    loadedAtMs: Date.parse(timestamp),
    canMutate: true,
  };
}

function attentionDeckDataset(crossedAdvisory = false): FabricConsoleDataset {
  const base = richDataset();
  const advisory = base.pages.attention.rows[1];
  const run = base.pages.runs.rows[0];
  if (
    advisory?.summary?.kind !== "attention" ||
    run?.summary?.kind !== "run"
  ) {
    throw new Error("Deck fixture unavailable");
  }
  return {
    ...base,
    projectSessions: {
      selectedProjectSessionId: null,
      choices: [{
        projectSessionId: sessionId,
        mode: "coordinated",
        state: "active",
        revision: 8,
        generation: 2,
        lastEventAt: timestamp,
      }],
    },
    pages: {
      ...base.pages,
      attention: crossedAdvisory
        ? {
            ...base.pages.attention,
            rows: [
              base.pages.attention.rows[0] as ConsoleRow<"attention">,
              {
                ...advisory,
                urgency: "critical-path",
                summary: { ...advisory.summary, priority: "critical-path" },
              },
            ],
          }
        : base.pages.attention,
      runs: {
        ...base.pages.runs,
        rows: [{
          ...run,
          summary: {
            ...run.summary,
            identity: {
              ...run.summary.identity,
              workstreams: [{
                workstreamId: "workstream-1" as never,
                deliveryRunId: "delivery-1" as never,
                leadAgentId: "delivery-lead" as AgentId,
                state: "active",
                updatedAt: timestamp,
              }],
            },
          },
        }],
      },
    },
  };
}

function acceptance46Dataset(): FabricConsoleDataset {
  const base = attentionDeckDataset(true);
  const required = base.pages.attention.rows[0];
  const fyi = base.pages.attention.rows[1];
  if (
    required?.summary?.kind !== "attention" ||
    fyi?.summary?.kind !== "attention"
  ) throw new Error("acceptance 46 attention fixture unavailable");
  const requiredSummary = required.summary;
  const fyiSummary = fyi.summary;
  const advisorySignals = [
    "Inactive for three hours",
    "High message volume",
    "Context pressure high",
    "Provider pane absent",
    "Optional integration outage",
  ];
  const advisoryRows: ConsoleRow<"attention">[] = advisorySignals.map((title, index) => ({
    ...fyi,
    stableId: `attention:advisory-signal-${String(index)}`,
    urgency: "advisory",
    summary: {
      ...fyiSummary,
      label: "Blocked",
      priority: "advisory",
      title,
    },
  }));
  const requiredRows: ConsoleRow<"attention">[] = [
    required,
    {
      ...required,
      stableId: "attention:critical-path",
      urgency: "critical-path",
      summary: {
        ...requiredSummary,
        label: "Blocked",
        priority: "critical-path",
        title: "Resolve the critical-path blocker",
      },
    },
    {
      ...required,
      stableId: "attention:acceptance-ready",
      urgency: "acceptance-ready",
      summary: {
        ...requiredSummary,
        label: "Approval",
        priority: "acceptance-ready",
        title: "Accept the reviewed delivery",
      },
    },
  ];
  return {
    ...base,
    pages: {
      ...base.pages,
      attention: {
        ...base.pages.attention,
        rows: rankConsoleRows([...advisoryRows, ...requiredRows.toReversed(), fyi]),
      },
    },
  };
}

function urgencyGlyphDataset(): FabricConsoleDataset {
  const base = attentionDeckDataset();
  const attention = base.pages.attention.rows[0];
  const run = base.pages.runs.rows[0];
  if (attention?.summary?.kind !== "attention" || run?.summary?.kind !== "run") {
    throw new Error("urgency glyph fixture unavailable");
  }
  const attentionSummary = attention.summary;
  const runSummary = run.summary;
  const attentionStates = [
    ["safety", "safety-integrity", "Safety gate"],
    ["critical", "critical-path", "Critical blocker"],
    ["expiring", "expiring-authority", "Authority expires"],
    ["acceptance", "acceptance-ready", "Acceptance ready"],
  ] as const;
  const attentionRows: ConsoleRow<"attention">[] = attentionStates.map(([id, urgency, title]) => ({
    ...attention,
    stableId: `attention:${id}`,
    urgency,
    summary: { ...attentionSummary, priority: urgency, title },
  }));
  const staleAttention = {
    ...attentionRows[0]!,
    stableId: "attention:stale",
    freshness: {
      state: "stale" as const,
      source: "fabric" as const,
      revision: attention.revision,
      observedAt: timestamp,
      ageMs: 60_000,
    },
    summary: { ...attentionSummary, title: "Stale safety gate" },
  } satisfies ConsoleRow<"attention">;
  const runRows: ConsoleRow<"runs">[] = ([
    ["run-healthy", "healthy", "session-healthy", "build"],
    ["run-degraded", "degraded", "session-degraded", "review"],
    ["run-stale", "healthy", "session-stale", "verify"],
  ] as const).map(([stableId, health, projectSessionId, phase]) => ({
    ...run,
    stableId,
    freshness: stableId === "run-stale"
      ? {
          state: "stale" as const,
          source: "fabric" as const,
          revision: run.revision,
          observedAt: timestamp,
          ageMs: 60_000,
        }
      : run.freshness,
    summary: {
      ...runSummary,
      projectSessionId: projectSessionId as ProjectSessionId,
      phase,
      health: health as RunProjection["health"],
      identity: { ...runSummary.identity, workstreams: [] },
    },
  }));
  return {
    ...base,
    projectSessions: {
      selectedProjectSessionId: null,
      choices: [
        {
          projectSessionId: "session-healthy" as ProjectSessionId,
          mode: "coordinated",
          state: "active",
          revision: 1,
          generation: 1,
          lastEventAt: timestamp,
        },
        {
          projectSessionId: "session-degraded" as ProjectSessionId,
          mode: "coordinated",
          state: "visibility_degraded",
          revision: 1,
          generation: 1,
          lastEventAt: timestamp,
        },
        {
          projectSessionId: "session-stale" as ProjectSessionId,
          mode: "coordinated",
          state: "recovery_required",
          revision: 1,
          generation: 1,
          lastEventAt: timestamp,
        },
      ],
    },
    pages: {
      ...base.pages,
      attention: {
        ...base.pages.attention,
        rows: [...attentionRows, staleAttention],
      },
      runs: { ...base.pages.runs, rows: runRows },
    },
  };
}

function datasetWithHeader(
  overrides: Readonly<{
    project?: ProjectId;
    session?: ProjectSessionId;
    run?: RunProjection["runId"];
    phase?: string;
    owner?: AgentId;
    nextMilestone?: string;
  }>,
): FabricConsoleDataset {
  const dataset = richDataset();
  const snapshot = dataset.snapshot;
  if (
    snapshot === null ||
    !("value" in snapshot.project) ||
    !("value" in snapshot.session) ||
    snapshot.session.value === null ||
    !("value" in snapshot.runs)
  ) {
    throw new Error("live header fixture unavailable");
  }
  const run = snapshot.runs.value[0];
  if (run === undefined) throw new Error("run header fixture unavailable");
  return {
    ...dataset,
    snapshotRevision: revisionFromProtocol(Number.MAX_SAFE_INTEGER),
    snapshot: {
      ...snapshot,
      snapshotRevision: Number.MAX_SAFE_INTEGER,
      project: {
        ...snapshot.project,
        value: {
          ...snapshot.project.value,
          projectId: overrides.project ?? snapshot.project.value.projectId,
        },
      },
      session: {
        ...snapshot.session,
        value: {
          ...snapshot.session.value,
          projectSessionId:
            overrides.session ?? snapshot.session.value.projectSessionId,
        },
      },
      runs: {
        ...snapshot.runs,
        value: [{
          ...run,
          runId: overrides.run ?? run.runId,
          phase: overrides.phase ?? run.phase,
          chairAgentId: overrides.owner ?? run.chairAgentId,
          nextMilestone: overrides.nextMilestone ?? run.nextMilestone,
        }],
      },
    },
  };
}

function controllerState(review: ActionReview | null = null): ConsoleControllerState {
  const selectionByView = Object.fromEntries(
    FABRIC_VIEWS.map((view) => [view, null]),
  ) as Record<FabricView, null | { stableId: string; revision: ReturnType<typeof revisionFromProtocol> }>;
  selectionByView.attention = {
    stableId: "attention:safety",
    revision: revisionFromProtocol(7),
  };
  return {
    activeView: "attention",
    selectionByView,
    scrollAnchorByView: Object.fromEntries(
      FABRIC_VIEWS.map((view) => [view, null]),
    ) as never,
    review,
    pendingCommandIds: [],
    lastActionStatus: null,
    lastReceipt: null,
  };
}

function controllableRunDataset(snapshotRevision = 11): FabricConsoleDataset {
  const dataset = richDataset(snapshotRevision);
  const run = dataset.pages.runs.rows[0];
  if (run === undefined) throw new Error("run fixture unavailable");
  return {
    ...dataset,
    pages: {
      ...dataset.pages,
      runs: {
        ...dataset.pages.runs,
        rows: [{
          ...run,
          detailRef: {
            kind: "run",
            projectSessionId: sessionId,
            coordinationRunId: "AFAB-004" as never,
            expectedRevision: 7,
          },
          actionAvailability: {
            state: "available",
            actions: ["resume"],
            requiresPreview: true,
          },
        }],
      },
    },
  };
}

function closedProjectionDataset(): FabricConsoleDataset {
  return {
    ...richDataset(),
    review: {
      reviewRuns: [{
        projectSessionId: sessionId,
        coordinationRunId: "AFAB-004",
        preparation: {
          state: "unavailable",
          reason: "preparation-id-not-projected",
          code: null,
        },
        completion: {
          state: "current",
          value: {
            schemaVersion: 1,
            blockers: [],
            targetGeneration: 4,
            targetChair: null,
            reviewedArtifactRef: "artifact-4",
            publicationLineageDigest: digestA,
            bundleDigest: digestB,
            manifestRootDigest: digestA,
            coverageDigest: digestB,
            riskReadMapDigest: digestA,
            mandatoryReadSetDigest: digestB,
            profileDigest: digestA,
            unavailableSlots: [],
            slots: [{
              slot: "native",
              headGeneration: 2,
              attemptGeneration: 1,
              actionRef: { adapterId: "adapter-native", actionId: "action-native" },
              evidenceId: "evidence-1",
              terminalKind: "safe-answer",
              verdict: "CLEAN",
              resultDigest: digestA,
              providerFailureCode: null,
              providerFailureDigest: null,
              routeReceiptDigest: digestB,
              adapterId: "adapter-native",
              endpointProvider: "openai",
              providerFamily: "gpt",
              model: "gpt-5.4",
              routeObservationDigest: digestA,
              actualRouteIdentityDigest: digestB,
              readCoverageDigest: digestA,
              reviewerFamilyRelation: "same-family-exempt",
              currentCertificationBasis: null,
              certifying: true,
              openFindingSet: { findingSetDigest: digestA, findingCount: 0, pageDigests: [] },
              blockers: [],
            }],
            finalReviewComplete: false,
          },
        },
        evidence: {
          state: "current",
          value: [{
            schemaVersion: 1,
            record: {
              evidenceId: "evidence-1",
              targetGeneration: 4,
              slot: "native",
              actionRef: { adapterId: "adapter-native", actionId: "action-native" },
              endpointProvider: "openai",
              providerFamily: "gpt",
              model: "gpt-5.4",
              routeReceiptDigest: digestB,
              routeObservationDigest: digestA,
              actualRouteIdentityDigest: digestB,
            },
            currency: {
              target: "current",
              source: "current",
              chair: "current",
              profile: "current",
              certifying: true,
              blockerCodes: [],
            },
            annotation: null,
          }],
        },
        recoveries: [],
        providerRoute: {
          state: "unavailable",
          reason: "operator-route-projection-unavailable",
          code: null,
        },
        capabilityFreshness: {
          state: "unavailable",
          reason: "operator-route-projection-unavailable",
          code: null,
        },
      }],
      topology: [{
        taskId: "task-1",
        coordinationRunId: "AFAB-004",
        read: {
          state: "current",
          value: {
            schemaVersion: 1,
            currency: "stale",
            pointer: { revision: 8 },
            plan: {
              waveId: "wave-7",
              waveRevision: 3,
              state: "started",
              predecessor: null,
              dependencies: [],
              decomposability: { kind: "decomposable", evidenceRef: "evidence-topology" },
              topology: { executionShape: "fabric-explicit", mode: "parallel", maximumConcurrentAgents: 3 },
              chair: { agentId: "codex-chair", principalGeneration: 2, chairLeaseGeneration: 4 },
              stageOwners: [{ stageId: "implementation", taskId: "task-1", ownerAgentId: "worker-1", writePartitionId: "partition-1" }],
              writePartitions: [{ partitionId: "partition-1", ownerAgentId: "worker-1", mode: "exclusive-write", pathSetDigest: digestA, authorityRef: "authority-1" }],
              contention: { mode: "disjoint-partitions", serializationOwnerAgentId: null, evidenceRef: "evidence-contention" },
              budget: { providerTurns: 12, toolCalls: 40, wallClockSeconds: 900, maximumParallelAgents: 3 },
              stopConditions: [{ conditionId: "stop-complete", kind: "objective-complete", predicateRef: "predicate-1" }],
              authority: { authorityRevision: 5, authorityRef: "authority-1", authorityDigest: digestA },
              policy: { policyRevision: 6, policyRef: "policy-1", policyDigest: digestB },
              rationaleRef: "rationale-evidence-1",
              planDigest: digestA,
            },
          },
        },
      }],
      contextPressure: [{
        agentId: "codex-chair",
        coordinationRunId: "AFAB-004",
        read: {
          state: "current",
          value: {
            schemaVersion: 1,
            currency: "current",
            readAt: timestamp,
            ageSeconds: 5,
            pressure: {
              pressure: "high",
              source: "native-exact",
              confidence: "exact",
              windowTokens: 100_000,
              usedTokens: 81_000,
              remainingTokens: 19_000,
              observedAt: timestamp,
              expiresAt: "2026-07-11T12:05:00.000Z",
              providerGeneration: 3,
              contextRevision: 9,
              revision: 4,
              evidenceDigest: digestB,
            },
          },
        },
      }],
    },
  } as unknown as FabricConsoleDataset;
}

function runControllerState(): ConsoleControllerState {
  const state = controllerState();
  return {
    ...state,
    activeView: "runs",
    selectionByView: {
      ...state.selectionByView,
      runs: { stableId: "AFAB-004", revision: revisionFromProtocol(7) },
    },
  };
}

function review(stage: ActionReview["stage"] = "review"): ActionReview {
  const actionPreview: OperatorActionPreview = {
    previewId: "preview-1",
    previewRevision: 3,
    previewDigest: digestA,
    intent: {
      kind: "control",
      action: "resume",
      target: {
        kind: "task",
        projectSessionId: sessionId,
        coordinationRunId: "AFAB-004" as never,
        taskId: "task-1" as never,
        expectedRevision: 7,
      },
    },
    intentDigest: digestB,
    beforeStateDigest: digestA,
    consequenceClass: "consequential",
    evidenceRefs: [{ path: "evidence/test.json" as never, digest: digestB }],
    gateIds: ["gate-1" as never],
    confirmationMode: "explicit",
    expiresAt: "2099-07-11T13:00:00.000Z" as Timestamp,
  };
  return {
    stage,
    binding: {
      view: "attention",
      itemId: "attention:safety",
      itemRevision: revisionFromProtocol(7),
      projectionRevision: revisionFromProtocol(11),
    },
    availableAction: "resume",
    preview: actionPreview,
    gates: [
      {
        gateId: "gate-1" as never,
        stateDigest: digestA,
        readTransactionId: "gate-read-1",
        changedFromRevision: null,
        gate: {
          gateId: "gate-1" as never,
          projectSessionId: sessionId,
          coordinationRunId: "AFAB-004" as never,
          scope: { kind: "task", taskId: "task-1" as never },
          affectedTaskIds: ["task-1" as never],
          dependencyRevision: 6,
          blockedOperationIds: [],
          enforcementPoints: ["task-readiness"],
          question: "Resume quarantined task?",
          reason: "Replacement evidence passed.",
          options: ["approve", "reject"],
          recommendation: "approve",
          consequences: ["Task execution may continue."],
          evidenceRefs: [{ path: "evidence/test.json" as never, digest: digestB }],
          revision: 7,
          createdByRef: "chair-1",
          expectedApproverRef: "operator-1",
          status: "pending",
        },
      },
    ],
    openedByEventId: "event-open",
    armedByEventId: stage === "confirm" ? "event-arm" : null,
    changes: [],
    status: null,
  };
}

describe("structured presenter and responsive Fabric renderer", () => {
  it("renders daemon grouping count and source range without adding a lifecycle claim", () => {
    const dataset = richDataset();
    const activity = dataset.pages.activity.rows[0];
    if (
      activity?.summary?.kind !== "activity" ||
      !("group" in activity.summary)
    ) {
      throw new Error("activity grouping fixture unavailable");
    }
    const base = controllerState();
    const controller: ConsoleControllerState = {
      ...base,
      activeView: "activity",
      selectionByView: {
        ...base.selectionByView,
        activity: {
          stableId: activity.stableId,
          revision: activity.revision,
        },
      },
    };
    const presented = presentFabricConsole(
      dataset,
      controller,
      createFabricUiState(),
      { columns: 120, rows: 32 },
    );

    expect(presented.masterRows[0]).toMatchObject({
      primary: "Review decision context",
      secondary: "task | x3 | source 11-13 | task:task-1",
    });
    expect(presented.detail?.lines).toEqual(expect.arrayContaining([
      { label: "Group count", value: "3" },
      { label: "Group source range", value: "11-13 inclusive" },
      {
        label: "Member 2",
        value: `event-2 | tool-invoked | source 12 | ${timestamp}`,
      },
      {
        label: "Member 2 detail",
        value: "available | 0 evidence link(s)",
      },
    ]));
    expect(
      JSON.stringify({
        primary: presented.masterRows[0]?.primary,
        secondary: presented.masterRows[0]?.secondary,
        detail: presented.detail?.lines,
      }),
    ).not.toMatch(/\b(?:completed|done|progress|succeeded)\b/iu);
    const unsafeEvidenceSummary = {
      ...activity.summary,
      group: {
        ...activity.summary.group,
        evidenceLinkCount: 2,
        evidenceLinksTruncated: true,
        evidenceLinks: [{ path: "evidence/a.txt" as never, digest: digestA }],
        members: activity.summary.group.members.map((member, index) =>
          index === 0
            ? {
                ...member,
                detailAvailability: "unavailable" as const,
                evidenceLinkCount: 2,
              }
            : member
        ) as unknown as typeof activity.summary.group.members,
      },
    };
    expect(
      activityGroupDetailLines(unsafeEvidenceSummary).find(
        (line) => line.label === "Group evidence",
      )?.value,
    ).toContain(
      "some full links unavailable | evidence-bearing member detail unavailable",
    );
  });

  it("renders grouped row and drill-down controls as visible inert tokens", () => {
    const baseDataset = richDataset();
    const baseActivity = baseDataset.pages.activity.rows[0];
    if (
      baseActivity?.summary?.kind !== "activity" ||
      !("group" in baseActivity.summary) ||
      baseDataset.snapshotRevision === null
    ) {
      throw new Error("activity grouping fixture unavailable");
    }
    const member = {
      ...baseActivity.summary.group.members[0],
      eventKind: "message\u2066-persisted",
      messageBodyRef: {
        projectSessionId: sessionId,
        messageId: "message-hostile" as never,
        expectedRevision: 1,
      },
    };
    const group = {
      ...baseActivity.summary.group,
      actorIds: ["codex\u202e-chair"],
      eventKinds: [member.eventKind],
      count: 1,
      sourceRange: { first: 11, last: 11 },
      members: [member] as [typeof member],
    };
    const activity = {
      ...baseActivity,
      summary: {
        ...baseActivity.summary,
        summary: "Review\u001b[31m context",
        group,
      },
      detailRef: {
        kind: "activity" as const,
        groupId: group.groupId,
        expectedRevision: 11,
      },
    };
    const dataset: FabricConsoleDataset = {
      ...baseDataset,
      pages: {
        ...baseDataset.pages,
        activity: {
          ...baseDataset.pages.activity,
          rows: [activity],
        },
      },
      inspection: {
        kind: "activity",
        state: "current",
        binding: {
          view: "activity",
          itemId: activity.stableId,
          itemRevision: activity.revision,
          projectionRevision: baseDataset.snapshotRevision,
        },
        readTransactionId: "read-hostile-group",
        detail: {
          kind: "activity",
          group,
          memberDetails: [{
            eventId: member.eventId,
            status: "available",
            content: "tool detail\u202e",
            transformation: "none",
          }],
        },
        messages: [{
          eventId: member.eventId,
          state: "current",
          result: {
            available: true,
            messageId: "message-hostile" as never,
            revision: 1,
            body: "ordinary body\u0007",
            terminalNeutralised: true,
            capabilityValuesRedacted: true,
            artifactRefs: [],
          },
        }],
      },
    };
    const base = controllerState();
    const frame = renderFabricConsoleFrame(
      dataset,
      {
        ...base,
        activeView: "activity",
        selectionByView: {
          ...base.selectionByView,
          activity: {
            stableId: activity.stableId,
            revision: activity.revision,
          },
        },
      },
      createFabricUiState(),
      { columns: 140, rows: 36 },
    );
    const output = frame.rows.join("\n");

    expect(output).not.toContain("\u001b");
    expect(output).not.toContain("\u202e");
    expect(output).not.toContain("\u2066");
    expect(output).not.toContain("\u0007");
    expect(output).toContain("<ESC>");
    expect(output).toContain("<BIDI-U+202E>");
    expect(output).toContain("<BIDI-U+2066>");
    expect(output).toContain("<BEL>");
  });

  it("shows the exact registered accepted scope in Project row and detail", () => {
    const dataset = richDataset();
    const projectRow = dataset.pages.project.rows[0];
    if (projectRow === undefined || projectRow.summary?.kind !== "project") {
      throw new Error("project fixture unavailable");
    }
    const acceptedScopeRef = {
      path: "docs/specs/console/acceptance.md" as never,
      digest: digestB,
    };
    const scopedDataset: FabricConsoleDataset = {
      ...dataset,
      pages: {
        ...dataset.pages,
        project: {
          ...dataset.pages.project,
          rows: [{
            ...projectRow,
            summary: { ...projectRow.summary, acceptedScopeRef },
          }],
        },
      },
    };
    const baseController = controllerState();
    const controller: ConsoleControllerState = {
      ...baseController,
      activeView: "project",
      selectionByView: {
        ...baseController.selectionByView,
        project: {
          stableId: projectRow.stableId,
          revision: projectRow.revision,
        },
      },
    };
    const presented = presentFabricConsole(
      scopedDataset,
      controller,
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );
    expect(presented.masterRows[0]?.secondary).toContain(
      `${acceptedScopeRef.path}@${acceptedScopeRef.digest}`,
    );
    expect(presented.detail?.lines).toContainEqual({
      label: "Accepted scope",
      value: `${acceptedScopeRef.path}@${acceptedScopeRef.digest}`,
    });
  });

  it("wraps ordinary detail facts so long canonical identities remain revealable", () => {
    const dataset = richDataset();
    const projectRow = dataset.pages.project.rows[0];
    if (projectRow?.summary?.kind !== "project") {
      throw new Error("project fixture unavailable");
    }
    const longPath = `${"deep-segment/".repeat(18)}scope-tail.json` as never;
    const projected: FabricConsoleDataset = {
      ...dataset,
      pages: {
        ...dataset.pages,
        project: {
          ...dataset.pages.project,
          rows: [{
            ...projectRow,
            summary: {
              ...projectRow.summary,
              acceptedScopeRef: { path: longPath, digest: digestB },
            },
          }],
        },
      },
    };
    const base = controllerState();
    const state: ConsoleControllerState = {
      ...base,
      activeView: "project",
      selectionByView: {
        ...base.selectionByView,
        project: { stableId: projectRow.stableId, revision: projectRow.revision },
      },
    };
    const frame = renderFabricConsoleFrame(
      projected,
      state,
      createFabricUiState({ focusId: `detail:project:${projectRow.stableId}` }),
      { columns: 120, rows: 36 },
    );

    const revealed = frame.rows.join("").replaceAll(" ", "").replaceAll("|", "");
    expect(revealed).toContain("scope-tail.json");
    expect(revealed).toContain("b".repeat(32));
  });

  it("presents closed review, actual-route, topology, and context projections without legacy substitutes", () => {
    const dataset = closedProjectionDataset();
    const expectedByView = {
      runs: [
        ["Review preparation", "unavailable | preparation-id-not-projected"],
        ["Review target generation", "4"],
        ["Review completion", "INCOMPLETE"],
        ["Review slot native", "CLEAN | certifying"],
        ["Provider route", "unavailable | operator-route-projection-unavailable"],
        ["Capability freshness", "unavailable | operator-route-projection-unavailable"],
      ],
      work: [
        ["Topology currency", "STALE"],
        ["Topology wave", "wave-7@r3 | started"],
        ["Topology rationale", "rationale-evidence-1"],
        ["Topology execution", "fabric-explicit | parallel | max 3"],
      ],
      agents: [
        ["Context pressure", "HIGH | CURRENT | age 5s"],
        ["Context source", "native-exact | exact"],
        ["Context tokens", "window 100000 | used 81000 | remaining 19000"],
      ],
      evidence: [
        ["Admitted review route", "openai | gpt | gpt-5.4"],
        ["Actual endpoint identity", `proved | ${digestB}`],
        ["Review currency", "target current | source current | chair current | profile current"],
      ],
    } as const;

    for (const [view, expected] of Object.entries(expectedByView) as readonly [
      "runs" | "work" | "agents" | "evidence",
      readonly (readonly [string, string])[],
    ][]) {
      const base = controllerState();
      const stableId = dataset.pages[view].rows[0]?.stableId;
      if (stableId === undefined) throw new Error(`${view} fixture unavailable`);
      const state: ConsoleControllerState = {
        ...base,
        activeView: view,
        selectionByView: {
          ...base.selectionByView,
          [view]: { stableId, revision: revisionFromProtocol(7) },
        },
      };
      const lines = presentFabricConsole(
        dataset,
        state,
        createFabricUiState(),
        { columns: 120, rows: 36 },
      ).detail?.lines;
      expect(lines).toEqual(expect.arrayContaining(
        expected.map(([label, value]) => ({ label, value })),
      ));
    }
  });

  it("binds review evidence detail to the exact run and evidence ID pair", () => {
    const current = closedProjectionDataset();
    const exact = current.review?.reviewRuns[0];
    if (exact === undefined || exact.evidence.state !== "current") {
      throw new Error("closed review fixture unavailable");
    }
    const exactEvidence = exact.evidence.value as unknown as readonly ReviewEvidenceReadV1[];
    const crossed = {
      ...exact,
      coordinationRunId: "AFAB-WRONG",
      evidence: {
        ...exact.evidence,
        value: exactEvidence.map((entry) => ({
          ...entry,
          record: {
            ...(entry.record as Readonly<Record<string, unknown>>),
            endpointProvider: "crossed-provider",
            providerFamily: "crossed-family",
            model: "crossed-model",
          },
        })),
      },
    };
    const dataset = {
      ...current,
      inspection: {
        kind: "artifact",
        state: "current",
        binding: {
          view: "evidence",
          itemId: "evidence-1",
          itemRevision: revisionFromProtocol(7),
          projectionRevision: revisionFromProtocol(11),
        },
        result: { coordinationRunId: "AFAB-004" },
      },
      review: {
        ...current.review,
        reviewRuns: [crossed, exact],
      },
    } as unknown as FabricConsoleDataset;
    const base = controllerState();
    const state: ConsoleControllerState = {
      ...base,
      activeView: "evidence",
      selectionByView: {
        ...base.selectionByView,
        evidence: {
          stableId: "evidence-1",
          revision: revisionFromProtocol(7),
        },
      },
    };

    const detail = presentFabricConsole(
      dataset,
      state,
      createFabricUiState(),
      { columns: 80, rows: 24 },
    ).detail?.lines ?? [];

    expect(detail).toContainEqual({
      label: "Admitted review route",
      value: "openai | gpt | gpt-5.4",
    });
    expect(JSON.stringify(detail)).not.toContain("crossed-provider");

    const ambiguous = presentFabricConsole(
      {
        ...dataset,
        inspection: undefined,
      } as unknown as FabricConsoleDataset,
      state,
      createFabricUiState(),
      { columns: 80, rows: 24 },
    ).detail?.lines ?? [];
    expect(ambiguous).toContainEqual({
      label: "Review evidence",
      value: "unavailable | coordination-run-binding-unavailable",
    });
    expect(JSON.stringify(ambiguous)).not.toContain("crossed-provider");
  });

  it("renders an actual-route mismatch ahead of a contradictory non-null digest", () => {
    const current = closedProjectionDataset();
    const run = current.review?.reviewRuns[0];
    if (run === undefined || run.evidence.state !== "current") {
      throw new Error("closed review fixture unavailable");
    }
    const runEvidence = run.evidence.value as unknown as readonly ReviewEvidenceReadV1[];
    const dataset = {
      ...current,
      review: {
        ...current.review,
        reviewRuns: [{
          ...run,
          evidence: {
            ...run.evidence,
            value: runEvidence.map((entry) => ({
              ...entry,
              currency: {
                ...(entry.currency as Readonly<Record<string, unknown>>),
                certifying: false,
                blockerCodes: ["actual-route-mismatch"],
              },
            })),
          },
        }],
      },
    } as unknown as FabricConsoleDataset;
    const base = controllerState();
    const state: ConsoleControllerState = {
      ...base,
      activeView: "evidence",
      selectionByView: {
        ...base.selectionByView,
        evidence: {
          stableId: "evidence-1",
          revision: revisionFromProtocol(7),
        },
      },
    };
    const detail = presentFabricConsole(
      dataset,
      state,
      createFabricUiState(),
      { columns: 80, rows: 24 },
    ).detail?.lines ?? [];

    expect(detail).toContainEqual({
      label: "Actual endpoint identity",
      value: "Unknown | actual-route-mismatch",
    });
    expect(detail.find(({ label }) => label === "Actual endpoint identity")?.value)
      .not.toContain("proved");
  });

  it("requires an exact explicit terminal-neutralisation confirmation before evidence actions", () => {
    const dataset = richDataset();
    const evidenceRow = dataset.pages.evidence.rows[0];
    if (evidenceRow === undefined) throw new Error("evidence fixture unavailable");
    const actionableRow: ConsoleRow<"evidence"> = {
      ...evidenceRow,
      view: "evidence",
      actionAvailability: {
        state: "available",
        actions: ["promotion"],
        requiresPreview: true,
      },
    };
    const reviewed: FabricConsoleDataset = {
      ...dataset,
      pages: {
        ...dataset.pages,
        evidence: { ...dataset.pages.evidence, rows: [actionableRow] },
      },
      inspection: {
        kind: "artifact",
        state: "current",
        binding: {
          view: "evidence",
          itemId: actionableRow.stableId,
          itemRevision: actionableRow.revision,
          projectionRevision: revisionFromProtocol(11),
        },
        readTransactionId: "artifact-review",
        result: {
          artifactRef: { path: "docs/spec.md" as never, digest: digestA },
          evidenceRevision: 7,
          evidenceKind: "artifact",
          sourceKind: "project-file",
          publisherKind: "agent",
          publisherRef: "chair-1",
          projectSessionId: sessionId,
          coordinationRunId: "run-1" as never,
          taskId: null,
          createdAt: timestamp,
          mediaType: "text/markdown",
          content: "reviewed",
          totalBytes: 12,
          totalLines: 1,
          renderedTotalBytes: 8,
          renderedTotalLines: 1,
          renderedArtifactDigest: digestB,
          transformation: "terminal-neutralised",
          terminalNeutralised: true,
          capabilityValuesRedacted: true,
          credentialValuesRedacted: true,
          pages: [{ pageIndex: 0, lineFragment: "whole", pageContentDigest: digestB, bytes: 8 }],
          coverage: { complete: true, verified: true, pageCount: 1 },
          reviewDisposition: "confirm-terminal-neutralised",
        },
      },
    };
    const baseController = controllerState();
    const controller: ConsoleControllerState = {
      ...baseController,
      activeView: "evidence",
      selectionByView: {
        ...baseController.selectionByView,
        evidence: {
          stableId: actionableRow.stableId,
          revision: actionableRow.revision,
        },
      },
    };
    const pending = presentFabricConsole(
      reviewed,
      controller,
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );
    expect(pending.actions).toEqual(expect.arrayContaining([
      expect.objectContaining({ id: "artifact:confirm-terminal-neutralised", enabled: true }),
      expect.objectContaining({ id: "action:promotion", enabled: false }),
    ]));

    const confirmed = presentFabricConsole(
      reviewed,
      controller,
      createFabricUiState({
        artifactConfirmation: {
          evidenceId: actionableRow.stableId,
          evidenceRevision: 7,
          sourceDigest: digestA,
          renderedDigest: digestB,
          transformation: "terminal-neutralised",
          pageCount: 1,
        },
      }),
      { columns: 80, rows: 24 },
    );
    expect(confirmed.actions).not.toEqual(expect.arrayContaining([
      expect.objectContaining({ id: "artifact:confirm-terminal-neutralised" }),
    ]));
    expect(confirmed.actions).toEqual(expect.arrayContaining([
      expect.objectContaining({ id: "action:promotion", enabled: true }),
    ]));
  });

  it("presents the evidence decision ladder and explains unavailable typed entry points", () => {
    const dataset = richDataset();
    const evidenceRow = dataset.pages.evidence.rows[0];
    if (evidenceRow === undefined) throw new Error("evidence fixture unavailable");
    const reviewable: FabricConsoleDataset = {
      ...dataset,
      workflowCapabilities: {
        intake: { state: "available" },
        gate: { state: "available" },
        implement: { state: "unavailable", reason: "implementation-protocol-unavailable" },
        launch: { state: "available" },
        git: { state: "unavailable", reason: "typed-planner-unregistered" },
        promotion: { state: "unavailable", reason: "typed-planner-unregistered" },
      },
      pages: {
        ...dataset.pages,
        evidence: {
          ...dataset.pages.evidence,
          rows: [{
            ...evidenceRow,
            detailRef: {
              kind: "evidence",
              evidenceId: evidenceRow.stableId,
              expectedRevision: 7,
            },
          }],
        },
      },
      inspection: {
        kind: "artifact",
        state: "current",
        binding: {
          view: "evidence",
          itemId: evidenceRow.stableId,
          itemRevision: evidenceRow.revision,
          projectionRevision: revisionFromProtocol(11),
        },
        readTransactionId: "artifact-decision-ladder",
        result: {
          artifactRef: { path: "docs/spec.md" as never, digest: digestA },
          evidenceRevision: 7,
          evidenceKind: "artifact",
          sourceKind: "project-file",
          publisherKind: "agent",
          publisherRef: "chair-1",
          projectSessionId: sessionId,
          coordinationRunId: "AFAB-004" as never,
          taskId: null,
          createdAt: timestamp,
          mediaType: "text/markdown",
          content: "reviewed",
          totalBytes: 8,
          totalLines: 1,
          renderedTotalBytes: 8,
          renderedTotalLines: 1,
          renderedArtifactDigest: digestA,
          transformation: "none",
          terminalNeutralised: true,
          capabilityValuesRedacted: true,
          credentialValuesRedacted: true,
          pages: [{ pageIndex: 0, lineFragment: "whole", pageContentDigest: digestA, bytes: 8 }],
          coverage: { complete: true, verified: true, pageCount: 1 },
          reviewDisposition: "eligible",
        },
      },
    };
    const base = controllerState();
    const controller: ConsoleControllerState = {
      ...base,
      activeView: "evidence",
      selectionByView: {
        ...base.selectionByView,
        evidence: { stableId: evidenceRow.stableId, revision: evidenceRow.revision },
      },
    };

    const presentation = presentFabricConsole(
      reviewable,
      controller,
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );

    expect(presentation.actions).toEqual(expect.arrayContaining([
      expect.objectContaining({
        id: "workflow:discuss",
        label: "Discuss",
        enabled: true,
      }),
      expect.objectContaining({ id: "workflow:accept", label: "Accept", enabled: true }),
      expect.objectContaining({
        id: "workflow:request-changes",
        label: "Request changes",
        enabled: true,
      }),
      expect.objectContaining({ id: "workflow:defer", label: "Defer", enabled: true }),
      expect.objectContaining({
        id: "workflow:implement",
        label: "Implement...",
        enabled: false,
        reason: "implementation-protocol-unavailable",
      }),
    ]));
  });

  it("enables only the selected run's exact projected control eligibility", () => {
    const dataset = richDataset();
    const run = dataset.pages.runs.rows[0];
    if (run === undefined || run.summary?.kind !== "run") {
      throw new Error("run fixture unavailable");
    }
    const guarded: FabricConsoleDataset = {
      ...dataset,
      productionActionPlanning: true,
      pages: {
        ...dataset.pages,
        runs: {
          ...dataset.pages.runs,
          rows: [{
            ...run,
            detailRef: {
              kind: "run",
              coordinationRunId: "AFAB-004" as never,
              expectedRevision: 7,
            },
            actionAvailability: {
              state: "available",
              actions: ["pause", "cancel"],
              requiresPreview: true,
            },
          }],
        },
      },
    };
    const base = controllerState();
    const controller: ConsoleControllerState = {
      ...base,
      activeView: "runs",
      selectionByView: {
        ...base.selectionByView,
        runs: { stableId: run.stableId, revision: run.revision },
      },
    };

    const presentation = presentFabricConsole(
      guarded,
      controller,
      createFabricUiState({ draft: "" }),
      { columns: 80, rows: 24 },
    );

    expect(presentation.actions).toEqual(expect.arrayContaining([
      expect.objectContaining({
        id: "action:pause",
        enabled: true,
      }),
      expect.objectContaining({ id: "action:cancel", enabled: false, reason: "enter-a-reason" }),
    ]));
    expect(presentation.actions).not.toEqual(expect.arrayContaining([
      expect.objectContaining({ id: "action:resume" }),
      expect.objectContaining({ id: "action:steer" }),
    ]));
  });

  it("enables Project-row cancel only for an exact live effect-free prelaunch session", () => {
    const dataset = richDataset();
    const project = dataset.pages.project.rows[0];
    const snapshot = dataset.snapshot;
    if (
      project === undefined ||
      snapshot?.session.freshness !== "live" || snapshot.session.value === null
    ) {
      throw new Error("live Project session fixture unavailable");
    }
    const guarded: FabricConsoleDataset = {
      ...dataset,
      productionActionPlanning: true,
      snapshot: {
        ...snapshot,
        session: {
          ...snapshot.session,
          value: { ...snapshot.session.value, state: "draft" },
        },
      },
      pages: {
        ...dataset.pages,
        project: {
          ...dataset.pages.project,
          rows: [{
            ...project,
            detailRef: { kind: "project", projectId, expectedRevision: 7 },
            actionAvailability: {
              state: "available",
              actions: ["cancel"],
              requiresPreview: true,
            },
          }],
        },
      },
    };
    const base = controllerState();
    const controller: ConsoleControllerState = {
      ...base,
      activeView: "project",
      selectionByView: {
        ...base.selectionByView,
        project: { stableId: project.stableId, revision: project.revision },
      },
    };

    expect(presentFabricConsole(
      guarded,
      controller,
      createFabricUiState({ draft: "cancel unused draft" }),
      { columns: 80, rows: 24 },
    ).actions).toEqual(expect.arrayContaining([
      expect.objectContaining({ id: "action:cancel", enabled: true }),
    ]));
    expect(presentFabricConsole(
      guarded,
      controller,
      createFabricUiState({ draft: "" }),
      { columns: 80, rows: 24 },
    ).actions).toEqual(expect.arrayContaining([
      expect.objectContaining({ id: "action:cancel", enabled: false, reason: "enter-a-reason" }),
    ]));

    const wrongState = {
      ...guarded,
      snapshot: {
        ...snapshot,
        session: {
          ...snapshot.session,
          value: { ...snapshot.session.value, state: "active" as const },
        },
      },
    } satisfies FabricConsoleDataset;
    expect(presentFabricConsole(
      wrongState,
      controller,
      createFabricUiState({ draft: "cancel active session" }),
      { columns: 80, rows: 24 },
    ).actions).toStrictEqual([]);
  });

  it("keeps typed launch, Git and promotion entry points discoverable with capability reasons", () => {
    const dataset = richDataset();
    const project = dataset.pages.project.rows[0];
    if (project === undefined) throw new Error("project fixture unavailable");
    const typedEntries: FabricConsoleDataset = {
      ...dataset,
      pages: {
        ...dataset.pages,
        project: {
          ...dataset.pages.project,
          rows: [{
            ...project,
            detailRef: { kind: "project", projectId, expectedRevision: 7 },
            actionAvailability: {
              state: "available",
              actions: ["project-session-launch", "promotion"],
              requiresPreview: true,
            },
          }],
        },
      },
      workflowCapabilities: {
        intake: { state: "available" },
        gate: { state: "available" },
        launch: { state: "available" },
        git: { state: "unavailable", reason: "git-contract-not-negotiated" },
        promotion: { state: "available" },
      },
    };
    const base = controllerState();
    const controller: ConsoleControllerState = {
      ...base,
      activeView: "project",
      selectionByView: {
        ...base.selectionByView,
        project: { stableId: project.stableId, revision: project.revision },
      },
    };

    const presentation = presentFabricConsole(
      typedEntries,
      controller,
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );

    expect(presentation.actions).toEqual(expect.arrayContaining([
      expect.objectContaining({ id: "workflow:launch", enabled: true }),
      expect.objectContaining({
        id: "workflow:git",
        enabled: false,
        reason: "git-contract-not-negotiated",
      }),
      expect.objectContaining({ id: "workflow:promotion", enabled: true }),
    ]));

    const withoutPromotionAuthority = presentFabricConsole(
      {
        ...typedEntries,
        pages: {
          ...typedEntries.pages,
          project: {
            ...typedEntries.pages.project,
            rows: [{
              ...typedEntries.pages.project.rows[0]!,
              actionAvailability: {
                state: "available",
                actions: ["project-session-launch"],
                requiresPreview: true,
              },
            }],
          },
        },
      },
      controller,
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );
    expect(withoutPromotionAuthority.actions).toContainEqual(expect.objectContaining({
      id: "workflow:promotion",
      enabled: false,
      reason: "authority-insufficient",
    }));
  });

  it("offers gate decisions only on judgement-bearing Attention rows", () => {
    const dataset = richDataset();
    const withCapabilities: FabricConsoleDataset = {
      ...dataset,
      workflowCapabilities: {
        intake: { state: "available" },
        gate: { state: "available" },
        launch: { state: "unavailable", reason: "fixture" },
        git: { state: "unavailable", reason: "fixture" },
        promotion: { state: "unavailable", reason: "fixture" },
      },
    };
    const decision = presentFabricConsole(
      withCapabilities,
      controllerState(),
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );
    expect(decision.actions).toEqual(expect.arrayContaining([
      expect.objectContaining({
        id: "workflow:discuss",
        enabled: false,
        reason: "attention-intake-binding-unavailable",
      }),
      expect.objectContaining({
        id: "workflow:accept",
        enabled: true,
      }),
      expect.objectContaining({
        id: "workflow:request-changes",
        enabled: true,
      }),
      expect.objectContaining({
        id: "workflow:defer",
        enabled: true,
      }),
    ]));

    const fyiController = controllerState();
    const fyi = dataset.pages.attention.rows[1];
    if (fyi === undefined) throw new Error("FYI fixture unavailable");
    const fyiPresentation = presentFabricConsole(
      withCapabilities,
      {
        ...fyiController,
        selectionByView: {
          ...fyiController.selectionByView,
          attention: { stableId: fyi.stableId, revision: fyi.revision },
        },
      },
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );
    expect(fyiPresentation.actions.some(({ id }) => id.startsWith("workflow:"))).toBe(false);
  });

  it("shares the exact drain-receipt parser between stop availability and planning", () => {
    const dataset = richDataset();
    const project = dataset.pages.project.rows[0];
    const snapshot = dataset.snapshot;
    const session = snapshot?.session;
    if (
      project === undefined || snapshot === null ||
      session?.freshness !== "live" ||
      session.value === null
    ) throw new Error("project/session fixture unavailable");
    const stopping: FabricConsoleDataset = {
      ...dataset,
      productionActionPlanning: true,
      snapshot: {
        ...snapshot,
        session: {
          ...session,
          value: { ...session.value, state: "quiescing" },
        },
      },
      pages: {
        ...dataset.pages,
        project: {
          ...dataset.pages.project,
          rows: [{
            ...project,
            detailRef: {
              kind: "project",
              projectId,
              expectedRevision: 7,
            },
            actionAvailability: {
              state: "available",
              actions: ["project-session-stop"],
              requiresPreview: true,
            },
          }],
        },
      },
    };
    const base = controllerState();
    const controller: ConsoleControllerState = {
      ...base,
      activeView: "project",
      selectionByView: {
        ...base.selectionByView,
        project: { stableId: project.stableId, revision: project.revision },
      },
    };
    const invalid = presentFabricConsole(
      stopping,
      controller,
      createFabricUiState({ draft: `../private/drain.json@${digestA}` }),
      { columns: 80, rows: 24 },
    );
    expect(invalid.actions).toContainEqual(expect.objectContaining({
      id: "action:project-session-stop",
      enabled: false,
      reason: "enter-drain-receipt-ref",
    }));
    const valid = presentFabricConsole(
      stopping,
      controller,
      createFabricUiState({ draft: `receipts/drain.json@${digestA}` }),
      { columns: 80, rows: 24 },
    );
    expect(valid.actions).toContainEqual(expect.objectContaining({
      id: "action:project-session-stop",
      enabled: true,
    }));
  });

  it("answers the reference questions from canonical facts without inferred progress", () => {
    const presentation = presentFabricConsole(
      richDataset(),
      controllerState(),
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );

    expect(presentation.mode).toBe("reference");
    expect(presentation.header).toMatchObject({
      project: "project-1",
      session: "session-1",
      run: "AFAB-004",
      phase: "implement",
      owner: "codex-chair",
      nextMilestone: "Console GREEN",
      health: "blocked",
      attentionCount: 1,
      freshness: "live",
    });
    expect(presentation.views.map(({ view }) => view)).toStrictEqual(FABRIC_VIEWS);
    expect(presentation.masterRows[0]).toMatchObject({
      stableId: "attention:safety",
      urgencyMarker: "!!",
      freshness: "LIVE 5s",
    });
    expect(JSON.stringify(presentation)).not.toMatch(/\d+%|percentage/i);
  });

  it("keeps advisory attention in the collapsed Watch stream", () => {
    const dataset = richDataset();
    const controller = controllerState();
    const presentation = presentFabricConsole(
      dataset,
      controller,
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );

    expect(presentation.needsYouRows.map(({ stableId }) => stableId)).toStrictEqual([
      "attention:safety",
    ]);
    expect(presentation.watchRows.map(({ stableId }) => stableId)).toStrictEqual([
      "attention:fyi",
    ]);
    expect(presentation.watchCollapsed).toBe(true);
    expect(presentation.header.needsYouCount).toBe(1);
    expect(presentation.header.watchCount).toBe(1);

    for (const viewport of [
      { columns: 80, rows: 24 },
      { columns: 120, rows: 32 },
    ]) {
      const frame = renderFabricConsoleFrame(
        dataset,
        controller,
        createFabricUiState(),
        viewport,
      );
      expect(frame.rows.join("\n")).toContain("WATCH:1 collapsed");
      expect(frame.rows.join("\n")).toContain(
        "WATCH latest: Routine evaluation complete",
      );
    }
  });

  it("keeps unavailable safety-integrity attention in Needs you", () => {
    const base = richDataset();
    const urgent = base.pages.attention.rows[0];
    if (urgent === undefined) throw new Error("urgent attention fixture unavailable");
    const dataset: FabricConsoleDataset = {
      ...base,
      pages: {
        ...base.pages,
        attention: {
          ...base.pages.attention,
          rows: [{
            ...urgent,
            summary: null,
            freshness: {
              state: "unavailable",
              source: "fabric",
              revision: urgent.revision,
              observedAt: timestamp,
              ageMs: 5_000,
              reason: "summary not projected",
            },
          }],
        },
      },
    };

    const presentation = presentFabricConsole(
      dataset,
      controllerState(),
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );
    expect(presentation.needsYouRows.map(({ stableId }) => stableId)).toStrictEqual([
      urgent.stableId,
    ]);
    expect(presentation.watchRows).toStrictEqual([]);

    const frame = renderFabricConsoleFrame(
      dataset,
      controllerState(),
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );
    expect(frame.rows.join("\n")).toContain(urgent.stableId);
    expect(frame.rows.join("\n")).toContain("summary not projected");
    expect(frame.rows.join("\n")).toContain("UNAVAILABLE 5s");
  });

  it("joins authoritative attention grouping metadata and ages quiet projections at render time", () => {
    const base = richDataset();
    const observedAtMs = Date.parse(timestamp);
    if (base.snapshot === null) throw new Error("snapshot fixture unavailable");
    const grouped: FabricConsoleDataset = {
      ...base,
      loadedAtMs: observedAtMs + 125_000,
      snapshot: {
        ...base.snapshot,
        attention: {
          freshness: "live",
          source: "fabric",
          revision: 11,
          observedAt: timestamp,
          value: [{
            itemId: "attention:safety",
            revision: 7,
            label: "Approval",
            priority: "safety-integrity",
            title: "Approve quarantine recovery",
            sourceFreshness: "snapshot",
            lastEventAt: timestamp,
            duplicateCount: 3,
          }],
        },
      },
    };

    const presentation = presentFabricConsole(
      grouped,
      controllerState(),
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );

    expect(presentation.masterRows[0]).toMatchObject({
      freshness: "LIVE 2m",
      secondary: expect.stringContaining("x3 grouped"),
    });
    expect(presentation.masterRows[0]?.secondary).toContain("source snapshot");
    expect(presentation.masterRows[0]?.secondary).toContain("last event 2m");
    expect(presentation.detail?.lines).toContainEqual({
      label: "Attention grouping",
      value: "x3 grouped | source snapshot | last event 2m",
    });

    const later = presentFabricConsole(
      { ...grouped, loadedAtMs: observedAtMs + 185_000 },
      controllerState(),
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );
    expect(later.masterRows[0]?.freshness).toBe("LIVE 3m");
    expect(later.masterRows[0]?.secondary).toContain("last event 3m");
  });

  it("builds a strict Deck queue and an identity-preserving projected-run roster", () => {
    const deckDataset = attentionDeckDataset(true);

    const presentation = presentFabricConsole(
      deckDataset,
      controllerState(),
      createFabricUiState(),
      { columns: 120, rows: 32 },
    );

    expect(presentation.needsYouRows.map(({ stableId }) => stableId)).toStrictEqual([
      "attention:safety",
    ]);
    expect(presentation.watchRows.map(({ stableId }) => stableId)).toStrictEqual([
      "attention:fyi",
    ]);
    expect(presentation.deckRows).toMatchObject([
      {
        kind: "session",
        entityId: "session-1",
        projectSessionId: "session-1",
        owner: null,
        phase: null,
        health: null,
        freshness: null,
      },
      {
        kind: "coordination",
        entityId: "AFAB-004",
        projectSessionId: "session-1",
        owner: "agent-chair",
        phase: "implement",
        health: "blocked",
        freshness: "LIVE 5s",
        lastEvent: timestamp,
      },
      {
        kind: "workstream",
        entityId: "workstream-1",
        deliveryRunId: "delivery-1",
        projectSessionId: "session-1",
        owner: "delivery-lead",
        phase: null,
        health: null,
        freshness: null,
        lastEvent: null,
        updatedAt: timestamp,
      },
    ]);

  });

  it.each([
    [80, 24],
    [120, 32],
  ] as const)(
    "keeps the highest-priority required judgement first and a projected run identifiable at %sx%s",
    (columns, rows) => {
      const dataset = acceptance46Dataset();
      const presentation = presentFabricConsole(
        dataset,
        controllerState(),
        createFabricUiState(),
        { columns, rows },
      );

      expect(presentation.needsYouRows.map(({ stableId }) => stableId)).toStrictEqual([
        "attention:safety",
        "attention:critical-path",
        "attention:acceptance-ready",
      ]);
      expect(presentation.topAttention?.stableId).toBe("attention:safety");
      expect(presentation.watchRows.map(({ stableId }) => stableId)).toEqual(
        expect.arrayContaining([
          "attention:fyi",
          "attention:advisory-signal-0",
          "attention:advisory-signal-1",
          "attention:advisory-signal-2",
          "attention:advisory-signal-3",
          "attention:advisory-signal-4",
        ]),
      );

      const frame = renderFabricConsoleFrame(
        dataset,
        controllerState(),
        createFabricUiState(),
        { columns, rows },
      );
      const safety = frame.hitRegions.find(
        ({ id }) => id === "row:attention:attention:safety",
      );
      const critical = frame.hitRegions.find(
        ({ id }) => id === "row:attention:attention:critical-path",
      );
      const run = frame.hitRegions.find(
        ({ id }) => id === "deck:coordination:session-1:AFAB-004",
      );
      expect(safety).toBeDefined();
      expect(critical).toBeDefined();
      expect(run).toBeDefined();
      if (safety === undefined || critical === undefined || run === undefined) return;
      expect(safety.rect.y1).toBeLessThan(critical.rect.y1);
      expect(frame.rows[safety.rect.y1 - 1]).toContain("!! Approve quarantine recovery");
      expect(frame.rows[run.rect.y1 - 1]).toContain("COORDINATION AFAB-004");
      expect(frame.rows[run.rect.y1 - 1]).toContain("BLOCKED");
      expect(frame.rows.join("\n")).toContain("WATCH:6 collapsed");
    },
  );

  it.each([
    [80, 24],
    [120, 32],
  ] as const)(
    "classifies Needs you strictly from urgency plus label, never advisory heuristic prose, at %sx%s",
    (columns, rows) => {
      const base = acceptance46Dataset();
      const dataset: FabricConsoleDataset = {
        ...base,
        pages: {
          ...base.pages,
          attention: {
            ...base.pages.attention,
            rows: base.pages.attention.rows.filter(({ stableId }) =>
              stableId === "attention:fyi" || stableId.startsWith("attention:advisory-signal-")
            ),
          },
        },
      };
      const presentation = presentFabricConsole(
        dataset,
        controllerState(),
        createFabricUiState(),
        { columns, rows },
      );

      expect(presentation.needsYouRows).toStrictEqual([]);
      expect(presentation.watchRows).toHaveLength(6);
      expect(presentation.watchRows.map(({ primary }) => primary)).toEqual(
        expect.arrayContaining([
          "Inactive for three hours",
          "High message volume",
          "Context pressure high",
          "Provider pane absent",
          "Optional integration outage",
        ]),
      );
      expect(presentation.watchRows.find(({ stableId }) => stableId === "attention:fyi"))
        .toMatchObject({ urgencyMarker: "!>" });

      const visible = renderFabricConsoleFrame(
        dataset,
        controllerState(),
        createFabricUiState(),
        { columns, rows },
      ).rows.join("\n");
      expect(visible).toContain("NEEDS YOU:0");
      expect(visible).toContain("No projected user judgement required.");
      expect(visible).toContain("WATCH:6 collapsed");
    },
  );

  it("filters Deck bands, keeps pins visible first, and discloses the filtered count", () => {
    const urgencyDataset = urgencyGlyphDataset();
    const watchRow = attentionDeckDataset().pages.attention.rows[1];
    if (watchRow === undefined) throw new Error("Watch fixture unavailable");
    const dataset = {
      ...urgencyDataset,
      pages: {
        ...urgencyDataset.pages,
        attention: {
          ...urgencyDataset.pages.attention,
          rows: [...urgencyDataset.pages.attention.rows, watchRow],
        },
      },
    };
    const unchangedDataset = structuredClone(dataset);
    const ui = createFabricUiState({
      filterQuery: "status:degraded",
      pinnedRowIds: ["deck:session:session-healthy"],
    });
    const presentation = presentFabricConsole(
      dataset,
      controllerState(),
      ui,
      { columns: 80, rows: 24 },
    );

    expect(presentation.needsYouRows).toStrictEqual([]);
    expect(presentation.watchRows).toStrictEqual([]);
    expect(presentation.deckRows.map(({ entityId }) => entityId)).toStrictEqual([
      "session-healthy",
      "session-degraded",
      "run-degraded",
    ]);
    expect(presentation).toMatchObject({
      deckFilterActive: true,
      deckShownCount: 3,
      deckUnfilteredCount: 12,
      detail: null,
      actions: [],
    });

    const visible = renderFabricConsoleFrame(
      dataset,
      controllerState(),
      ui,
      { columns: 80, rows: 24 },
    ).rows.join("\n");
    expect(visible).toContain("FILTERED VIEW, 3 of 12 shown");
    expect(visible).toMatch(/\^\s+PINNED.*session-healthy/u);

    const compact = renderFabricConsoleFrame(
      attentionDeckDataset(),
      controllerState(),
      createFabricUiState({
        filterQuery: "status:ok",
        pinnedRowIds: ["row:attention:attention:safety"],
      }),
      { columns: 30, rows: 6 },
    ).rows.join("\n");
    expect(compact).toContain("FILTERED 3/5");
    expect(compact).toContain("^ PINNED");
    const shortWide = renderFabricConsoleFrame(
      attentionDeckDataset(),
      controllerState(),
      createFabricUiState({
        filterQuery: "status:ok",
        pinnedRowIds: ["row:attention:attention:safety"],
      }),
      { columns: 80, rows: 6 },
    ).rows.join("\n");
    expect(shortWide).toContain("^ PINNED");

    const pinnedWatch = renderFabricConsoleFrame(
      dataset,
      controllerState(),
      createFabricUiState({
        filterQuery: "status:degraded",
        pinnedRowIds: ["row:attention:attention:fyi"],
      }),
      { columns: 120, rows: 32 },
    ).rows.join("\n");
    expect(pinnedWatch).toContain("WATCH latest: ^ PINNED Routine evaluation");

    const emptyFiltered = renderFabricConsoleFrame(
      attentionDeckDataset(),
      controllerState(),
      createFabricUiState({ filterQuery: "identity-that-is-not-projected" }),
      { columns: 80, rows: 24 },
    ).rows.join("\n");
    expect(emptyFiltered).toContain("No rows match the active filter.");
    expect(emptyFiltered).not.toContain("No projected user judgement required.");
    expect(emptyFiltered).not.toContain("No projected runs.");

    const textFiltered = presentFabricConsole(
      dataset,
      controllerState(),
      createFabricUiState({ filterQuery: "status:degraded RUN-DEGRADED" }),
      { columns: 80, rows: 24 },
    );
    expect(textFiltered.deckRows.map(({ entityId }) => entityId)).toStrictEqual([
      "run-degraded",
    ]);
    const urgent = presentFabricConsole(
      attentionDeckDataset(),
      controllerState(),
      createFabricUiState({ filterQuery: "status:urgent" }),
      { columns: 80, rows: 24 },
    );
    expect(urgent.needsYouRows.map(({ stableId }) => stableId)).toStrictEqual([
      "attention:safety",
    ]);
    expect(urgent.deckRows.map(({ entityId }) => entityId)).toStrictEqual([
      "AFAB-004",
    ]);
    expect(dataset).toStrictEqual(unchangedDataset);
  });

  it("renders every urgency glyph and its projection-only non-colour twin", () => {
    const dataset = urgencyGlyphDataset();
    const presentation = presentFabricConsole(
      dataset,
      controllerState(),
      createFabricUiState(),
      { columns: 120, rows: 32 },
    );

    expect(presentation.needsYouRows.map(({ stableId, urgencyMarker }) => [
      stableId,
      urgencyMarker,
    ])).toStrictEqual([
      ["attention:safety", "!!"],
      ["attention:critical", "!>"],
      ["attention:expiring", "!"],
      ["attention:acceptance", "+"],
      ["attention:stale", "?"],
    ]);
    expect(presentation.deckRows.map(({ entityId, urgencyMarker, statusLabel }) => [
      entityId,
      urgencyMarker,
      statusLabel,
    ])).toStrictEqual([
      ["session-degraded", "~", "DEGRADED"],
      ["run-degraded", "~", "DEGRADED"],
      ["session-healthy", " ", "ACTIVE"],
      ["run-healthy", " ", "HEALTHY"],
      ["session-stale", "?", "UNAVAILABLE"],
      ["run-stale", "?", "STALE"],
    ]);

    const frame = renderFabricConsoleFrame(
      dataset,
      controllerState(),
      createFabricUiState(),
      { columns: 120, rows: 32 },
    );
    const visible = frame.rows.join("\n");
    const activeSession = frame.rows.find((row) => row.includes("SESSION session-healthy"));
    const degradedSession = frame.rows.find((row) => row.includes("SESSION session-degraded"));
    expect(activeSession).toBeDefined();
    expect(activeSession).not.toContain("?");
    expect(activeSession).not.toContain("UNAVAILABLE");
    expect(degradedSession).toContain("~  SESSION session-degraded");
    expect(degradedSession).toContain("DEGRADED");
    expect(visible).toContain("!! Safety gate");
    expect(visible).toContain("!> Critical blocker");
    expect(visible).toContain("!  Authority expires");
    expect(visible).toContain("+  Acceptance ready");
    expect(visible).toContain("?  Stale safety gate");
    expect(visible).toContain("~  COORDINATION run-degraded");
    expect(visible).toContain("DEGRADED");
    expect(visible).toContain("?  COORDINATION run-stale");
    expect(visible).toContain("STALE");
    expect(visible).not.toMatch(/\u001b\[[0-9;]*m/u);
  });

  it("marks live run rows with absent summaries as unavailable instead of guessing", () => {
    const base = urgencyGlyphDataset();
    const { projectSessions: _projectSessions, ...withoutSessions } = base;
    const source = base.pages.runs.rows[0];
    if (source === undefined) throw new Error("run glyph fixture unavailable");
    const presentation = presentFabricConsole(
      {
        ...withoutSessions,
        pages: {
          ...base.pages,
          runs: { ...base.pages.runs, rows: [{ ...source, summary: null }] },
        },
      },
      controllerState(),
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );

    expect(presentation.deckRows).toMatchObject([{
      urgencyMarker: "?",
      statusLabel: "UNAVAILABLE",
    }]);
  });

  it("keeps compact selection unique and retains stale freshness wording", () => {
    const dataset = urgencyGlyphDataset();
    const baseController = controllerState();
    const controller: ConsoleControllerState = {
      ...baseController,
      selectionByView: {
        ...baseController.selectionByView,
        attention: {
          stableId: "attention:critical",
          revision: revisionFromProtocol(7),
        },
      },
    };
    const selected = renderFabricConsoleFrame(
      dataset,
      controller,
      createFabricUiState(),
      { columns: 30, rows: 6 },
    ).rows.join("\n");
    expect(selected.match(/Critical blocker/gu)).toHaveLength(1);
    expect(selected).toContain("Safety gate");

    const staleOnly: FabricConsoleDataset = {
      ...dataset,
      pages: {
        ...dataset.pages,
        attention: {
          ...dataset.pages.attention,
          rows: [dataset.pages.attention.rows.at(-1)!],
        },
      },
    };
    const stale = renderFabricConsoleFrame(
      staleOnly,
      {
        ...baseController,
        selectionByView: {
          ...baseController.selectionByView,
          attention: {
            stableId: "attention:stale",
            revision: revisionFromProtocol(7),
          },
        },
      },
      createFabricUiState(),
      { columns: 30, rows: 6 },
    ).rows.join("\n");
    expect(stale).toContain("*?  Stale safety gate STALE 1m");
  });

  it("aligns Deck glyph cells and keeps compact healthy-run focus visible", () => {
    const dataset = urgencyGlyphDataset();
    const reference = renderFabricConsoleFrame(
      dataset,
      controllerState(),
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );
    const attentionRegion = reference.hitRegions.find(({ id }) => id === "row:attention:attention:safety");
    const sessionRegion = reference.hitRegions.find(({ id }) => id === "deck:session:session-healthy");
    expect(attentionRegion).toBeDefined();
    expect(sessionRegion).toBeDefined();
    if (attentionRegion === undefined || sessionRegion === undefined) return;
    expect(reference.rows[attentionRegion.rect.y1 - 1]?.slice(1, 3)).toBe("!!");
    expect(reference.rows[sessionRegion.rect.y1 - 1]?.slice(1, 3)).toBe("  ");

    const focused = renderFabricConsoleFrame(
      dataset,
      controllerState(),
      createFabricUiState({ focusId: "deck:coordination:session-healthy:run-healthy" }),
      { columns: 30, rows: 6 },
    );
    const runRegion = focused.hitRegions.find(({ id }) => id === "deck:coordination:session-healthy:run-healthy");
    expect(runRegion).toBeDefined();
    if (runRegion === undefined) return;
    expect(focused.rows[runRegion.rect.y1 - 1]).toMatch(/^>/u);
  });

  it.each([
    [30, 6],
    [80, 24],
    [120, 32],
  ] as const)("keeps the two-cell urgency gutter legible at %sx%s", (columns, rows) => {
    const frame = renderFabricConsoleFrame(
      urgencyGlyphDataset(),
      controllerState(),
      createFabricUiState(),
      { columns, rows },
    );
    const visible = frame.rows.join("\n");

    expect(visible).toContain("!!");
    expect(visible).toContain("!>");
    expect(frame.rows.every((line) => cellWidth(line) === columns)).toBe(true);
    expect({ mode: frame.mode, rows: frame.rows }).toMatchSnapshot();
  });

  it("renders an empty Deck fixture without inventing attention or run state", () => {
    const base = attentionDeckDataset();
    const { projectSessions: _projectSessions, ...withoutSessions } = base;
    const dataset: FabricConsoleDataset = {
      ...withoutSessions,
      pages: {
        ...base.pages,
        attention: { ...base.pages.attention, rows: [] },
        runs: { ...base.pages.runs, rows: [] },
      },
    };

    const frame = renderFabricConsoleFrame(
      dataset,
      controllerState(),
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );

    expect(frame.presentation).toMatchObject({
      deckRows: [],
      deckTotalCount: 0,
      needsYouRows: [],
      watchRows: [],
      header: { needsYouCount: 0, watchCount: 0 },
    });
    expect(frame.rows.join("\n")).toContain("No projected user judgement required.");
    expect(frame.rows.join("\n")).toContain("No projected runs.");
  });

  it("groups multiple attachable sessions without selecting one", () => {
    const base = attentionDeckDataset();
    const firstRun = base.pages.runs.rows[0];
    if (firstRun?.summary?.kind !== "run") throw new Error("Deck run unavailable");
    const firstWorkstream = firstRun.summary.identity.workstreams[0];
    if (firstWorkstream === undefined) throw new Error("Deck workstream unavailable");
    const firstRunWithPermutedWorkstreams = {
      ...firstRun,
      summary: {
        ...firstRun.summary,
        identity: {
          ...firstRun.summary.identity,
          workstreams: [firstWorkstream, {
            ...firstWorkstream,
            workstreamId: "workstream-0" as never,
            deliveryRunId: "delivery-0" as never,
          }],
        },
      },
    };
    const secondSessionId = "session-2";
    const secondRun = {
      ...firstRun,
      stableId: "AFAB-009",
      summary: {
        ...firstRun.summary,
        projectSessionId: secondSessionId as never,
        phase: "evaluate",
        identity: {
          ...firstRun.summary.identity,
          chairAgentId: "agent-evaluator" as AgentId,
          workstreams: [],
        },
      },
    };
    const dataset: FabricConsoleDataset = {
      ...base,
      projectSessions: {
        selectedProjectSessionId: null,
        choices: [
          {
            projectSessionId: secondSessionId as never,
            mode: "independent",
            state: "active",
            revision: 3,
            generation: 1,
            lastEventAt: timestamp,
          },
          ...(base.projectSessions?.choices ?? []),
        ],
      },
      pages: {
        ...base.pages,
        runs: {
          ...base.pages.runs,
          rows: [secondRun, firstRunWithPermutedWorkstreams],
        },
      },
    };
    const unchangedDataset = structuredClone(dataset);

    const presentation = presentFabricConsole(
      dataset,
      controllerState(),
      createFabricUiState(),
      { columns: 120, rows: 32 },
    );

    expect(dataset.projectSessions?.selectedProjectSessionId).toBeNull();
    expect(presentation.deckRows.map(({ kind, entityId }) => `${kind}:${entityId}`))
      .toStrictEqual([
        "session:session-1",
        "coordination:AFAB-004",
        "workstream:workstream-0",
        "workstream:workstream-1",
        "session:session-2",
        "coordination:AFAB-009",
      ]);
    expect(dataset).toStrictEqual(unchangedDataset);
  });

  it("groups runs by their projected session identity without synthesising a session row", () => {
    const base = attentionDeckDataset();
    const { projectSessions: _projectSessions, ...withoutSessionChoices } = base;

    const presentation = presentFabricConsole(
      withoutSessionChoices,
      controllerState(),
      createFabricUiState(),
      { columns: 120, rows: 32 },
    );

    expect(presentation.deckRows.map(({ kind, entityId }) => `${kind}:${entityId}`))
      .toStrictEqual([
        "coordination:AFAB-004",
        "workstream:workstream-1",
      ]);
    expect(presentation.deckRows[0]).toMatchObject({
      projectSessionId: "session-1",
      primary: "COORDINATION AFAB-004 | SESSION session-1",
    });
  });

  it("rejects a run with no projected session rather than inventing an identity", () => {
    const base = attentionDeckDataset();
    const sourceRun = base.pages.runs.rows[0];
    if (sourceRun?.summary?.kind !== "run") throw new Error("Deck run unavailable");
    const { projectSessionId: _projectSessionId, ...unscopedSummary } = sourceRun.summary;
    const { projectSessions: _projectSessions, ...withoutSessionChoices } = base;
    const dataset: FabricConsoleDataset = {
      ...withoutSessionChoices,
      pages: {
        ...withoutSessionChoices.pages,
        runs: {
          ...withoutSessionChoices.pages.runs,
          rows: [{ ...sourceRun, summary: unscopedSummary }],
        },
      },
    };

    expect(() => presentFabricConsole(
      dataset,
      controllerState(),
      createFabricUiState(),
      { columns: 120, rows: 32 },
    )).toThrow("exact run projection has no project-session identity");
  });

  it("keeps a multi-run header neutral until an exact run is selected", () => {
    const base = richDataset();
    const runsFact = base.snapshot?.runs;
    if (base.snapshot === null || runsFact === undefined || !("value" in runsFact)) {
      throw new Error("snapshot run fixture unavailable");
    }
    const firstRun = runsFact.value[0];
    if (firstRun === undefined) throw new Error("snapshot run fixture unavailable");
    const dataset: FabricConsoleDataset = {
      ...base,
      snapshot: {
        ...base.snapshot,
        runs: {
          ...runsFact,
          value: [firstRun, {
            ...firstRun,
            runId: "AFAB-009" as never,
            phase: "evaluate",
            chairAgentId: "agent-evaluator" as AgentId,
            health: "healthy",
          }],
        },
      },
    };

    const presentation = presentFabricConsole(
      dataset,
      controllerState(),
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );

    expect(presentation.header).toMatchObject({
      run: "choose:2",
      owner: "unassigned",
      health: "unknown",
      phase: "not selected",
    });
  });

  it("encodes opaque Deck identity tuples without colon collisions", () => {
    const base = attentionDeckDataset();
    const source = base.pages.runs.rows[0];
    if (source?.summary?.kind !== "run") throw new Error("Deck run unavailable");
    const rows = [
      {
        ...source,
        stableId: "c",
        summary: { ...source.summary, projectSessionId: "a:b" as never, identity: { ...source.summary.identity, workstreams: [] } },
      },
      {
        ...source,
        stableId: "b:c",
        summary: { ...source.summary, projectSessionId: "a" as never, identity: { ...source.summary.identity, workstreams: [] } },
      },
    ];
    const dataset: FabricConsoleDataset = {
      ...base,
      projectSessions: {
        selectedProjectSessionId: null,
        choices: ["a:b", "a"].map((projectSessionId) => ({
          projectSessionId: projectSessionId as never,
          mode: "independent" as const,
          state: "active" as const,
          revision: 1,
          generation: 1,
          lastEventAt: timestamp,
        })),
      },
      pages: { ...base.pages, runs: { ...base.pages.runs, rows } },
    };
    const presentation = presentFabricConsole(
      dataset,
      controllerState(),
      createFabricUiState(),
      { columns: 120, rows: 32 },
    );
    const coordinationIds = presentation.deckRows
      .filter(({ kind }) => kind === "coordination")
      .map(({ stableId }) => stableId);

    expect(coordinationIds).toStrictEqual([
      "coordination:a:b%3Ac",
      "coordination:a%3Ab:c",
    ]);
    expect(new Set(coordinationIds).size).toBe(2);
  });

  it("keeps a degraded run row visible without inventing its missing session or fields", () => {
    const base = attentionDeckDataset();
    const { projectSessions: _projectSessions, ...baseWithoutSessions } = base;
    const source = base.pages.runs.rows[0];
    if (source === undefined) throw new Error("Deck run unavailable");
    const dataset: FabricConsoleDataset = {
      ...baseWithoutSessions,
      pages: {
        ...base.pages,
        runs: {
          ...base.pages.runs,
          rows: [{
            ...source,
            stableId: "run-unavailable",
            summary: null,
            freshness: {
              state: "unavailable",
              source: "fabric",
              revision: source.revision,
              observedAt: timestamp,
              ageMs: 0,
              reason: "projection-source-unavailable",
            },
          }],
        },
      },
    };

    const presentation = presentFabricConsole(
      dataset,
      controllerState(),
      createFabricUiState(),
      { columns: 120, rows: 32 },
    );

    expect(presentation.deckRows).toMatchObject([{
      kind: "coordination",
      entityId: "run-unavailable",
      projectSessionId: null,
      owner: null,
      phase: null,
      health: null,
      lastEvent: null,
      primary: "COORDINATION run-unavailable | SESSION not projected",
    }]);
    expect(presentation.deckRows[0]?.secondary).toContain("owner:not projected");
    expect(presentation.deckRows[0]?.freshness).toBe("UNAVAILABLE now");
  });

  it.each([
    [30, 6, "strip"],
    [80, 24, "reference"],
    [120, 32, "wide"],
    [200, 10, "compact"],
  ] as const)(
    "keeps Needs you and the projected-run roster visible at %sx%s",
    (columns, rows, mode) => {
      const frame = renderFabricConsoleFrame(
        attentionDeckDataset(),
        controllerState(),
        createFabricUiState(),
        { columns, rows },
      );
      const visible = frame.rows.join("\n");

      expect(frame.mode).toBe(mode);
      expect(visible).toContain("AFAB-004");
      expect(visible).toContain("Approve quarantine");
      if (columns === 30) {
        expect(frame.rows[0]).toContain("project-1 NEEDS 1 RUNS 1 !");
        expect(visible).toContain("*!! Approve quarantine");
        expect(visible).toContain("AFAB-004 implement BLOCKED");
        expect(frame.rows.at(-1)).toContain("[enter]open");
        expect(frame.rows.at(-1)).toContain("[q]Detach");
      } else {
        expect(visible).toMatch(/NEEDS YOU|N:1/u);
        expect(visible).toMatch(/PROJECTED ROSTER|R:3/u);
        expect(visible).toMatch(/RUN IDENTITIES:2|RUN:2/u);
      }
      expect(frame.hitRegions.some(({ id }) => id === "detach")).toBe(true);
      expect(frame.rows.every((line) => cellWidth(line) === columns)).toBe(true);
      expect({ mode: frame.mode, rows: frame.rows }).toMatchSnapshot();
    },
  );

  it.each([
    ["available", "sent"],
    ["unavailable", "failed"],
    ["stale", "ambiguous"],
  ] as const)(
    "renders native notification %s status at reference and compact dimensions without granting an action",
    (status, journalState) => {
      const dataset = richDataset();
      const first = dataset.pages.attention.rows[0];
      if (
        first?.summary?.kind !== "attention" ||
        first.summary.nativeNotification.kind !== "daemon-journal"
      ) {
        throw new Error("expected attention fixture");
      }
      const notification = {
        ...first.summary.nativeNotification,
        status,
        journalState,
      };
      const attentionRows = [
        {
          ...first,
          detailRef: {
            kind: "run" as const,
            coordinationRunId: "AFAB-004" as never,
            expectedRevision: 7,
          },
          actionAvailability: {
            state: "available" as const,
            actions: ["pause", "resume"] as const,
            requiresPreview: true as const,
          },
          summary: { ...first.summary, nativeNotification: notification },
        },
        ...dataset.pages.attention.rows.slice(1),
      ];
      const projected = {
        ...dataset,
        pages: {
          ...dataset.pages,
          attention: { ...dataset.pages.attention, rows: attentionRows },
        },
      };
      const state = controllerState();
      const stateBefore = structuredClone(state);
      const datasetBefore = structuredClone(projected);

      const presentation = presentFabricConsole(
        projected,
        state,
        createFabricUiState(),
        { columns: 80, rows: 24 },
      );
      expect(presentation.masterRows[0]?.secondary).toContain(
        `notify ${status}/${journalState}`,
      );
      expect(presentation.detail?.lines).toEqual(
        expect.arrayContaining([
          {
            label: "Native notification",
            value: `${status} | journal ${journalState}`,
          },
          {
            label: "Notification basis",
            value: expect.stringContaining("integration available | delivery r7 | claim g3"),
          },
        ]),
      );
      expect(presentation.actions).toStrictEqual([]);

      const reference = renderFabricConsoleFrame(
        projected,
        state,
        createFabricUiState(),
        { columns: 80, rows: 24 },
      );
      const compact = renderFabricConsoleFrame(
        projected,
        state,
        createFabricUiState({ compactPane: "detail" }),
        { columns: 60, rows: 18 },
      );
      expect(reference.rows.join("\n")).toContain(
        `Native notification: ${status} | journal ${journalState}`,
      );
      expect(compact.rows.join("\n")).toContain(
        `Native notification: ${status} | journal ${journalState}`,
      );
      expect(state).toStrictEqual(stateBefore);
      expect(projected).toStrictEqual(datasetBefore);
    },
  );

  it.each(FABRIC_VIEWS.filter((view) => view !== "runs"))(
    "suppresses raw control capability leakage from the %s view",
    (view) => {
      const dataset = richDataset();
      const selected = dataset.pages[view].rows[0];
      if (selected === undefined) throw new Error(`${view} fixture unavailable`);
      const leaked = {
        ...selected,
        detailRef: {
          kind: "run" as const,
          projectSessionId: sessionId,
          coordinationRunId: "AFAB-004" as never,
          expectedRevision: 7,
        },
        actionAvailability: {
          state: "available" as const,
          actions: ["pause", "resume", "cancel", "steer"] as const,
          requiresPreview: true as const,
        },
      };
      const projected: FabricConsoleDataset = {
        ...dataset,
        pages: {
          ...dataset.pages,
          [view]: { ...dataset.pages[view], rows: [leaked] },
        } as FabricConsoleDataset["pages"],
      };
      const base = controllerState();
      const state: ConsoleControllerState = {
        ...base,
        activeView: view,
        selectionByView: {
          ...base.selectionByView,
          [view]: { stableId: selected.stableId, revision: selected.revision },
        },
      };
      const presentation = presentFabricConsole(
        projected,
        state,
        createFabricUiState({ draft: "unsafe control draft" }),
        { columns: 80, rows: 24 },
      );
      expect(presentation.actions.filter(({ id }) =>
        id === "action:pause" || id === "action:resume" ||
        id === "action:cancel" || id === "action:steer"
      )).toStrictEqual([]);
    },
  );

  it("renders and exports an unavailable optional notification without synthetic journal observations", () => {
    const dataset = richDataset();
    const first = dataset.pages.attention.rows[0];
    if (first?.summary?.kind !== "attention") throw new Error("expected Attention fixture");
    const unavailableRow: ConsoleRow<"attention"> = {
      ...first,
      summary: {
        ...first.summary,
        nativeNotification: {
          kind: "feature-unavailable",
          status: "unavailable",
          reason: "feature-not-negotiated",
        },
      },
    };
    const unavailable: FabricConsoleDataset = {
      ...dataset,
      connection: {
        state: "live",
        compatibility: { mode: "current" },
      },
      pages: {
        ...dataset.pages,
        attention: {
          ...dataset.pages.attention,
          rows: [unavailableRow, ...dataset.pages.attention.rows.slice(1)],
        },
      },
    };
    const state = controllerState();
    const ui = createFabricUiState();
    const presentation = presentFabricConsole(unavailable, state, ui, { columns: 80, rows: 24 });

    expect(presentation.connection).toBe("LIVE");
    expect(presentation.masterRows[0]?.secondary).toContain(
      "notify unavailable/feature-not-negotiated",
    );
    expect(presentation.detail?.lines).toEqual(expect.arrayContaining([{
      label: "Native notification",
      value: "unavailable | feature-not-negotiated",
    }]));
    expect(presentation.detail?.lines.some((line) => line.label === "Notification basis")).toBe(false);

    const exported = JSON.parse(renderConsoleSnapshot({
      dataset: unavailable,
      controller: state,
      ui,
      viewport: { columns: 80, rows: 24 },
    }, "json")) as {
      connection: string;
      connectionDetail: FabricConsoleDataset["connection"];
      views: { attention: { rows: readonly { secondary: string }[]; detail: { lines: readonly { label: string; value: string }[] } } };
    };
    expect(exported.connection).toBe("LIVE");
    expect(exported.connectionDetail).toMatchObject({
      state: "live",
      compatibility: { mode: "current" },
    });
    expect(exported.views.attention.rows[0]?.secondary).toContain("feature-not-negotiated");
    const notificationLines = exported.views.attention.detail.lines.filter((line) =>
      line.label.startsWith("Notification") || line.label === "Native notification"
    );
    expect(notificationLines).toStrictEqual([{
      label: "Native notification",
      value: "unavailable | feature-not-negotiated",
    }]);
    expect(JSON.stringify(notificationLines)).not.toMatch(/journal|timestamp|observed|delivery|claim|integration|\b0\b/iu);
  });

  it("uses a full-frame Review containing every consequential binding", () => {
    const presentation = presentFabricConsole(
      richDataset(),
      controllerState(review()),
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );

    expect(presentation.review).toMatchObject({
      stage: "review",
      itemId: "attention:safety",
      itemRevision: "7",
      projectionRevision: "11",
      previewRevision: "3",
      previewDigest: digestA,
      intentDigest: digestB,
      beforeStateDigest: digestA,
      consequenceClass: "consequential",
      confirmationMode: "explicit",
      gates: [
        {
          gateId: "gate-1",
          gateRevision: "7",
          scope: "task:task-1",
          question: "Resume quarantined task?",
          consequences: ["Task execution may continue."],
        },
      ],
    });
    expect(presentation.actions).toStrictEqual([
      {
        id: "review:continue",
        label: "Continue to confirmation",
        enabled: true,
        availableAction: null,
      },
      {
        id: "review:cancel",
        label: "Cancel Review",
        enabled: true,
        availableAction: null,
      },
    ]);
    const frame = renderFabricConsoleFrame(
      richDataset(),
      controllerState(review()),
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );
    expect(frame.rows.join("\n")).toContain(
      "Consequence: Task execution may continue.",
    );
  });

  it("presents exact accepted artifact, action and target for promotion", () => {
    const base = review("committed");
    const promotion: ActionReview = {
      ...base,
      preview: {
        ...base.preview,
        consequenceClass: "promotion",
        intent: {
          kind: "promotion",
          projectSessionId: sessionId,
          coordinationRunId: "AFAB-004" as never,
          gateId: "gate-release" as never,
          expectedGateRevision: 9,
          expectedGateStatus: "approved",
          releaseBinding: {
            acceptedDeliveryReceiptRef: {
              path: "receipts/accepted.json" as never,
              digest: digestA,
            },
            artifactDigest: digestB,
            promotionAction: "publish",
            target: "registry:stable",
          },
        },
      },
      status: {
        status: "committed",
        commandId: "promotion-command",
        receipt: {
          commandId: "promotion-command",
          previewId: "preview-1",
          previewRevision: 3,
          intentDigest: digestB,
          beforeStateDigest: digestA,
          afterStateDigest: digestB,
          effectRef: {
            path: "effects/promotion.json" as never,
            digest: digestA,
          },
          evidenceRefs: [],
          committedAt: timestamp,
        },
      },
    };
    const presentation = presentFabricConsole(
      richDataset(),
      controllerState(promotion),
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );

    expect(presentation.review?.intent).toEqual(
      expect.arrayContaining([
        { label: "Accepted receipt", value: "receipts/accepted.json" },
        { label: "Accepted receipt digest", value: digestA },
        { label: "Artifact digest", value: digestB },
        { label: "Promotion action", value: "publish" },
        { label: "Promotion target", value: "registry:stable" },
      ]),
    );
    expect(presentation.review?.receipt).toStrictEqual({
      commandId: "promotion-command",
      afterStateDigest: digestB,
      effect: `effects/promotion.json@${digestA}`,
      committedAt: timestamp,
    });
    const frame = renderFabricConsoleFrame(
      richDataset(),
      controllerState(promotion),
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );
    const text = frame.rows.join("\n");
    expect(text).toContain(`RcptDig:${digestA}`);
    expect(text).toContain(`Artifact:${digestB}`);
    expect(text).toContain("Action:publish");
    expect(text).toContain("Target:registry:stable");
  });

  it("keeps optional GitHub failure explicit without degrading local projection", () => {
    const dataset = richDataset(11, "unavailable");
    const state = { ...controllerState(), activeView: "system" as const };
    const presentation = presentFabricConsole(
      dataset,
      state,
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );

    expect(presentation.connection).toBe("LIVE");
    expect(presentation.masterRows[0]).toMatchObject({
      stableId: "github",
      primary: "github",
      secondary: "adapter disabled",
      freshness: "UNAVAILABLE 5s",
    });
    expect(dataset.pages.work.rows).toHaveLength(1);
  });

  it("keeps a projected conflict visible while the transport is degraded", () => {
    const dataset = richDataset();
    if (dataset.snapshot === null) throw new Error("snapshot fixture unavailable");
    const conflicted: FabricConsoleDataset = {
      ...dataset,
      connection: { state: "degraded", reason: "transport-failure" },
      snapshot: {
        ...dataset.snapshot,
        runs: {
          freshness: "conflict",
          source: "fabric",
          revision: dataset.snapshot.snapshotRevision,
          observedAt: timestamp,
          candidates: [
            dataset.snapshot.runs.freshness === "conflict"
              ? dataset.snapshot.runs.candidates[0]
              : dataset.snapshot.runs.freshness === "unavailable"
                ? []
                : dataset.snapshot.runs.value,
            [],
          ],
        },
      },
    };

    expect(presentFabricConsole(
      conflicted,
      controllerState(),
      createFabricUiState(),
      { columns: 80, rows: 24 },
    ).header.freshness).toBe("conflict");
  });

  it.each(FABRIC_VIEWS)(
    "characterises the current %s view frame before renderer decomposition",
    (view) => {
      const frame = renderFabricConsoleFrame(
        richDataset(),
        { ...controllerState(), activeView: view },
        createFabricUiState(),
        { columns: 80, rows: 24 },
      );

      expect({
        mode: frame.mode,
        rows: frame.rows,
        hitRegions: frame.hitRegions.map(({ id, kind, rect, enabled, shortcut }) => ({
          id,
          kind,
          rect,
          enabled,
          ...(shortcut === undefined ? {} : { shortcut }),
        })),
      }).toMatchSnapshot();
    },
  );

  it("renders the responsive ladder at exact current terminal dimensions", () => {
    const dataset = richDataset();
    const state = controllerState();
    const ui = createFabricUiState({ draft: "preserve me", focusId: "row:attention:safety" });
    const cases = [
      [140, 36, "wide"],
      [80, 24, "reference"],
      [60, 18, "compact"],
      [30, 6, "strip"],
      [5, 2, "inert"],
      [0, 0, "inert"],
    ] as const;

    for (const [columns, rows, mode] of cases) {
      const before = structuredClone(ui);
      const frame = renderFabricConsoleFrame(dataset, state, ui, { columns, rows });
      expect(frame.mode).toBe(mode);
      expect(frame.columns).toBe(columns);
      expect(frame.rows).toHaveLength(rows);
      expect(frame.rows.every((line) => cellWidth(line) === columns)).toBe(true);
      expect(ui).toStrictEqual(before);
    }
  });

  it("enforces 30x6 as the exact interactive minimum without coercing invalid dimensions", () => {
    const cases = [
      [{ columns: 30, rows: 6 }, "strip"],
      [{ columns: 29, rows: 6 }, "inert"],
      [{ columns: 30, rows: 5 }, "inert"],
      [{ columns: 29, rows: 5 }, "inert"],
      [{ columns: 29, rows: 24 }, "inert"],
      [{ columns: 80, rows: 5 }, "inert"],
      [{ columns: 30.5, rows: 6 }, "inert"],
      [{ columns: 30, rows: 6.5 }, "inert"],
      [{ columns: Number.MAX_SAFE_INTEGER, rows: 6 }, "inert"],
      [{ columns: 80, rows: 24 }, "reference"],
      [{ columns: 140, rows: 36 }, "wide"],
    ] as const;

    for (const [viewport, mode] of cases) {
      expect(responsiveModeFor(viewport)).toBe(mode);
      expect(renderFabricConsoleFrame(
        richDataset(),
        controllerState(),
        createFabricUiState(),
        viewport,
      ).mode).toBe(mode);
    }
  });

  it("keeps one safe selected-item action reachable at the 30x6 minimum", () => {
    const frame = renderFabricConsoleFrame(
      controllableRunDataset(),
      runControllerState(),
      createFabricUiState({ focusId: "action:resume" }),
      { columns: 30, rows: 6 },
    );

    expect(frame.mode).toBe("strip");
    expect(frame.rows[4]).toContain("Resume");
    expect(frame.hitRegions.find(({ id }) => id === "action:resume"))
      .toMatchObject({ enabled: true, rect: { y1: 5, y2: 5 } });
  });

  it("reclamps an oversized master offset so a shrunken projection never renders blank", () => {
    const frame = renderFabricConsoleFrame(
      richDataset(),
      controllerState(),
      createFabricUiState({ scrollOffsetByView: { attention: 999 } }),
      { columns: 80, rows: 24 },
    );

    expect(frame.rows.join("\n")).toContain("Approve quarantine recovery");
    expect(frame.hitRegions.some(
      ({ id }) => id === "row:attention:attention:safety",
    )).toBe(true);
  });

  it("allocates every mandatory 80x24 header field before clipping its value", () => {
    const frame = renderFabricConsoleFrame(
      datasetWithHeader({
        project: "project-".repeat(20) as ProjectId,
        session: "session-".repeat(20) as ProjectSessionId,
        run: "run-".repeat(20) as RunProjection["runId"],
        phase: "phase-".repeat(20),
        owner: "owner-".repeat(20) as AgentId,
        nextMilestone: "next-".repeat(30),
      }),
      controllerState(),
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );
    const [identity = "", lifecycle = "", next = ""] = frame.rows;

    expect(identity.slice(0, 18)).toMatch(/^P:.*~$/u);
    expect(identity.slice(19, 35)).toMatch(/^S:.*~$/u);
    expect(identity.slice(36, 50)).toMatch(/^R:.*~$/u);
    expect(identity.slice(51, 72)).toMatch(/^r9007199254740991/u);
    expect(identity.slice(73, 80)).toBe("LIVE   ");
    expect([identity[18], identity[35], identity[50], identity[72]])
      .toStrictEqual(["|", "|", "|", "|"]);

    expect(lifecycle.slice(0, 25)).toMatch(/^Phase:.*~$/u);
    expect(lifecycle.slice(26, 45)).toMatch(/^Owner:.*~$/u);
    expect(lifecycle.slice(46, 64)).toMatch(/^Health:/u);
    expect(lifecycle.slice(65, 72)).toBe("Attn:1 ");
    expect(lifecycle.slice(73, 80)).toMatch(/^Runs:/u);
    expect([lifecycle[25], lifecycle[45], lifecycle[64], lifecycle[72]])
      .toStrictEqual(["|", "|", "|", "|"]);

    expect(next.slice(0, 52)).toMatch(/^Next:.*~$/u);
    expect(next.slice(53, 80)).toMatch(/^Capacity:/u);
    expect(next[52]).toBe("|");
  });

  it("keeps every responsive hit region visible, bounded, and non-overlapping", () => {
    const dataset = richDataset();
    const state = controllerState();
    const ui = createFabricUiState();
    const viewports = [
      { columns: 140, rows: 36 },
      { columns: 80, rows: 24 },
      { columns: 60, rows: 18 },
      { columns: 30, rows: 6 },
      { columns: 8, rows: 1 },
      { columns: 0, rows: 0 },
    ] as const;

    for (const viewport of viewports) {
      const frame = renderFabricConsoleFrame(dataset, state, ui, viewport);
      for (const region of frame.hitRegions) {
        expect(region.rect.x1).toBeGreaterThanOrEqual(1);
        expect(region.rect.y1).toBeGreaterThanOrEqual(1);
        expect(region.rect.x2).toBeLessThanOrEqual(viewport.columns);
        expect(region.rect.y2).toBeLessThanOrEqual(viewport.rows);
        const visible = frame.rows
          .slice(region.rect.y1 - 1, region.rect.y2)
          .map((line) => line.slice(region.rect.x1 - 1, region.rect.x2))
          .join("\n");
        expect(visible.trim(), `${frame.mode}:${region.id}`).not.toBe("");
      }
      for (const [index, region] of frame.hitRegions.entries()) {
        for (const other of frame.hitRegions.slice(index + 1)) {
          const overlaps =
            region.rect.x1 <= other.rect.x2 &&
            other.rect.x1 <= region.rect.x2 &&
            region.rect.y1 <= other.rect.y2 &&
            other.rect.y1 <= region.rect.y2;
          expect(overlaps, `${frame.mode}:${region.id}:${other.id}`).toBe(false);
        }
      }
    }
  });

  it("composes wide Deck attention and roster columns around a cell-bound divider", () => {
    const dataset = richDataset();
    const attention = dataset.pages.attention.rows[0];
    if (attention?.summary?.kind !== "attention") {
      throw new Error("attention fixture unavailable");
    }
    const wideDataset: FabricConsoleDataset = {
      ...dataset,
      pages: {
        ...dataset.pages,
        attention: {
          ...dataset.pages.attention,
          rows: [{
            ...attention,
            summary: {
              ...attention.summary,
              title: `👩‍💻 ${"界漢".repeat(30)} 🧑🏽‍🚀`,
            },
          }, ...dataset.pages.attention.rows.slice(1)],
        },
      },
    };
    const frame = renderFabricConsoleFrame(
      wideDataset,
      controllerState(),
      createFabricUiState({ focusId: "splitter:master-detail", splitterRatio: 0.45 }),
      { columns: 140, rows: 36 },
    );
    const master = frame.hitRegions.find(({ id }) => id === "row:attention:attention:safety");
    const roster = frame.hitRegions.find(({ id }) =>
      id === "deck:coordination:session-1:AFAB-004"
    );
    const detail = frame.hitRegions.find(({ id }) => id === "detail:attention:attention:safety");

    expect(master).toBeDefined();
    expect(roster).toBeDefined();
    expect(detail).toBeDefined();
    if (master === undefined || roster === undefined || detail === undefined) return;
    expect(frame.rows.every((row) => cellWidth(row) === 140)).toBe(true);
    expect(master.rect.x2).toBe(54);
    expect(roster.rect.x1).toBe(56);
    expect(detail.rect.x1).toBe(56);
    expect(cellAt(frame.rows[master.rect.y1 - 1] ?? "", 55)).toBe("|");
    expect(frame.rows.join("\n")).toContain("界");
    expect(frame.rows.join("\n")).toContain("漢");
  });

  it("retains the generic wide CJK master/detail splitter composition", () => {
    const dataset = richDataset();
    const run = dataset.pages.runs.rows[0];
    if (run?.summary?.kind !== "run") throw new Error("run fixture unavailable");
    const wideDataset: FabricConsoleDataset = {
      ...dataset,
      pages: {
        ...dataset.pages,
        runs: {
          ...dataset.pages.runs,
          rows: [{
            ...run,
            summary: {
              ...run.summary,
              projectSessionId: `👩‍💻 ${"界漢".repeat(30)} 🧑🏽‍🚀` as never,
            },
          }],
        },
      },
    };
    const frame = renderFabricConsoleFrame(
      wideDataset,
      runControllerState(),
      createFabricUiState({ focusId: "splitter:master-detail", splitterRatio: 0.45 }),
      { columns: 140, rows: 36 },
    );
    const splitter = frame.hitRegions.find(({ id }) => id === "splitter:master-detail");
    const master = frame.hitRegions.find(({ id }) => id === "row:runs:AFAB-004");
    const detail = frame.hitRegions.find(({ id }) => id === "detail:runs:AFAB-004");

    expect(splitter).toBeDefined();
    expect(master).toBeDefined();
    expect(detail).toBeDefined();
    if (splitter === undefined || master === undefined || detail === undefined) return;
    expect(frame.rows.every((row) => cellWidth(row) === 140)).toBe(true);
    expect(cellAt(frame.rows[splitter.rect.y1 - 1] ?? "", splitter.rect.x1)).toBe(">");
    expect(master.rect.x2 + 1).toBe(splitter.rect.x1);
    expect(splitter.rect.x2 + 1).toBe(detail.rect.x1);
    expect(frame.rows.join("\n")).toContain("界");
    expect(frame.rows.join("\n")).toContain("漢");
  });

  it("blanks a whole wide grapheme when fixed-cell replacement intersects it", () => {
    const source = "A界B👩‍💻C ";
    const cjkIntersection = writeFixedCells(source, 3, 1, "|");
    const emojiIntersection = writeFixedCells(source, 6, 1, "|");

    expect(cellWidth(cjkIntersection)).toBe(cellWidth(source));
    expect(cellAt(cjkIntersection, 2)).toBe(" ");
    expect(cellAt(cjkIntersection, 3)).toBe("|");
    expect(cellWidth(emojiIntersection)).toBe(cellWidth(source));
    expect(cellAt(emojiIntersection, 5)).toBe(" ");
    expect(cellAt(emojiIntersection, 6)).toBe("|");
    expect(cjkIntersection).not.toContain("界");
    expect(emojiIntersection).not.toContain("👩‍💻");
  });

  it("terminal-neutralises hostile projected chrome in the canonical renderer", () => {
    const frame = renderFabricConsoleFrame(
      datasetWithHeader({
        project: "p\u001b" as ProjectId,
        session: "s\u009b" as ProjectSessionId,
        run: "r\u202e" as RunProjection["runId"],
        phase: "ph\u2066",
        owner: "o\u0007" as AgentId,
        nextMilestone: "n\u007f",
      }),
      controllerState(),
      createFabricUiState(),
      { columns: 140, rows: 36 },
    );
    const output = frame.rows.join("\n");

    expect(output).not.toContain("\u001b");
    expect(output).not.toContain("\u009b");
    expect(output).not.toContain("\u202e");
    expect(output).not.toContain("\u2066");
    expect(output).toContain("<ESC>");
    expect(output).toContain("<C1-9B>");
    expect(output).toContain("<BIDI-U+202E>");
    expect(output).toContain("<BIDI-U+2066>");
    expect(output).toContain("<BEL>");
    expect(output).toContain("<DEL>");
  });

  it("retains the authoritative top attention item in strip mode from every view", () => {
    const state = { ...controllerState(), activeView: "system" as const };
    const frame = renderFabricConsoleFrame(
      richDataset(),
      state,
      createFabricUiState(),
      { columns: 30, rows: 6 },
    );
    expect(frame.mode).toBe("strip");
    expect(frame.presentation.activeView).toBe("system");
    expect(frame.presentation.topAttention?.stableId).toBe("attention:safety");
    expect(frame.rows[1]).toContain("Approve quarantine");
  });

  it("makes every height below six inert even when the detach label fits", () => {
    const frame = renderFabricConsoleFrame(
      richDataset(),
      controllerState(),
      createFabricUiState(),
      { columns: 30, rows: 3 },
    );

    expect(frame.mode).toBe("inert");
    expect(frame.rows[0]).toContain("q detach");
    expect(frame.rows.join("\n")).not.toContain("Approve quarantine");
    expect(frame.hitRegions.map(({ id }) => id)).toStrictEqual(["detach"]);
  });

  it("uses narrow tall strip rows for identity, operating state, and selected work", () => {
    const frame = renderFabricConsoleFrame(
      richDataset(),
      controllerState(),
      createFabricUiState({ focusId: "row:attention:attention:safety" }),
      { columns: 30, rows: 24 },
    );
    const visible = frame.rows.join("\n");

    expect(frame.mode).toBe("strip");
    expect(visible).toContain("Project:project-1");
    expect(visible).toContain("Session:session-1");
    expect(visible).toContain("Run:AFAB-004");
    expect(visible).toContain("Revision:r11");
    expect(visible).toContain("Fresh:LIVE");
    expect(visible).toContain("Phase:implement");
    expect(visible).toContain("Owner:codex-chair");
    expect(visible).toContain("Next:Console GREEN");
    expect(visible).toContain("Health:blocked");
    expect(visible).toContain(">!! Approve quarantine");
    expect(frame.rows.filter((line) => line.trim().length > 0).length).toBeGreaterThanOrEqual(12);
    expect(frame.hitRegions.find(({ id }) => id === "detach")).toMatchObject({
      rect: { y1: 24, y2: 24 },
      enabled: true,
    });
  });

  it("withholds strip confirmation at widths 30 and 39 until exact context fits", () => {
    const dataset = richDataset();
    const state = controllerState(review("confirm"));
    const width30 = renderFabricConsoleFrame(
      dataset,
      state,
      createFabricUiState({ focusId: "review:confirm" }),
      { columns: 30, rows: 8 },
    );
    const width39 = renderFabricConsoleFrame(
      dataset,
      state,
      createFabricUiState({ focusId: "review:confirm" }),
      { columns: 39, rows: 8 },
    );
    const reference = renderFabricConsoleFrame(
      dataset,
      state,
      createFabricUiState({ focusId: "review:confirm" }),
      { columns: 80, rows: 24 },
    );

    for (const frame of [width30, width39]) {
      expect(frame.rows.join("\n")).toContain("REVIEW CONFIRM");
      expect(frame.hitRegions.some(({ id }) => id === "review:confirm"))
        .toBe(false);
      expect(frame.hitRegions.some(
        ({ kind }) => kind === "row" || kind === "tab" || kind === "splitter",
      )).toBe(false);
    }
    const visibleReference = reference.rows.join("\n");
    expect(visibleReference).toContain("Evidence:");
    expect(visibleReference).toContain("Question: Resume quarantined task?");
    expect(visibleReference).toContain("Reason: Replacement evidence passed.");
    expect(visibleReference).toContain("Recommendation: approve");
    expect(visibleReference).toContain(`Preview:${digestA}`);
    expect(visibleReference).toContain(`Intent:${digestB}`);
    expect(visibleReference).toContain(`Before:${digestA}`);
    expect(visibleReference).toContain("Confirmation: explicit");
    expect(reference.hitRegions.find(({ id }) => id === "review:confirm"))
      .toMatchObject({
      kind: "action",
      enabled: true,
    });
  });

  it("reports exact review coverage without inventing a percentage", () => {
    const frame = renderFabricConsoleFrame(
      richDataset(),
      controllerState(review()),
      createFabricUiState(),
      { columns: 30, rows: 8 },
    );
    const visible = frame.rows.join("\n");

    expect(visible).toMatch(/C\d+\/\d+/u);
    expect(visible).not.toContain("%");
  });

  it("counts every workflow intent line before enabling review continuation", () => {
    const workflow: ConsoleWorkflowReview = {
      workflowId: "workflow-intent-visibility",
      kind: "project-session-transition",
      source: "daemon-preview",
      stage: "review",
      previewDigest: "sha256:preview",
      expectedRevision: revisionFromProtocol(11),
      consequenceClass: "consequential",
      confirmationMode: "explicit",
      summary: "Transition the exact session",
      details: [
        { label: "Session", value: "session-1" },
        { label: "Expected revision", value: "11" },
      ],
      evidence: ["evidence/session-transition.json"],
      openedByEventId: "event-workflow-open",
      armedByEventId: null,
      result: null,
      failure: null,
    };
    const short = renderFabricConsoleFrame(
      richDataset(),
      controllerState(),
      createFabricUiState({ workflowReview: workflow }),
      { columns: 200, rows: 12 },
    );
    const complete = renderFabricConsoleFrame(
      richDataset(),
      controllerState(),
      createFabricUiState({ workflowReview: workflow }),
      { columns: 200, rows: 18 },
    );

    expect(short.rows.join("\n")).not.toContain("Intent Expected revision:11");
    expect(short.hitRegions.some(({ id }) => id === "review:continue"))
      .toBe(false);
    expect(complete.rows.join("\n")).toContain("Intent Expected revision:11");
    expect(complete.hitRegions.find(({ id }) => id === "review:continue"))
      .toMatchObject({ enabled: true });
  });

  it("keeps unresolved Launch custody observable and not closable", () => {
    const workflow: ConsoleWorkflowReview = {
      workflowId: "workflow-launch-unresolved",
      kind: "operator-action",
      source: "daemon-preview",
      stage: "ambiguous",
      previewDigest: "sha256:preview",
      expectedRevision: revisionFromProtocol(11),
      consequenceClass: "consequential",
      confirmationMode: "explicit",
      summary: "project-session-launch recovery",
      details: [{ label: "commandId", value: "console_launch_command" }],
      evidence: [],
      openedByEventId: "event-launch-recovery",
      armedByEventId: null,
      result: "operator-action | console_launch_command | ambiguous",
      failure: "LAUNCH_AMBIGUOUS",
    };
    const frame = renderFabricConsoleFrame(
      richDataset(),
      controllerState(),
      createFabricUiState({ workflowReview: workflow }),
      { columns: 200, rows: 18 },
    );

    expect(frame.hitRegions.find(({ id }) => id === "review:observe"))
      .toMatchObject({ enabled: true });
    expect(frame.hitRegions.some(({ id }) => id === "review:close")).toBe(false);
  });

  it.each(["editor", "guided", "palette", "filter"] as const)(
    "renders honest %s modal help with explicit input focus and local Detach authority",
    (inputMode) => {
      const frame = renderFabricConsoleFrame(
        richDataset(),
        controllerState(),
        createFabricUiState({
          inputMode,
          draft: "q? remains draft",
          filterDraft: "q? remains filter",
          mouseCapture: true,
        }),
        { columns: 80, rows: 24 },
      );
      const visible = frame.rows.join("\n");

      expect(visible).toContain("Esc");
      expect(visible).toContain("Ctrl-C");
      expect(visible).toContain("Detach");
      if (inputMode === "filter") {
        expect(visible).toContain("Enter applies view");
        expect(visible).not.toContain("Enter reviews");
      }
      expect(visible).not.toContain("? help");
      expect(visible).not.toContain("q detach");
      expect(frame.hitRegions.map(({ id }) => id)).toStrictEqual([
        `input:${inputMode}`,
        "detach",
      ]);
    },
  );

  it.each([
    { columns: 30, rows: 6 },
    { columns: 80, rows: 24 },
  ] as const)(
    "renders a redacted draft tail, cursor, byte count, and exact input hit region at $columns x $rows",
    (viewport) => {
      const secret = "afop_SUPERSECRET123456";
      const draft = `${secret} command-suffix`;
      const frame = renderFabricConsoleFrame(
        richDataset(),
        controllerState(),
        createFabricUiState({
          inputMode: "palette",
          focusId: "input:palette",
          draft,
        }),
        viewport,
      );
      const input = frame.hitRegions.find(({ id }) => id === "input:palette");
      expect(input).toBeDefined();
      if (input === undefined) return;
      const renderedInput = frame.rows
        .slice(input.rect.y1 - 1, input.rect.y2)
        .map((line) => line.slice(input.rect.x1 - 1, input.rect.x2))
        .join("\n");
      const visible = frame.rows.join("\n");

      expect(visible).not.toContain(secret);
      expect(renderedInput).toContain("suffix");
      expect(renderedInput).toContain("▏");
      expect(renderedInput).toContain(`${String(Buffer.byteLength(draft))}B`);
      expect(input).toMatchObject({
        enabled: true,
        rect: { x1: 1, y1: viewport.rows, x2: viewport.columns - 9, y2: viewport.rows },
      });
    },
  );

  it("makes a full-size review modal pointer-local and removes underlying hit geometry", () => {
    const frame = renderFabricConsoleFrame(
      richDataset(),
      controllerState(review()),
      createFabricUiState({ mouseCapture: true }),
      { columns: 80, rows: 24 },
    );
    const ids = frame.hitRegions.map(({ id }) => id);

    expect(ids).toContain("review:scroll");
    expect(ids).toContain("review:continue");
    expect(ids).toContain("review:cancel");
    expect(ids).toContain("detach");
    expect(frame.hitRegions.some(({ kind }) => kind === "row" || kind === "tab" || kind === "splitter")).toBe(false);
    expect(ids.some((id) => id.startsWith("action:") || id.startsWith("view:"))).toBe(false);
  });

  it("exposes inert detach geometry only when its label is visible", () => {
    const dataset = richDataset();
    const state = controllerState();
    const ui = createFabricUiState();
    const visible = renderFabricConsoleFrame(dataset, state, ui, {
      columns: 8,
      rows: 1,
    });
    const clipped = renderFabricConsoleFrame(dataset, state, ui, {
      columns: 7,
      rows: 1,
    });

    expect(visible).toMatchObject({ mode: "inert", rows: ["q detach"] });
    expect(visible.hitRegions).toStrictEqual([
      {
        id: "detach",
        kind: "detach",
        rect: { x1: 1, y1: 1, x2: 8, y2: 1 },
        enabled: false,
        geometryKey: visible.geometryKey,
        binding: null,
      },
    ]);
    expect(clipped.rows[0]?.trim()).toBe("");
    expect(clipped.hitRegions).toStrictEqual([]);
  });

  it("binds row and action hit geometry to item and projection revisions", () => {
    const dataset = controllableRunDataset();
    const frame = renderFabricConsoleFrame(
      dataset,
      runControllerState(),
      createFabricUiState(),
      { columns: 80, rows: 24 },
    );
    const rowRegion = frame.hitRegions.find(
      ({ id }) => id === "row:runs:AFAB-004",
    );
    const actionRegion = frame.hitRegions.find(({ id }) => id === "action:resume");

    expect(rowRegion).toMatchObject({
      enabled: true,
      binding: {
        view: "runs",
        itemId: "AFAB-004",
        itemRevision: "7",
        projectionRevision: "11",
      },
    });
    expect(actionRegion).toMatchObject({
      enabled: true,
      binding: rowRegion?.binding,
    });
    expect(frame.geometryKey).toContain("80x24:r11");
  });

  it("invalidates pointer activation after resize or revision change", () => {
    const dataset = controllableRunDataset();
    const frame = renderFabricConsoleFrame(
      dataset,
      runControllerState(),
      createFabricUiState({ mouseCapture: true }),
      { columns: 80, rows: 24 },
    );
    const region = frame.hitRegions.find(({ id }) => id === "action:resume");
    expect(region).toBeDefined();
    if (region === undefined) return;
    const x = region.rect.x1;
    const y = region.rect.y1;
    const initial: FabricPointerState = { pressed: null };
    const pressed = reduceFabricPointer(
      initial,
      { kind: "mouse", phase: "press", button: "left", x, y, modifiers: { shift: false, alt: false, ctrl: false } },
      frame,
      dataset,
    );
    const resized = renderFabricConsoleFrame(
      dataset,
      runControllerState(),
      createFabricUiState({ mouseCapture: true }),
      { columns: 120, rows: 30 },
    );
    const resizedAction = resized.hitRegions.find(({ id }) => id === "action:resume");
    expect(resizedAction).toBeDefined();
    if (resizedAction === undefined) return;

    const afterResize = reduceFabricPointer(
      pressed.state,
      {
        kind: "mouse",
        phase: "release",
        button: "left",
        x: resizedAction.rect.x1,
        y: resizedAction.rect.y1,
        modifiers: { shift: false, alt: false, ctrl: false },
      },
      resized,
      dataset,
    );
    expect(afterResize.intents).toStrictEqual([]);

    const currentPress = reduceFabricPointer(
      initial,
      { kind: "mouse", phase: "press", button: "left", x, y, modifiers: { shift: false, alt: false, ctrl: false } },
      frame,
      dataset,
    );
    const changed = controllableRunDataset(12);
    const afterRevision = reduceFabricPointer(
      currentPress.state,
      { kind: "mouse", phase: "release", button: "left", x, y, modifiers: { shift: false, alt: false, ctrl: false } },
      frame,
      changed,
    );
    expect(afterRevision.intents).toStrictEqual([]);
  });
});

describe("declared run progress presentation", () => {
  function runsDataset(
    declaredProgress: NonNullable<
      import("@local/agent-fabric-protocol").OperatorViewSummaryMap["runs"]["declaredProgress"]
    >,
  ): { dataset: FabricConsoleDataset; controller: ConsoleControllerState } {
    const dataset = richDataset();
    const runRow = dataset.pages.runs.rows[0];
    if (runRow === undefined || runRow.summary?.kind !== "run") {
      throw new Error("run fixture unavailable");
    }
    const progressed: FabricConsoleDataset = {
      ...dataset,
      pages: {
        ...dataset.pages,
        runs: {
          ...dataset.pages.runs,
          rows: [{
            ...runRow,
            summary: { ...runRow.summary, declaredProgress },
          }],
        },
      },
    };
    const baseController = controllerState();
    return {
      dataset: progressed,
      controller: {
        ...baseController,
        activeView: "runs",
        selectionByView: {
          ...baseController.selectionByView,
          runs: { stableId: runRow.stableId, revision: runRow.revision },
        },
      },
    };
  }

  it("never renders a denominator, ratio, percentage or ETA for the open arm", () => {
    const { dataset, controller } = runsDataset({
      plan: "open",
      counts: { blocked: 0, ready: 1, active: 1, complete: 3, cancelled: 0, degraded: 0 },
    });
    const presented = presentFabricConsole(
      dataset,
      controller,
      createFabricUiState(),
      { columns: 120, rows: 32 },
    );
    expect(presented.masterRows[0]?.secondary).toContain("progress open | 3 complete");
    const serialised = JSON.stringify(presented);
    // The finite n/N arm is deferred to the plan-declaration cutover; until
    // then no rendering may fabricate a denominator from known counts.
    expect(serialised).not.toContain("3/");
    expect(serialised).not.toMatch(/\d+ ?%|percentage|\bETA\b/iu);
  });

  it("shows an open plan as known counts without a denominator, percentage or ETA", () => {
    const { dataset, controller } = runsDataset({
      plan: "open",
      counts: { blocked: 1, ready: 0, active: 2, complete: 4, cancelled: 0, degraded: 0 },
    });
    const presented = presentFabricConsole(
      dataset,
      controller,
      createFabricUiState(),
      { columns: 120, rows: 32 },
    );
    expect(presented.masterRows[0]?.secondary).toContain("progress open | 4 complete");
    expect(presented.detail?.lines).toContainEqual({
      label: "Progress",
      value: "open plan | 4 complete | active 2 | ready 0 | blocked 1 | degraded 0 | cancelled 0 | no declared total",
    });
    const serialised = JSON.stringify(presented);
    expect(serialised).not.toMatch(/\d+ ?%|percentage|\bETA\b/iu);
    expect(serialised).not.toContain("4/");
  });

  it("shows an unknown plan by reason without fabricating counts", () => {
    const { dataset, controller } = runsDataset({
      plan: "unknown",
      reason: "unrecognised task state: parked",
    });
    const presented = presentFabricConsole(
      dataset,
      controller,
      createFabricUiState(),
      { columns: 120, rows: 32 },
    );
    expect(presented.masterRows[0]?.secondary).toContain("progress unknown");
    expect(presented.detail?.lines).toContainEqual({
      label: "Progress",
      value: "unknown | unrecognised task state: parked",
    });
    expect(JSON.stringify(presented)).not.toMatch(/\d+ ?%|percentage|\bETA\b/iu);
  });

  it("shows finite progress as completed over the declared total and exact plan revision", () => {
    const { dataset, controller } = runsDataset({
      plan: "finite",
      planRevision: 3,
      counts: { blocked: 0, ready: 1, active: 1, complete: 4, cancelled: 1, degraded: 0 },
      declaredTaskDenominator: 8,
    });
    const presented = presentFabricConsole(
      dataset,
      controller,
      createFabricUiState(),
      { columns: 120, rows: 32 },
    );
    expect(presented.masterRows[0]?.secondary).toContain("4/8 (plan r3)");
    expect(presented.detail?.lines).toContainEqual({
      label: "Progress",
      value: "4/8 (plan r3) | active 1 | ready 1 | blocked 0 | degraded 0 | cancelled 1",
    });
    expect(JSON.stringify(presented)).not.toMatch(/\d+ ?%|percentage|\bETA\b/iu);
  });
});

describe("run identity presentation", () => {
  function identityDataset(
    identity: import("@local/agent-fabric-protocol").RunIdentity,
  ): { dataset: FabricConsoleDataset; controller: ConsoleControllerState } {
    const dataset = richDataset();
    const runRow = dataset.pages.runs.rows[0];
    if (runRow === undefined || runRow.summary?.kind !== "run") {
      throw new Error("run fixture unavailable");
    }
    const identified: FabricConsoleDataset = {
      ...dataset,
      pages: {
        ...dataset.pages,
        runs: {
          ...dataset.pages.runs,
          rows: [{
            ...runRow,
            summary: { ...runRow.summary, identity },
          }],
        },
      },
    };
    const baseController = controllerState();
    return {
      dataset: identified,
      controller: {
        ...baseController,
        activeView: "runs",
        selectionByView: {
          ...baseController.selectionByView,
          runs: { stableId: runRow.stableId, revision: runRow.revision },
        },
      },
    };
  }

  it("declares the coordination run kind, lead and last event from Fabric facts only", () => {
    const { dataset, controller } = identityDataset({
      runKind: "coordination",
      chairAgentId: "agent-chair" as never,
      acceptedScopeRef: null,
      currentPlanRef: null,
      planRevision: null,
      workstreams: [],
      lastEventAt: null,
    });
    const presented = presentFabricConsole(
      dataset,
      controller,
      createFabricUiState(),
      { columns: 120, rows: 32 },
    );
    expect(presented.masterRows[0]?.secondary).toContain("coordination");
    expect(presented.detail?.lines).toContainEqual({ label: "Run kind", value: "coordination" });
    expect(presented.detail?.lines).toContainEqual({ label: "Lead", value: "agent-chair" });
    expect(presented.detail?.lines).toContainEqual({ label: "Last event", value: "none recorded" });
    expect(presented.detail?.lines).toContainEqual({ label: "Workstreams", value: "none recorded" });
    expect(presented.detail?.lines).toContainEqual({ label: "Accepted scope", value: "not projected" });
    expect(presented.detail?.lines).toContainEqual({ label: "Current plan", value: "not projected" });
    expect(presented.detail?.lines).toContainEqual({ label: "Plan revision", value: "not projected" });
  });

  it("renders the exact accepted-scope and current-plan refs supplied by the projection", () => {
    const { dataset, controller } = identityDataset({
      runKind: "coordination",
      chairAgentId: "agent-chair" as never,
      acceptedScopeRef: { path: "scope/accepted.md" as never, digest: `sha256:${"a".repeat(64)}` as never },
      currentPlanRef: { path: "plans/current.md" as never, digest: `sha256:${"b".repeat(64)}` as never },
      planRevision: 3,
      workstreams: [],
      lastEventAt: timestamp,
    });
    const presented = presentFabricConsole(
      dataset,
      controller,
      createFabricUiState(),
      { columns: 120, rows: 32 },
    );
    expect(presented.detail?.lines).toContainEqual({
      label: "Accepted scope",
      value: `scope/accepted.md@sha256:${"a".repeat(64)}`,
    });
    expect(presented.detail?.lines).toContainEqual({
      label: "Current plan",
      value: `plans/current.md@sha256:${"b".repeat(64)}`,
    });
    expect(presented.detail?.lines).toContainEqual({ label: "Plan revision", value: "3" });
  });

  it("groups delivery workstreams under their parent coordination run without flattening", () => {
    const { dataset, controller } = identityDataset({
      runKind: "coordination",
      chairAgentId: "agent-chair" as never,
      acceptedScopeRef: null,
      currentPlanRef: null,
      planRevision: null,
      workstreams: [
        {
          workstreamId: "ws-console" as never,
          deliveryRunId: "delivery-console" as never,
          leadAgentId: "agent-console-lead" as never,
          state: "active",
          updatedAt: timestamp,
        },
        {
          workstreamId: "ws-docs" as never,
          deliveryRunId: "delivery-docs" as never,
          leadAgentId: "agent-docs-lead" as never,
          state: "complete",
          updatedAt: timestamp,
        },
      ],
      lastEventAt: timestamp,
    });
    const presented = presentFabricConsole(
      dataset,
      controller,
      createFabricUiState(),
      { columns: 120, rows: 32 },
    );
    // The parent coordination run stays one row; its delivery workstreams
    // remain an explicit child group with their own stable identities.
    expect(presented.masterRows).toHaveLength(1);
    expect(presented.masterRows[0]?.secondary).toContain("coordination | 2 workstreams");
    expect(presented.detail?.lines).toContainEqual({
      label: "Workstream ws-console",
      value: `delivery delivery-console | lead agent-console-lead | active | updated ${timestamp}`,
    });
    expect(presented.detail?.lines).toContainEqual({
      label: "Workstream ws-docs",
      value: `delivery delivery-docs | lead agent-docs-lead | complete | updated ${timestamp}`,
    });
  });
});
