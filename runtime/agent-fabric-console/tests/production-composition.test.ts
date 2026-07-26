import { describe, expect, it, vi } from "vitest";

import type {
  OperatorActionPreview,
  OperatorCapabilityCredential,
  OperatorProjectionSnapshot,
  ProjectId,
  ProjectSessionId,
  Sha256Digest,
  Timestamp,
  ChairBridgeRecoveryIntent,
} from "@local/agent-fabric-protocol";

import {
  createProductionConsoleActionPlanner,
  createProductionConsoleBootstrap,
} from "../src/production-composition.js";
import type { ConsoleControllerState } from "../src/controller.js";
import { createEmptyViewPages, revisionFromProtocol } from "../src/model.js";
import type { FabricConsoleDataset } from "../src/protocol-adapter.js";
import type { FabricRuntimeActivation } from "../src/runtime.js";

const credential = {
  capabilityId: "capability_console_production",
  token: "secret-never-render",
} as OperatorCapabilityCredential;
const projectId = "project_console_production" as ProjectId;
const projectSessionId = "session_console_production" as ProjectSessionId;
const observedAt = "2026-07-12T00:00:00.000Z" as Timestamp;
const digest = (`sha256:${"a".repeat(64)}`) as Sha256Digest;

function dataset(): FabricConsoleDataset {
  const pages = createEmptyViewPages();
  const snapshot: OperatorProjectionSnapshot = {
    schemaVersion: 1,
    snapshotRevision: 11,
    readTransactionId: "read_console_production",
    project: {
      freshness: "live",
      source: "fabric",
      revision: 1,
      observedAt,
      value: { projectId, canonicalRoot: "/repo" },
    },
    session: {
      freshness: "live",
      source: "fabric",
      revision: 8,
      observedAt,
      value: {
        projectSessionId,
        projectId,
        mode: "coordinated",
        state: "active",
        revision: 8,
        generation: 3,
        authorityRef: digest,
        budgetRef: "budget_console_production",
        launchPacketRef: { path: "launch/packet.json" as never, digest },
        membershipRevision: 2,
        origin: { kind: "operator-launch", operatorId: "operator_console_production" as never },
      },
    },
    runs: {
      freshness: "live",
      source: "fabric",
      revision: 4,
      observedAt,
      value: [],
    },
    attention: {
      freshness: "live",
      source: "fabric",
      revision: 0,
      observedAt,
      value: [],
    },
    capacity: {
      freshness: "live",
      source: "fabric",
      revision: 11,
      observedAt,
      value: {},
    },
    cursor: 0,
    stateDigest: digest,
  };
  return {
    connection: { state: "live", compatibility: { mode: "current" } },
    snapshot,
    snapshotRevision: revisionFromProtocol(11),
    cursor: 0,
    loadedAtMs: Date.parse(observedAt),
    canMutate: true,
    pages: {
      ...pages,
      runs: {
        view: "runs",
        rows: [{
          view: "runs",
          stableId: "run_console_production",
          revision: revisionFromProtocol(4),
          urgency: "normal",
          freshness: {
            state: "live",
            source: "fabric",
            revision: revisionFromProtocol(4),
            observedAt,
            ageMs: 0,
          },
          summary: {
            kind: "run",
            phase: "active",
            health: "healthy",
            nextMilestone: "verification",
            declaredProgress: { plan: "open", counts: { blocked: 0, ready: 0, active: 1, complete: 0, cancelled: 0, degraded: 0 } },
            identity: {
              runKind: "coordination",
              chairAgentId: "agent_production_chair" as never,
              acceptedScopeRef: null,
              currentPlanRef: null,
              planRevision: null,
              workstreams: [],
              lastEventAt: observedAt,
            },
          },
          detailRef: {
            kind: "run",
            coordinationRunId: "run_console_production" as never,
            expectedRevision: 4,
          },
          actionAvailability: {
            state: "available",
            actions: ["pause", "resume", "cancel", "steer"],
            requiresPreview: true,
          },
        }],
        nextCursor: 1,
        hasMore: false,
        snapshotRevision: revisionFromProtocol(11),
        readTransactionId: "page_runs_console_production",
      },
    },
  };
}

function state(): ConsoleControllerState {
  const selection = Object.fromEntries(
    ["attention", "project", "runs", "work", "agents", "evidence", "activity", "system"]
      .map((view) => [view, null]),
  ) as ConsoleControllerState["selectionByView"];
  return {
    activeView: "runs",
    selectionByView: {
      ...selection,
      runs: { stableId: "run_console_production", revision: revisionFromProtocol(4) },
    },
    scrollAnchorByView: Object.fromEntries(
      ["attention", "project", "runs", "work", "agents", "evidence", "activity", "system"]
        .map((view) => [view, null]),
    ) as ConsoleControllerState["scrollAnchorByView"],
    review: null,
    pendingCommandIds: [],
    lastActionStatus: null,
    lastReceipt: null,
    lastFailure: null,
  };
}

function activation(action: string, eventId: string): FabricRuntimeActivation {
  return {
    regionId: `action:${action}`,
    provenance: "keyboard",
    eventId,
    binding: {
      view: "runs",
      itemId: "run_console_production",
      itemRevision: revisionFromProtocol(4),
      projectionRevision: revisionFromProtocol(11),
    },
  };
}

describe("production Console mutation planner", () => {
  it("refuses raw Pause and Resume capability leakage outside a state-bound Runs row", async () => {
    const planner = createProductionConsoleActionPlanner({
      credential,
      operatorId: "operator_console_production" as never,
      clientId: "console_client_production" as never,
    });
    const value = dataset();
    const run = value.pages.runs.rows[0];
    if (run === undefined) throw new Error("run fixture unavailable");
    const attentionId = "attention_control_leak";
    const leaked: FabricConsoleDataset = {
      ...value,
      pages: {
        ...value.pages,
        attention: {
          view: "attention",
          rows: [{
            ...run,
            view: "attention",
            stableId: attentionId,
            summary: {
              kind: "attention",
              label: "Decision",
              priority: "critical-path",
              title: "Inspect the run before control",
              nativeNotification: {
                kind: "feature-unavailable",
                status: "unavailable",
                reason: "feature-not-negotiated",
              },
            },
            actionAvailability: {
              state: "available",
              actions: ["pause", "resume"],
              requiresPreview: true,
            },
          } as never],
          nextCursor: 1,
          hasMore: false,
          snapshotRevision: revisionFromProtocol(11),
          readTransactionId: "attention_control_leak",
        },
      },
    };
    const controller = state();
    const attentionState: ConsoleControllerState = {
      ...controller,
      activeView: "attention",
      selectionByView: {
        ...controller.selectionByView,
        attention: { stableId: attentionId, revision: revisionFromProtocol(4) },
      },
    };

    for (const action of ["pause", "resume"] as const) {
      await expect(planner.plan({
        activation: {
          regionId: `action:${action}`,
          provenance: "keyboard",
          eventId: `input_${action}_attention`,
          binding: {
            view: "attention",
            itemId: attentionId,
            itemRevision: revisionFromProtocol(4),
            projectionRevision: revisionFromProtocol(11),
          },
        },
        dataset: leaked,
        state: attentionState,
        draft: "",
      })).resolves.toBeNull();
    }
  });

  it("binds a typed action to exact row, target revision and direct-input provenance", async () => {
    const planner = createProductionConsoleActionPlanner({
      credential,
      operatorId: "operator_console_production" as never,
      clientId: "console_client_production" as never,
    });

    const request = await planner.plan({
      activation: activation("resume", "input_resume_01"),
      dataset: dataset(),
      state: state(),
      draft: "this is not parsed as a command",
    });

    expect(request).toMatchObject({
      view: "runs",
      itemId: "run_console_production",
      itemRevision: "4",
      projectionRevision: "11",
      availableAction: "resume",
      intent: {
        kind: "control",
        action: "resume",
        target: {
          kind: "run",
          projectSessionId,
          coordinationRunId: "run_console_production",
          expectedRevision: 4,
        },
      },
      command: {
        credential,
        expectedRevision: 4,
        actor: "operator_console_production",
        provenance: {
          kind: "console-direct-input",
          clientId: "console_client_production",
          inputEventId: "input_resume_01",
        },
        evidenceRefs: [],
      },
    });
    expect(request?.command.commandId).toMatch(/^console_[a-f0-9]{48}$/u);
  });

  it("uses draft text only as the typed steer/cancel payload and refuses unsupported actions", async () => {
    const planner = createProductionConsoleActionPlanner({
      credential,
      operatorId: "operator_console_production" as never,
      clientId: "console_client_production" as never,
    });
    const steer = await planner.plan({
      activation: activation("steer", "input_steer_01"),
      dataset: dataset(),
      state: state(),
      draft: "Keep the exact public contract.",
    });
    expect(steer?.intent).toMatchObject({
      kind: "control",
      action: "steer",
      instruction: "Keep the exact public contract.",
      evidenceRefs: [],
    });
    await expect(planner.plan({
      activation: activation("steer", "input_steer_empty"),
      dataset: dataset(),
      state: state(),
      draft: "   ",
    })).resolves.toBeNull();
    await expect(planner.plan({
      activation: activation("git", "input_git_01"),
      dataset: dataset(),
      state: state(),
      draft: "push --force",
    })).resolves.toBeNull();
  });

  it("plans effect-free prelaunch cancellation only from the exact live Project row", async () => {
    const planner = createProductionConsoleActionPlanner({
      credential,
      operatorId: "operator_console_production" as never,
      clientId: "console_client_production" as never,
    });
    const projectFixture = (
      sessionState: "draft" | "awaiting_launch" | "active",
      stale = false,
    ): { value: FabricConsoleDataset; controller: ConsoleControllerState; activation: FabricRuntimeActivation } => {
      const current = dataset();
      const snapshot = current.snapshot;
      if (snapshot?.session.freshness !== "live" || snapshot.session.value === null) {
        throw new Error("live session fixture unavailable");
      }
      const runRow = current.pages.runs.rows[0];
      if (runRow === undefined) throw new Error("run fixture unavailable");
      const value = {
        ...current,
        snapshot: {
          ...snapshot,
          session: {
            ...snapshot.session,
            value: { ...snapshot.session.value, state: sessionState },
          },
        },
        pages: {
          ...current.pages,
          project: {
            view: "project" as const,
            rows: [{
              view: "project" as const,
              stableId: projectId,
              revision: revisionFromProtocol(1),
              urgency: "normal" as const,
              freshness: { ...runRow.freshness, state: stale ? "stale" as const : "live" as const },
              summary: {
                kind: "project" as const,
                goal: "cancel unused prelaunch session",
                acceptedScopeRef: null,
                repositoryRevision: "head",
              },
              detailRef: { kind: "project" as const, projectId, expectedRevision: 1 },
              actionAvailability: {
                state: "available" as const,
                actions: ["cancel" as const],
                requiresPreview: true,
              },
            }],
            nextCursor: 1,
            hasMore: false,
            snapshotRevision: revisionFromProtocol(11),
            readTransactionId: "project_cancel",
          },
        },
      } satisfies FabricConsoleDataset;
      const controller = state();
      return {
        value,
        controller: {
          ...controller,
          activeView: "project",
          selectionByView: {
            ...controller.selectionByView,
            project: { stableId: projectId, revision: revisionFromProtocol(1) },
          },
        },
        activation: {
          regionId: "action:cancel",
          provenance: "keyboard",
          eventId: `cancel_${sessionState}`,
          binding: {
            view: "project",
            itemId: projectId,
            itemRevision: revisionFromProtocol(1),
            projectionRevision: revisionFromProtocol(11),
          },
        },
      };
    };

    for (const sessionState of ["draft", "awaiting_launch"] as const) {
      const fixture = projectFixture(sessionState);
      await expect(planner.plan({
        activation: fixture.activation,
        dataset: fixture.value,
        state: fixture.controller,
        draft: "cancel unused session",
      })).resolves.toMatchObject({
        availableAction: "cancel",
        intent: {
          kind: "control",
          action: "cancel",
          target: {
            kind: "session",
            projectSessionId,
            expectedRevision: 8,
            expectedGeneration: 3,
          },
        },
        command: { expectedRevision: 8 },
      });
    }

    for (const fixture of [projectFixture("active"), projectFixture("draft", true)]) {
      await expect(planner.plan({
        activation: fixture.activation,
        dataset: fixture.value,
        state: fixture.controller,
        draft: "cancel unused session",
      })).resolves.toBeNull();
    }
  });

  it("builds confirmation and reconciliation commands from the bound preview revision", async () => {
    const planner = createProductionConsoleActionPlanner({
      credential,
      operatorId: "operator_console_production" as never,
      clientId: "console_client_production" as never,
    });
    const planned = await planner.plan({
      activation: activation("pause", "input_pause_01"),
      dataset: dataset(),
      state: state(),
      draft: "",
    });
    if (planned === null) throw new Error("pause should be planned");
    const preview: OperatorActionPreview = {
      previewId: "preview_console_production",
      previewRevision: 1,
      previewDigest: digest,
      intent: planned.intent,
      intentDigest: (`sha256:${"b".repeat(64)}`) as Sha256Digest,
      beforeStateDigest: (`sha256:${"c".repeat(64)}`) as Sha256Digest,
      consequenceClass: "consequential",
      evidenceRefs: [],
      gateIds: [],
      confirmationMode: "explicit",
      expiresAt: "2099-01-01T00:00:00.000Z" as Timestamp,
    };
    const reviewing: ConsoleControllerState = {
      ...state(),
      review: {
        stage: "confirm",
        binding: {
          view: "runs",
          itemId: "run_console_production",
          itemRevision: revisionFromProtocol(4),
          projectionRevision: revisionFromProtocol(11),
        },
        availableAction: "pause",
        preview,
        gates: [],
        openedByEventId: "input_pause_01",
        armedByEventId: "input_pause_02",
        changes: [],
        status: null,
      },
    };
    const confirmation = await planner.confirmation({
      activation: activation("pause", "input_pause_03"),
      dataset: dataset(),
      state: reviewing,
      draft: "",
    });
    expect(confirmation.command).toMatchObject({
      expectedRevision: 4,
      provenance: { inputEventId: "input_pause_03" },
    });
    expect(confirmation.command.commandId).not.toBe(planned.command.commandId);

    const reconcile = await planner.reconcile?.({
      targetCommandId: confirmation.command.commandId,
      activation: activation("pause", "input_pause_observe_01"),
      dataset: dataset(),
      state: reviewing,
    });
    expect(reconcile).toMatchObject({
      expectedRevision: 4,
      provenance: { inputEventId: "input_pause_observe_01" },
    });
  });

  it("binds session drain to session generation, session revision and global revision", async () => {
    const planner = createProductionConsoleActionPlanner({
      credential,
      operatorId: "operator_console_production" as never,
      clientId: "console_client_production" as never,
    });
    const current = dataset();
    const run = current.pages.runs.rows[0];
    if (run === undefined) throw new Error("run fixture is unavailable");
    const draining: FabricConsoleDataset = {
      ...current,
      pages: {
        ...current.pages,
        runs: {
          ...current.pages.runs,
          rows: [{
            ...run,
            actionAvailability: {
              state: "available",
              actions: ["project-session-drain"],
              requiresPreview: true,
            },
          }],
        },
      },
    };

    const request = await planner.plan({
      activation: activation("project-session-drain", "input_drain_01"),
      dataset: draining,
      state: state(),
      draft: "ignored",
    });

    expect(request).toMatchObject({
      availableAction: "project-session-drain",
      intent: {
        kind: "project-session-drain",
        projectSessionId,
        expectedSessionRevision: 8,
        expectedSessionGeneration: 3,
        expectedGlobalStateRevision: 11,
      },
      command: { expectedRevision: 8 },
    });
  });

  it("plans every action retained by the production availability boundary", async () => {
    const planner = createProductionConsoleActionPlanner({
      credential,
      operatorId: "operator_console_production" as never,
      clientId: "console_client_production" as never,
    });
    const current = dataset();
    const run = current.pages.runs.rows[0];
    if (run === undefined) throw new Error("run fixture is unavailable");
    const supported = [
      "pause",
      "resume",
      "cancel",
      "steer",
      "project-session-drain",
      "project-session-stop",
    ] as const;
    const available: FabricConsoleDataset = {
      ...current,
      pages: {
        ...current.pages,
        runs: {
          ...current.pages.runs,
          rows: [{
            ...run,
            actionAvailability: {
              state: "available",
              actions: supported,
              requiresPreview: true,
            },
          }],
        },
      },
    };

    for (const action of supported) {
      await expect(planner.plan({
        activation: activation(action, `input_${action}`),
        dataset: available,
        state: state(),
        draft: action === "project-session-stop"
          ? `receipts/drain.json@${digest}`
          : "exact operator payload",
      })).resolves.toMatchObject({ availableAction: action });
    }
  });

  it("plans the server-authored lost-chair abandon from the recovery project row", async () => {
    const recoveryIntent: Extract<ChairBridgeRecoveryIntent, { path: "abandon" }> = {
      kind: "chair-bridge-recovery",
      schemaVersion: 1,
      path: "abandon",
      projectSessionId,
      coordinationRunId: "run_console_production" as never,
      lossId: "loss_console_production",
      recoveryManifestDigest: digest,
      expectedSessionRevision: 8,
      expectedSessionGeneration: 3,
      expectedRunRevision: 4,
      expectedChairGeneration: 2,
      expectedPrincipalGeneration: 1,
      expectedBridgeRevision: 5,
      expectedLostBridgeGeneration: 1,
      expectedProviderSessionGeneration: 2,
      providerAdapterId: "claude-agent-sdk",
      providerContractDigest: digest,
      reason: "operator confirmed terminal retained-chair loss",
    };
    const planner = createProductionConsoleActionPlanner({
      credential,
      operatorId: "operator_console_production" as never,
      clientId: "console_client_production" as never,
      chairRecoveryIntent: recoveryIntent,
    });
    const current = dataset();
    const snapshot = current.snapshot;
    if (snapshot?.session.freshness !== "live" || snapshot.session.value === null) {
      throw new Error("live session fixture unavailable");
    }
    const recovery = {
      ...current,
      snapshot: {
        ...snapshot,
        session: {
          ...snapshot.session,
          value: { ...snapshot.session.value, state: "recovery_required" as const },
        },
      },
      pages: {
        ...current.pages,
        project: {
          view: "project" as const,
          rows: [{
            view: "project" as const,
            stableId: projectId,
            revision: revisionFromProtocol(1),
            urgency: "critical-path" as const,
            freshness: current.pages.runs.rows[0]!.freshness,
            summary: {
              kind: "project" as const,
              goal: "recover retained chair",
              acceptedScopeRef: null,
              repositoryRevision: "head",
            },
            detailRef: { kind: "project" as const, projectId, expectedRevision: 1 },
            actionAvailability: {
              state: "available" as const,
              actions: ["chair-bridge-recovery" as const],
              requiresPreview: true,
            },
          }],
          nextCursor: 1,
          hasMore: false,
          snapshotRevision: revisionFromProtocol(11),
          readTransactionId: "project_recovery",
        },
      },
    } satisfies FabricConsoleDataset;
    const controller = state();
    const projectState: ConsoleControllerState = {
      ...controller,
      activeView: "project",
      selectionByView: {
        ...controller.selectionByView,
        project: { stableId: projectId, revision: revisionFromProtocol(1) },
      },
    };

    await expect(planner.plan({
      activation: {
        regionId: "action:chair-bridge-recovery",
        provenance: "keyboard",
        eventId: "recover_chair",
        binding: {
          view: "project",
          itemId: projectId,
          itemRevision: revisionFromProtocol(1),
          projectionRevision: revisionFromProtocol(11),
        },
      },
      dataset: recovery,
      state: projectState,
      draft: "ignored caller text",
    })).resolves.toMatchObject({
      availableAction: "chair-bridge-recovery",
      intent: recoveryIntent,
      command: { expectedRevision: 5 },
    });
  });
});

describe("production Console package-root bootstrap", () => {
  it("maps the public Fabric session into protocol binding, planner and idempotent close", async () => {
    const detach = async () => {};
    const close = async () => {};
    const operatorId = "operator_console_production" as never;
    const clientId = "console_client_production" as never;
    const client = {
      kind: "operator",
      features: [
        "operator-projection.v1",
        "operator-projection.v2",
        "scoped-gate-read.v1",
        "native-notification-projection.v1",
        "run-session-projection.v1",
        "declared-run-progress.v2",
        "run-identity-projection.v2",
        "activity-narrative-grouping.v1",
        "agent-topology-projection.v1",
        "work-facts-projection.v1",
        "run-scoped-projection.v1",
        "artifact-content-read.v1",
      ],
      projection: {
        snapshot: async () => dataset().snapshot,
        events: async () => ({
          status: "continuation",
          events: [],
          nextCursor: 0,
          hasMore: false,
          snapshotRevision: 11,
          readTransactionId: "events_console_production",
        }),
      },
      console: {
        readOnly: false,
        launchAvailable: false,
        actions: {
          preview: async () => { throw new Error("not called"); },
          commit: async () => { throw new Error("not called"); },
          status: async () => { throw new Error("not called"); },
          reconcile: async () => { throw new Error("not called"); },
        },
        gates: { read: async () => { throw new Error("not called"); } },
        projection: {
          viewPage: async () => { throw new Error("not called"); },
          runPage: async () => { throw new Error("not called"); },
          readDetail: async () => { throw new Error("not called"); },
        },
      },
      artifacts: { readContent: async () => { throw new Error("not called"); } },
      operations: {},
      close,
    };
    const typedEntryPlannerFactory = vi.fn(() => ({
      capabilities: {
        launch: { state: "unavailable" as const, reason: "launch-preparation-unavailable" },
        git: { state: "unavailable" as const, reason: "git-preparation-unavailable" },
        promotion: { state: "available" as const },
      },
      buildIntent: vi.fn(),
    }));
    const bootstrap = createProductionConsoleBootstrap({
      typedEntryPlannerFactory,
      loadFabric: async () => ({
        openLocalOperatorConsoleSession: async () => ({
          client,
          compatibility: { mode: "current" },
          credential,
          projectId,
          projectSessionId,
          operatorId,
          clientId,
          daemonPid: 123,
          detach,
          close,
        }),
      }),
    });

    const connected = await bootstrap.startOrAttach({
      projectRoot: "/repo",
      surface: "standalone",
    });

    expect(connected).toMatchObject({
      status: "connected",
      credential,
      projectId,
      projectSessionId,
      binding: { ok: true, readOnly: false },
      actionPlanner: expect.objectContaining({
        plan: expect.any(Function),
        confirmation: expect.any(Function),
      }),
      workflowPlanner: expect.objectContaining({
        capabilities: expect.objectContaining({ promotion: { state: "available" } }),
        prepare: expect.any(Function),
        arm: expect.any(Function),
        commit: expect.any(Function),
      }),
    });
    expect(typedEntryPlannerFactory).toHaveBeenCalledWith({
      client,
      credential,
      projectId,
      operatorId: "operator_console_production",
      clientId: "console_client_production",
    });
  });

  it("removes server-enabled actions that the production planner cannot bind exactly", async () => {
    const close = async () => {};
    const bootstrap = createProductionConsoleBootstrap({
      loadFabric: async () => ({
        openLocalOperatorConsoleSession: async () => ({
          client: {
            kind: "operator",
            features: [
              "operator-projection.v1",
              "operator-projection.v2",
              "scoped-gate-read.v1",
              "native-notification-projection.v1",
              "run-session-projection.v1",
              "declared-run-progress.v2",
              "run-identity-projection.v2",
              "activity-narrative-grouping.v1",
              "agent-topology-projection.v1",
              "work-facts-projection.v1",
              "run-scoped-projection.v1",
              "artifact-content-read.v1",
            ],
            projection: {
              snapshot: async () => dataset().snapshot,
              events: async () => { throw new Error("unused"); },
            },
            console: {
              readOnly: false,
              launchAvailable: false,
              actions: {
                preview: async () => { throw new Error("unused"); },
                commit: async () => { throw new Error("unused"); },
                status: async () => { throw new Error("unused"); },
                reconcile: async () => { throw new Error("unused"); },
              },
              gates: { read: async () => { throw new Error("unused"); } },
              projection: {
                viewPage: async () => ({
                  status: "page",
                  view: "runs",
                  rows: [{
                    itemId: "run_console_production",
                    itemRevision: 4,
                    fact: {
                      freshness: "live",
                      source: "fabric",
                      revision: 4,
                      observedAt,
                      value: {
                        summary: {
                          kind: "run",
                          phase: "active",
                          health: "healthy",
                          nextMilestone: "verification",
                          declaredProgress: { plan: "open", counts: { blocked: 0, ready: 0, active: 1, complete: 0, cancelled: 0, degraded: 0 } },
                          identity: {
                            runKind: "coordination",
                            chairAgentId: "agent_production_chair",
                            workstreams: [],
                            lastEventAt: observedAt,
                          },
                        },
                        detailRef: {
                          kind: "run",
                          coordinationRunId: "run_console_production",
                          expectedRevision: 4,
                        },
                        actionAvailability: {
                          state: "available",
                          actions: [
                            "pause", "resume", "cancel", "steer",
                            "project-session-drain", "project-session-stop",
                            "daemon-drain", "daemon-stop", "git", "promotion",
                          ],
                          requiresPreview: true,
                        },
                      },
                    },
                  }],
                  nextCursor: 1,
                  hasMore: false,
                  snapshotRevision: 11,
                  readTransactionId: "filtered_actions",
                }),
                runPage: async () => { throw new Error("unused"); },
                readDetail: async () => { throw new Error("unused"); },
              },
            },
            artifacts: { readContent: async () => { throw new Error("unused"); } },
            operations: {},
            close,
          },
          compatibility: { mode: "current" },
          credential,
          projectId,
          projectSessionId,
          operatorId: "operator_console_production",
          clientId: "console_client_production",
          detach: async () => {},
          close,
        }),
      }),
    });

    const connected = await bootstrap.startOrAttach({
      projectRoot: "/repo",
      surface: "standalone",
    });
    if (connected.status !== "connected" || !connected.binding.ok) {
      throw new Error("production binding is unavailable");
    }
    const page = await connected.binding.port.viewPage({
      credential,
      projectId,
      projectSessionId,
      view: "runs",
      snapshotRevision: 11,
      cursor: 0,
      limit: 10,
    });
    if (page.status !== "page" || page.rows[0]?.fact.freshness !== "live") {
      throw new Error("filtered page is unavailable");
    }
    expect(page.rows[0].fact.value.actionAvailability).toStrictEqual({
      state: "available",
      actions: [
        "pause",
        "resume",
        "cancel",
        "steer",
      ],
      requiresPreview: true,
    });
    await connected.close();
  });

  it("rebinds the same connector between the project selector and an exact session", async () => {
    const projectCredential = {
      capabilityId: "capability_project_selector",
      token: "project-selector-secret",
    } as OperatorCapabilityCredential;
    const sessionCredential = {
      capabilityId: "capability_session_selector",
      token: "session-selector-secret",
    } as OperatorCapabilityCredential;
    const baseClient = (readOnly: boolean) => ({
      kind: "operator" as const,
      features: [
        "operator-projection.v1",
        "operator-projection.v2",
        "scoped-gate-read.v1",
        "native-notification-projection.v1",
        "run-session-projection.v1",
        "declared-run-progress.v2",
        "run-identity-projection.v2",
        "activity-narrative-grouping.v1",
        "agent-topology-projection.v1",
        "work-facts-projection.v1",
        "run-scoped-projection.v1",
        "artifact-content-read.v1",
      ],
      projection: {
        snapshot: async () => dataset().snapshot,
        events: async () => { throw new Error("unused"); },
      },
      console: {
        readOnly,
        launchAvailable: true,
        ...(readOnly ? {} : {
          actions: {
            preview: async () => { throw new Error("unused"); },
            commit: async () => { throw new Error("unused"); },
            status: async () => { throw new Error("unused"); },
            reconcile: async () => { throw new Error("unused"); },
          },
        }),
        gates: { read: async () => { throw new Error("unused"); } },
        projection: {
          viewPage: async () => { throw new Error("unused"); },
          runPage: async () => { throw new Error("unused"); },
          readDetail: async () => { throw new Error("unused"); },
        },
      },
      artifacts: { readContent: async () => { throw new Error("unused"); } },
      operations: {},
      close: async () => {},
    });
    const projectClient = baseClient(true);
    const selectedClient = baseClient(false);
    let activeClient = projectClient;
    let activeCredential = projectCredential;
    let selectedProjectSessionId: ProjectSessionId | undefined;
    const connector = {
      get client() { return activeClient; },
      compatibility: { mode: "current" as const },
      get credential() { return activeCredential; },
      projectClient,
      projectCompatibility: { mode: "current" as const },
      projectCredential,
      attachableProjectSessions: [{
        projectSessionId,
        mode: "independent" as const,
        state: "active" as const,
        revision: 4,
        generation: 1,
        lastEventAt: observedAt,
      }],
      projectId,
      operatorId: "operator_console_production",
      get projectSessionId() { return selectedProjectSessionId; },
      clientId: "console_client_production",
      async selectProjectSession(next: ProjectSessionId) {
        expect(next).toBe(projectSessionId);
        activeClient = selectedClient;
        activeCredential = sessionCredential;
        selectedProjectSessionId = next;
      },
      async selectProject() {
        activeClient = projectClient;
        activeCredential = projectCredential;
        selectedProjectSessionId = undefined;
      },
      refreshProjectSessions: async () => connector.attachableProjectSessions,
      detach: async () => {},
      close: async () => {},
    };
    const bootstrap = createProductionConsoleBootstrap({
      loadFabric: async () => ({
        openLocalOperatorConsoleSession: async () => connector,
      }),
    });

    const projectConnection = await bootstrap.startOrAttach({
      projectRoot: "/repo",
      surface: "standalone",
    });
    if (projectConnection.status !== "connected") throw new Error("project selector unavailable");
    expect(projectConnection).not.toHaveProperty("projectSessionId");
    expect(projectConnection.binding).toMatchObject({ ok: true, readOnly: true });
    expect(projectConnection.sessionSelection?.choices).toMatchObject([{
      projectSessionId,
    }]);

    const selected = await projectConnection.sessionSelection?.selectProjectSession(
      projectSessionId,
    );
    expect(selected).toMatchObject({
      status: "connected",
      credential: sessionCredential,
      projectSessionId,
      binding: { ok: true, readOnly: false },
    });
    if (selected?.status !== "connected") throw new Error("exact session unavailable");
    const returned = await selected.sessionSelection?.selectProject();
    expect(returned).toMatchObject({
      status: "connected",
      credential: projectCredential,
      binding: { ok: true, readOnly: true },
    });
    expect(returned).not.toHaveProperty("projectSessionId");
  });

  it.each([
    ["configuration-missing", "configuration-missing"],
    ["start-failed", "start-failed"],
    ["schema-cutover-required", "schema-cutover-required"],
    ["authority-unavailable", "authority-unavailable"],
  ] as const)("renders %s as an explicit read-only unavailable reason", async (reason, expected) => {
    const bootstrap = createProductionConsoleBootstrap({
      loadFabric: async () => ({
        openLocalOperatorConsoleSession: async () => {
          throw Object.assign(new Error("secret detail"), { reason });
        },
      }),
    });

    await expect(bootstrap.startOrAttach({
      projectRoot: "/repo",
      surface: "standalone",
    })).resolves.toStrictEqual({ status: "unavailable", reason: expected });
  });

  it("preserves current protocol incompatibility as a connection-level bootstrap result", async () => {
    const bootstrap = createProductionConsoleBootstrap({
      loadFabric: async () => ({
        openLocalOperatorConsoleSession: async () => {
          throw Object.assign(new Error("Console protocol incompatible"), {
            code: "CONSOLE_PROTOCOL_INCOMPATIBLE",
            primary: { code: "PROTOCOL_INVALID", message: "unknown optional feature" },
            result: {
              code: "PROTOCOL_INCOMPATIBLE",
              message: "unnegotiated notification field",
              operation: "fabric.v1.operator-projection.snapshot",
              closedReason: "unnegotiated-field",
            },
          });
        },
      }),
    });

    await expect(bootstrap.startOrAttach({
      projectRoot: "/repo",
      surface: "standalone",
    })).resolves.toStrictEqual({
      status: "protocol-incompatible",
      primary: { code: "PROTOCOL_INVALID", message: "unknown optional feature" },
      result: {
        code: "PROTOCOL_INCOMPATIBLE",
        message: "unnegotiated notification field",
        operation: "fabric.v1.operator-projection.snapshot",
        closedReason: "unnegotiated-field",
      },
    });
  });

  it("closes an attached public session when protocol binding fails", async () => {
    let closeCount = 0;
    const bootstrap = createProductionConsoleBootstrap({
      loadFabric: async () => ({
        openLocalOperatorConsoleSession: async () => ({
          client: { features: {} },
          compatibility: { mode: "current" },
          credential,
          projectId,
          operatorId: "operator_console_production",
          clientId: "console_client_production",
          detach: async () => {},
          close: async () => { closeCount += 1; },
        }),
      }),
    });

    await expect(bootstrap.startOrAttach({
      projectRoot: "/repo",
      surface: "standalone",
    })).resolves.toStrictEqual({ status: "unavailable", reason: "start-failed" });
    expect(closeCount).toBe(1);
  });

  it("rejects and closes a session that does not identify the current baseline", async () => {
    let closeCount = 0;
    const bootstrap = createProductionConsoleBootstrap({
      loadFabric: async () => ({
        openLocalOperatorConsoleSession: async () => ({
          client: {},
          compatibility: { mode: "obsolete" },
          credential,
          projectId,
          operatorId: "operator_console_production",
          clientId: "console_client_production",
          detach: async () => {},
          close: async () => { closeCount += 1; },
        }),
      }),
    });

    await expect(bootstrap.startOrAttach({
      projectRoot: "/repo",
      surface: "standalone",
    })).resolves.toStrictEqual({ status: "unavailable", reason: "start-failed" });
    expect(closeCount).toBe(1);
  });
});
