import { describe, expect, it, vi } from "vitest";

import {
  FABRIC_OPERATIONS,
  type NegotiatedOperatorClient,
  type OperatorCapabilityCredential,
  type OperatorActionPreview,
  type OperatorClientId,
  type OperatorId,
  type OperatorProjectionSnapshot,
  type ProjectId,
  type ProjectSessionLaunchPrepareRequest,
  type ProjectSessionId,
  type ScopedGateReadRequest,
  type ScopedGateReadResult,
  type ScopedGate,
  type Sha256Digest,
  type Timestamp,
} from "@local/agent-fabric-protocol";

import { createEmptyViewPages, revisionFromProtocol } from "../src/model.js";
import type { FabricConsoleDataset } from "../src/protocol-adapter.js";
import { createProductionConsoleTypedEntryPlanner } from "../src/typed-entry-planner.js";

const projectId = "project_typed_entry" as ProjectId;
const projectSessionId = "session_typed_entry" as ProjectSessionId;
const operatorId = "operator_typed_entry" as OperatorId;
const clientId = "console_typed_entry" as OperatorClientId;
const credential = {
  capabilityId: "capability_typed_entry",
  token: "afop_typed_entry_secret",
} as OperatorCapabilityCredential;
const digest = (`sha256:${"c".repeat(64)}`) as Sha256Digest;
const observedAt = "2026-07-12T00:00:00.000Z" as Timestamp;

function dataset(): FabricConsoleDataset {
  const snapshot: OperatorProjectionSnapshot = {
    schemaVersion: 1,
    snapshotRevision: 11,
    readTransactionId: "typed-entry-snapshot",
    project: {
      freshness: "live",
      source: "fabric",
      revision: 3,
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
        generation: 2,
        authorityRef: digest,
        budgetRef: "budget_typed_entry",
        launchPacketRef: { path: "launch/packet.json" as never, digest },
        membershipRevision: 1,
        origin: { kind: "operator-launch", operatorId: "operator_typed_entry" as never },
      },
    },
    runs: {
      freshness: "live",
      source: "fabric",
      revision: 2,
      observedAt,
      value: [],
    },
    attention: {
      freshness: "live",
      source: "fabric",
      revision: 1,
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
    cursor: 11,
    stateDigest: digest,
  };
  const pages = createEmptyViewPages();
  return {
    connection: { state: "live", compatibility: { mode: "current" } },
    snapshot,
    snapshotRevision: revisionFromProtocol(11),
    cursor: 11,
    pages: {
      ...pages,
      project: {
        view: "project",
        rows: [{
          view: "project",
          stableId: projectId,
          revision: revisionFromProtocol(3),
          urgency: "normal",
          freshness: {
            state: "live",
            source: "fabric",
            revision: revisionFromProtocol(3),
            observedAt,
            ageMs: 0,
          },
          summary: {
            kind: "project",
            goal: "Ship the accepted scope",
            acceptedScopeRef: null,
            repositoryRevision: "abc123",
          },
          detailRef: { kind: "project", projectId, expectedRevision: 3 },
          actionAvailability: {
            state: "available",
            actions: ["promotion"],
            requiresPreview: true,
          },
        }],
        nextCursor: 1,
        hasMore: false,
        snapshotRevision: revisionFromProtocol(11),
        readTransactionId: "typed-entry-project-page",
      },
    },
    loadedAtMs: Date.parse(observedAt),
    canMutate: true,
  };
}

function releaseGate(): ScopedGate {
  return {
    gateId: "gate_release_typed_entry" as never,
    projectSessionId,
    coordinationRunId: "run_release_typed_entry" as never,
    scope: { kind: "release" },
    affectedTaskIds: [],
    dependencyRevision: 5,
    blockedOperationIds: [FABRIC_OPERATIONS.operatorActionCommit],
    enforcementPoints: ["operation"],
    question: "Promote the accepted delivery?",
    reason: "Release authority is target-bound.",
    options: ["Promote", "Reject"],
    recommendation: "Promote",
    consequences: ["Production target changes."],
    evidenceRefs: [{ path: "receipts/accepted.json" as never, digest }],
    revision: 7,
    createdByRef: "chair:release",
    expectedApproverRef: "operator:human",
    status: "approved",
    resolution: {
      kind: "typed-console",
      operatorId: "operator_typed_entry" as never,
      decidedAt: observedAt,
      evidenceRefs: [{ path: "receipts/accepted.json" as never, digest }],
      confirmationCommandId: "command_release_typed_entry" as never,
    },
    releaseBinding: {
      acceptedDeliveryReceiptRef: { path: "receipts/accepted.json" as never, digest },
      artifactDigest: digest,
      promotionAction: "deploy-production",
      target: "production-au",
    },
  };
}

function client(
  read: (input: ScopedGateReadRequest) => Promise<ScopedGateReadResult>,
  prepareLaunch?: (input: ProjectSessionLaunchPrepareRequest) => Promise<OperatorActionPreview>,
): NegotiatedOperatorClient {
  return {
    kind: "operator",
    features: [],
    operations: {},
    ...(prepareLaunch === undefined
      ? {}
      : { projectSessions: { prepareLaunch } }),
    console: {
      readOnly: false,
      launchAvailable: true,
      actions: {
        preview: vi.fn(),
        commit: vi.fn(),
        status: vi.fn(),
        reconcile: vi.fn(),
      },
      gates: { read },
      projection: { viewPage: vi.fn(), runPage: vi.fn(), readDetail: vi.fn() },
    },
    close: async () => {},
  };
}

describe("production Console typed-entry planner", () => {
  it("prepares Launch through the dedicated daemon API from the exact live Project row", async () => {
    const launchIntent = {
      kind: "project-session-launch" as const,
      projectId,
      projectSessionId,
      expectedProjectRevision: 3,
      expectedSessionRevision: 8,
      expectedSessionGeneration: 2,
      trustRecordDigest: digest,
      launchPacketRef: { path: "launch/packet.json" as never, digest },
      authorityRef: digest,
      budgetRef: "budget_typed_entry",
      resourcePlanRef: { path: "launch/resource-plan.json" as never, digest },
      providerAdapterId: "claude-agent-sdk",
      providerActionId: "provider_launch_typed_entry" as never,
      providerContractDigest: digest,
      resourceStateDigest: digest,
    };
    const preview: OperatorActionPreview = {
      previewId: "preview_launch_typed_entry",
      previewRevision: 1,
      previewDigest: digest,
      intent: launchIntent,
      intentDigest: digest,
      beforeStateDigest: digest,
      consequenceClass: "consequential",
      evidenceRefs: [],
      gateIds: [],
      confirmationMode: "explicit",
      expiresAt: "2099-01-01T00:00:00.000Z" as Timestamp,
    };
    const prepareLaunch = vi.fn(async (
      _input: ProjectSessionLaunchPrepareRequest,
    ): Promise<OperatorActionPreview> => preview);
    const planner = createProductionConsoleTypedEntryPlanner({
      client: client(vi.fn(), prepareLaunch),
      credential,
      projectId,
      operatorId,
      clientId,
    });

    expect(planner.capabilities.launch).toStrictEqual({ state: "available" });
    const launchInput = {
      kind: "launch",
      fields: {},
      eventId: "launch-typed-entry",
      binding: {
        view: "project",
        itemId: projectId,
        itemRevision: revisionFromProtocol(3),
        projectionRevision: revisionFromProtocol(11),
      },
      dataset: dataset(),
    } as const;
    await expect(planner.buildIntent(launchInput)).resolves.toStrictEqual({
      intent: launchIntent,
      expectedRevision: 8,
      daemonPreview: preview,
    });
    expect(prepareLaunch).toHaveBeenCalledWith({
      command: expect.objectContaining({
        credential,
        expectedRevision: 8,
        actor: operatorId,
        provenance: {
          kind: "console-direct-input",
          clientId,
          inputEventId: "launch-typed-entry",
        },
      }),
      projectId,
      projectSessionId,
      expectedSessionGeneration: 2,
      launchPacketRef: { path: "launch/packet.json", digest },
    });
    await expect(planner.buildIntent(launchInput)).resolves.toStrictEqual({
      intent: launchIntent,
      expectedRevision: 8,
      daemonPreview: preview,
    });
    expect(prepareLaunch).toHaveBeenCalledTimes(2);
    expect(prepareLaunch.mock.calls[1]?.[0].command.commandId)
      .toBe(prepareLaunch.mock.calls[0]?.[0].command.commandId);
    await planner.buildIntent({ ...launchInput, eventId: "launch-after-restart" });
    expect(prepareLaunch.mock.calls[2]?.[0].command.commandId)
      .not.toBe(prepareLaunch.mock.calls[0]?.[0].command.commandId);
  });

  it("rejects caller-authored Launch fields before contacting the daemon", async () => {
    const prepareLaunch = vi.fn();
    const planner = createProductionConsoleTypedEntryPlanner({
      client: client(vi.fn(), prepareLaunch),
      credential,
      projectId,
      operatorId,
      clientId,
    });

    await expect(planner.buildIntent({
      kind: "launch",
      fields: { revision: "999" },
      eventId: "launch-forged-field",
      binding: {
        view: "project",
        itemId: projectId,
        itemRevision: revisionFromProtocol(3),
        projectionRevision: revisionFromProtocol(11),
      },
      dataset: dataset(),
    })).rejects.toThrow("accepts no fields");
    expect(prepareLaunch).not.toHaveBeenCalled();
  });

  it("builds Promotion only from the exact approved release gate", async () => {
    const gate = releaseGate();
    const read = vi.fn(async () => ({
      status: "current" as const,
      gate,
      readTransactionId: "release-gate-read",
      stateDigest: digest,
    }));
    const planner = createProductionConsoleTypedEntryPlanner({
      client: client(read),
      credential,
      projectId,
      operatorId,
      clientId,
    });
    expect(planner.capabilities).toStrictEqual({
      launch: {
        state: "unavailable",
        reason: "project-session-launch-prepare-unavailable",
      },
      git: {
        state: "unavailable",
        reason: "daemon-git-intent-preparation-unavailable",
      },
      promotion: { state: "available" },
    });

    const result = await planner.buildIntent({
      kind: "promotion",
      fields: { gate: gate.gateId },
      eventId: "promotion-typed-entry",
      binding: {
        view: "project",
        itemId: projectId,
        itemRevision: revisionFromProtocol(3),
        projectionRevision: revisionFromProtocol(11),
      },
      dataset: dataset(),
    });

    expect(read).toHaveBeenCalledWith({
      credential,
      projectId,
      projectSessionId,
      gateId: gate.gateId,
    });
    expect(result).toStrictEqual({
      expectedRevision: 7,
      intent: {
        kind: "promotion",
        projectSessionId,
        coordinationRunId: gate.coordinationRunId,
        gateId: gate.gateId,
        expectedGateRevision: 7,
        expectedGateStatus: "approved",
        releaseBinding: gate.releaseBinding,
      },
    });
  });

  it("rejects extra fields and non-release gates before Preview", async () => {
    const gate = { ...releaseGate(), scope: { kind: "run" as const } } as ScopedGate;
    const read = vi.fn(async () => ({
      status: "current" as const,
      gate,
      readTransactionId: "wrong-gate-read",
      stateDigest: digest,
    }));
    const planner = createProductionConsoleTypedEntryPlanner({
      client: client(read),
      credential,
      projectId,
      operatorId,
      clientId,
    });
    const input = {
      kind: "promotion" as const,
      eventId: "promotion-rejection",
      binding: {
        view: "project" as const,
        itemId: projectId,
        itemRevision: revisionFromProtocol(3),
        projectionRevision: revisionFromProtocol(11),
      },
      dataset: dataset(),
    };

    await expect(planner.buildIntent({
      ...input,
      fields: { gate: gate.gateId, target: "forged" },
    })).rejects.toThrow("exactly gate");
    expect(read).not.toHaveBeenCalled();
    await expect(planner.buildIntent({
      ...input,
      fields: { gate: gate.gateId },
    })).rejects.toThrow("approved release gate");
  });
});
