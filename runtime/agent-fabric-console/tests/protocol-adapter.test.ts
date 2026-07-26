import { createHash } from "node:crypto";
import { readFile, readdir } from "node:fs/promises";
import { fileURLToPath } from "node:url";

import { describe, expect, it, vi } from "vitest";

import type {
  ActivityNarrativeGroup,
  ArtifactContentReadRequest,
  ArtifactContentReadResult,
  OperatorCapabilityCredential,
  OperatorDetailReadResult,
  OperatorProjectionSnapshot,
  OperatorViewPageResult,
  MessageBodyReadResult,
  ProjectId,
  ProjectionEventsResult,
  Sha256Digest,
  Timestamp,
} from "@local/agent-fabric-protocol";
import { FABRIC_OPERATIONS } from "@local/agent-fabric-protocol";
import {
  ConsoleProtocolAdapter,
  bindConsoleProtocolClient,
  createBootstrapUnavailableDataset,
  type BootstrapUnavailableReason,
  type ConsoleProtocolBinding,
  type ConsoleProtocolPort,
} from "../src/protocol-adapter.js";
import { FABRIC_VIEWS, revisionFromProtocol } from "../src/model.js";

const projectId = "project-1" as ProjectId;
const credential = {
  capabilityId: "capability-1",
  token: "secret-never-render",
} as OperatorCapabilityCredential;
const observedAt = "2026-07-11T12:00:00.000Z" as Timestamp;
const digest = (`sha256:${"a".repeat(64)}`) as Sha256Digest;

function snapshot(revision: number, cursor = revision): OperatorProjectionSnapshot {
  return {
    schemaVersion: 1,
    snapshotRevision: revision,
    readTransactionId: `read-${String(revision)}`,
    project: {
      freshness: "live",
      source: "fabric",
      revision,
      observedAt,
      value: { projectId, canonicalRoot: "/repo" },
    },
    session: {
      freshness: "unavailable",
      source: "fabric",
      revision,
      observedAt,
      reason: "no session selected",
    },
    runs: {
      freshness: "live",
      source: "fabric",
      revision,
      observedAt,
      value: [],
    },
    attention: {
      freshness: "live",
      source: "fabric",
      revision,
      observedAt,
      value: [],
    },
    capacity: {
      freshness: "unavailable",
      source: "fabric",
      revision,
      observedAt,
      reason: "not declared",
    },
    cursor,
    stateDigest: digest,
  };
}

function emptyPage(
  view: (typeof FABRIC_VIEWS)[number],
  revision: number,
  cursor = 0,
  hasMore = false,
): OperatorViewPageResult {
  return {
    status: "page",
    view,
    rows: [],
    nextCursor: cursor,
    hasMore,
    snapshotRevision: revision,
    readTransactionId: `page-${view}-${String(cursor)}`,
  } as OperatorViewPageResult;
}

function fakePort(
  overrides: Partial<ConsoleProtocolPort> = {},
): ConsoleProtocolPort {
  return {
    snapshot: vi.fn(async () => snapshot(1)),
    events: vi.fn(async (): Promise<ProjectionEventsResult> => ({
      status: "continuation",
      events: [],
      nextCursor: 1,
      hasMore: false,
      snapshotRevision: 1,
      readTransactionId: "events-1",
    })),
    viewPage: vi.fn(async (request) =>
      emptyPage(request.view, request.snapshotRevision),
    ),
    readDetail: vi.fn(async (): Promise<OperatorDetailReadResult> => ({
      status: "resnapshot-required",
      reason: "snapshot-mismatch",
      currentSnapshotRevision: 2,
    })),
    readGate: vi.fn(async () => {
      throw new Error("unused gate read");
    }),
    readMessageBody: null,
    readRepository: null,
    readArtifactContent: null,
    ...overrides,
  };
}

function binding(port: ConsoleProtocolPort): ConsoleProtocolBinding {
  return {
    ok: true,
    port,
    readOnly: true,
    actions: null,
    nativeNotificationProjection: "daemon-journal",
    runSessionProjection: "exact",
    compatibility: { mode: "current" },
  };
}

describe("public protocol adapter", () => {
  it("maps a current Attention page without the optional notification feature to unavailable", async () => {
    const port = fakePort({
      viewPage: vi.fn(async (request): Promise<OperatorViewPageResult> => {
        if (request.view !== "attention") {
          return emptyPage(request.view, request.snapshotRevision);
        }
        return {
          status: "page",
          view: "attention",
          rows: [{
            itemId: "attention-optional",
            itemRevision: 1,
            fact: {
              freshness: "live",
              source: "fabric",
              revision: 1,
              observedAt,
              value: {
                summary: {
                  kind: "attention",
                  label: "FYI",
                  priority: "advisory",
                  title: "Optional notification state",
                },
                detailRef: { kind: "system", componentId: "native", expectedRevision: 1 },
                actionAvailability: { state: "read-only", reason: "state-ineligible" },
              },
            },
          }],
          nextCursor: 1,
          hasMore: false,
          snapshotRevision: request.snapshotRevision,
          readTransactionId: "optional-page",
        };
      }),
    });
    const adapter = new ConsoleProtocolAdapter({
      binding: {
        ok: true,
        port,
        readOnly: true,
        actions: null,
        nativeNotificationProjection: "feature-unavailable",
        runSessionProjection: "exact",
        compatibility: { mode: "current" },
      },
      credential,
      projectId,
    });

    const loaded = await adapter.open();
    expect(loaded.connection).toStrictEqual({
      state: "live",
      compatibility: { mode: "current" },
    });
    expect(loaded.pages.attention.rows[0]?.summary).toMatchObject({
      nativeNotification: {
        kind: "feature-unavailable",
        status: "unavailable",
        reason: "feature-not-negotiated",
      },
    });
  });

  it("loads one canonical snapshot and all eight cursor-paged views", async () => {
    const calls: Array<{ view: string; cursor: number; revision: number }> = [];
    const port = fakePort({
      viewPage: vi.fn(async (request) => {
        calls.push({
          view: request.view,
          cursor: request.cursor,
          revision: request.snapshotRevision,
        });
        if (request.view === "attention" && request.cursor === 0) {
          return {
            status: "page",
            view: "attention",
            rows: [
              {
                itemId: "gate:safety",
                itemRevision: 7,
                fact: {
                  freshness: "live",
                  source: "fabric",
                  revision: 7,
                  observedAt,
                  value: {
                    summary: {
                      kind: "attention",
                      label: "Approval",
                      priority: "safety-integrity",
                      title: "Approve quarantine recovery",
                      nativeNotification: {
                        targetIntegration: "native-desktop",
                        status: "available",
                        journalState: "sent",
                        deliveryItemRevision: 7,
                        claimGeneration: null,
                        integrationState: "available",
                        observedAt,
                      },
                    },
                    detailRef: {
                      kind: "system",
                      componentId: "quarantine",
                      expectedRevision: 7,
                    },
                    actionAvailability: {
                      state: "available",
                      actions: ["resume"],
                      requiresPreview: true,
                    },
                  },
                },
              },
            ],
            nextCursor: 1,
            hasMore: true,
            snapshotRevision: request.snapshotRevision,
            readTransactionId: "attention-page-1",
          } as unknown as OperatorViewPageResult;
        }
        return emptyPage(
          request.view,
          request.snapshotRevision,
          request.cursor,
        );
      }),
    });
    const adapter = new ConsoleProtocolAdapter({
      binding: binding(port),
      credential,
      projectId,
      now: () => Date.parse(observedAt) + 1_000,
      pageLimit: 25,
    });

    const result = await adapter.open();

    expect(result.connection).toStrictEqual({
      state: "live",
      compatibility: { mode: "current" },
    });
    expect(result.snapshot?.snapshotRevision).toBe(1);
    expect(result.snapshotRevision).toBe("1");
    expect(result.cursor).toBe(1);
    expect(Object.keys(result.pages)).toStrictEqual(FABRIC_VIEWS);
    expect(result.pages.attention.rows[0]).toMatchObject({
      stableId: "gate:safety",
      revision: "7",
      urgency: "safety-integrity",
    });
    expect(calls).toHaveLength(9);
    expect(calls).toContainEqual({ view: "attention", cursor: 1, revision: 1 });
    expect(calls.every(({ revision }) => revision === 1)).toBe(true);
  });

  it("loads exact closed review, topology, and context reads without substituting summary fields", async () => {
    const completionRead = vi.fn(async () => ({
      schemaVersion: 1,
      code: "NOT_FOUND",
      currentRevision: null,
      evidenceDigest: null,
    } as never));
    const evidenceList = vi.fn(async () => ({
      schemaVersion: 1,
      entries: [],
      nextCursor: null,
    } as never));
    const topologyRead = vi.fn(async () => ({
      schemaVersion: 1,
      currency: "unavailable",
      plan: null,
      pointer: null,
    } as never));
    const contextRead = vi.fn(async () => ({
      schemaVersion: 1,
      currency: "unavailable",
      pressure: null,
      readAt: observedAt,
      ageSeconds: null,
    } as never));
    const port = fakePort({
      reviewCompletionRead: completionRead,
      reviewEvidenceList: evidenceList,
      topologyWaveCurrentRead: topologyRead,
      providerContextPressureRead: contextRead,
      viewPage: vi.fn(async (request): Promise<OperatorViewPageResult> => {
        const common = {
          itemRevision: 7,
          fact: {
            freshness: "live" as const,
            source: "fabric" as const,
            revision: 7,
            observedAt,
          },
        };
        if (request.view === "runs") {
          return {
            status: "page",
            view: "runs",
            rows: [{
              ...common,
              itemId: "run-1",
              fact: {
                ...common.fact,
                value: {
                  summary: {
                    kind: "run",
                    projectSessionId: "session-1",
                    phase: "implement",
                    health: "healthy",
                    nextMilestone: "review",
                    declaredProgress: { plan: "open", counts: { blocked: 0, ready: 0, active: 1, complete: 0, cancelled: 0, degraded: 0 } },
                    identity: {
                      runKind: "coordination",
                      chairAgentId: "agent-chair",
                      workstreams: [],
                      lastEventAt: observedAt,
                    },
                  },
                  detailRef: {
                    kind: "run",
                    projectSessionId: "session-1",
                    coordinationRunId: "run-1",
                    expectedRevision: 7,
                  },
                  actionAvailability: { state: "read-only", reason: "state-ineligible" },
                },
              },
            }],
            nextCursor: 1,
            hasMore: false,
            snapshotRevision: request.snapshotRevision,
            readTransactionId: "runs-closed",
          } as unknown as OperatorViewPageResult;
        }
        if (request.view === "work") {
          return {
            status: "page",
            view: "work",
            rows: [{
              ...common,
              itemId: "task-1",
              fact: {
                ...common.fact,
                value: {
                  summary: { kind: "work", state: "active", checkState: "passing" },
                  detailRef: { kind: "task", taskId: "task-1", expectedRevision: 7 },
                  actionAvailability: { state: "read-only", reason: "state-ineligible" },
                },
              },
            }],
            nextCursor: 1,
            hasMore: false,
            snapshotRevision: request.snapshotRevision,
            readTransactionId: "work-closed",
          } as unknown as OperatorViewPageResult;
        }
        if (request.view === "agents") {
          return {
            status: "page",
            view: "agents",
            rows: [{
              ...common,
              itemId: "agent-1",
              fact: {
                ...common.fact,
                value: {
                  summary: { kind: "agent", role: "worker", lifecycle: "working", contextPressure: "unknown" },
                  detailRef: { kind: "agent", agentId: "agent-1", expectedRevision: 7 },
                  actionAvailability: { state: "read-only", reason: "state-ineligible" },
                },
              },
            }],
            nextCursor: 1,
            hasMore: false,
            snapshotRevision: request.snapshotRevision,
            readTransactionId: "agents-closed",
          } as unknown as OperatorViewPageResult;
        }
        return emptyPage(request.view, request.snapshotRevision);
      }),
    });
    const adapter = new ConsoleProtocolAdapter({
      binding: binding(port),
      credential,
      projectId,
    });

    const result = await adapter.open();

    expect(result.review?.reviewRuns[0]).toMatchObject({
      projectSessionId: "session-1",
      coordinationRunId: "run-1",
      preparation: {
        state: "unavailable",
        reason: "preparation-id-not-projected",
      },
      completion: { state: "unavailable", reason: "read-error", code: "NOT_FOUND" },
      evidence: { state: "current", value: [] },
      providerRoute: {
        state: "unavailable",
        reason: "operator-route-projection-unavailable",
      },
    });
    expect(result.review?.topology[0]).toMatchObject({
      taskId: "task-1",
      coordinationRunId: "run-1",
      read: { state: "current", value: { currency: "unavailable" } },
    });
    expect(result.review?.contextPressure[0]).toMatchObject({
      agentId: "agent-1",
      coordinationRunId: "run-1",
      read: { state: "current", value: { currency: "unavailable" } },
    });
    expect(completionRead).toHaveBeenCalledWith({
      schemaVersion: 1,
      projectSessionId: "session-1",
      coordinationRunId: "run-1",
    });
    expect(evidenceList).toHaveBeenCalledWith(expect.objectContaining({
      targetGeneration: null,
      slot: null,
      cursor: null,
    }));
    expect(topologyRead).toHaveBeenCalledWith(expect.objectContaining({
      coordinationRunId: "run-1",
      taskId: "task-1",
    }));
    expect(contextRead).toHaveBeenCalledWith(expect.objectContaining({
      coordinationRunId: "run-1",
      agentId: "agent-1",
    }));

    await adapter.poll();
    expect(completionRead).toHaveBeenCalledTimes(2);
    expect(evidenceList).toHaveBeenCalledTimes(2);
    expect(topologyRead).toHaveBeenCalledTimes(2);
    expect(contextRead).toHaveBeenCalledTimes(2);
  });

  it("discards a mixed projection and resnapshots as one revision", async () => {
    const snapshots = [snapshot(1), snapshot(2)];
    const port = fakePort({
      snapshot: vi.fn(async () => snapshots.shift() ?? snapshot(2)),
      viewPage: vi.fn(async (request) => {
        if (request.snapshotRevision === 1 && request.view === "work") {
          return {
            status: "resnapshot-required",
            view: "work",
            reason: "snapshot-mismatch",
            currentSnapshotRevision: 2,
            snapshotCursor: 2,
          } as OperatorViewPageResult;
        }
        return emptyPage(request.view, request.snapshotRevision);
      }),
    });
    const adapter = new ConsoleProtocolAdapter({
      binding: binding(port),
      credential,
      projectId,
    });

    const result = await adapter.open();

    expect(result.connection).toStrictEqual({
      state: "live",
      compatibility: { mode: "current" },
    });
    expect(result.snapshotRevision).toBe("2");
    expect(port.snapshot).toHaveBeenCalledTimes(2);
    expect(
      Object.values(result.pages).every(
        ({ snapshotRevision }) => snapshotRevision === "2",
      ),
    ).toBe(true);
  });

  it("preserves last-good state and disables mutation truth after an outage", async () => {
    let fail = false;
    const port = fakePort({
      snapshot: vi.fn(async () => {
        if (fail) {
          throw new Error("socket unavailable\nsecret-never-render");
        }
        return snapshot(11);
      }),
    });
    const adapter = new ConsoleProtocolAdapter({
      binding: {
        ok: true,
        port,
        readOnly: false,
        actions: {} as never,
        nativeNotificationProjection: "daemon-journal",
        runSessionProjection: "exact",
        compatibility: { mode: "current" },
      },
      credential,
      projectId,
    });
    const good = await adapter.open();
    fail = true;

    const degraded = await adapter.refresh();

    expect(good.canMutate).toBe(true);
    expect(degraded.snapshot).toBe(good.snapshot);
    expect(degraded.pages).toBe(good.pages);
    expect(degraded.canMutate).toBe(false);
    expect(degraded.connection).toMatchObject({
      state: "degraded",
      reason: "transport-failure",
    });
    expect(JSON.stringify(degraded)).not.toContain("secret-never-render");
  });

  it("turns result-shape incompatibility into a connection failure without cached rows", async () => {
    const port = fakePort();
    const adapter = new ConsoleProtocolAdapter({
      binding: binding(port),
      credential,
      projectId,
    });
    const current = await adapter.open();
    expect(current.snapshot).not.toBeNull();
    vi.mocked(port.events).mockRejectedValueOnce(Object.assign(
      new Error("incompatible projection"),
      {
        code: "PROTOCOL_INCOMPATIBLE",
        cause: {
          operation: "fabric.v1.operator-projection.snapshot",
          reason: "missing-negotiated-field",
        },
      },
    ));

    const rejected = await adapter.poll();

    expect(rejected.connection).toStrictEqual({
      state: "protocol-incompatible",
      code: "PROTOCOL_INCOMPATIBLE",
      message: "incompatible projection",
      operation: "fabric.v1.operator-projection.snapshot",
      closedReason: "missing-negotiated-field",
      primary: {
        code: "PROTOCOL_INCOMPATIBLE",
        message: "incompatible projection",
      },
    });
    expect(rejected.snapshot).toBeNull();
    expect(rejected.pages.attention.rows).toStrictEqual([]);
    expect(rejected.canMutate).toBe(false);
    expect(adapter.actionClient).toBeNull();
  });

  it("resumes from the durable cursor and reloads on opaque committed events", async () => {
    const snapshots = [snapshot(3, 30), snapshot(4, 41)];
    const port = fakePort({
      snapshot: vi.fn(async () => snapshots.shift() ?? snapshot(4, 41)),
      events: vi.fn(async (request): Promise<ProjectionEventsResult> => ({
        status: "continuation",
        events: [
          {
            cursor: 41,
            projectSessionId: "session-1" as never,
            kind: "task.changed",
            revision: 4,
            occurredAt: observedAt,
            payload: { taskId: "task-1" },
          },
        ],
        nextCursor: 41,
        hasMore: false,
        snapshotRevision: 4,
        readTransactionId: `events-after-${String(request.after)}`,
      })),
    });
    const adapter = new ConsoleProtocolAdapter({
      binding: binding(port),
      credential,
      projectId,
      eventLimit: 20,
    });
    await adapter.open();

    const refreshed = await adapter.poll();

    expect(port.events).toHaveBeenCalledWith(
      expect.objectContaining({ after: 30, limit: 20 }),
    );
    expect(refreshed.snapshotRevision).toBe("4");
    expect(refreshed.cursor).toBe(41);
    expect(port.snapshot).toHaveBeenCalledTimes(2);
  });

  it("resnapshots an eventless revision change and observes the notification transition", async () => {
    const snapshots = [snapshot(7, 70), snapshot(8, 70)];
    const pageRevision = { value: 7 };
    const port = fakePort({
      snapshot: vi.fn(async () => {
        const next = snapshots.shift() ?? snapshot(8, 70);
        pageRevision.value = next.snapshotRevision;
        return next;
      }),
      events: vi.fn(async (): Promise<ProjectionEventsResult> => ({
        status: "continuation",
        events: [],
        nextCursor: 70,
        hasMore: false,
        snapshotRevision: 8,
        readTransactionId: "events-eventless-8",
      })),
      viewPage: vi.fn(async (request): Promise<OperatorViewPageResult> => {
        if (request.view !== "attention") return emptyPage(request.view, request.snapshotRevision);
        const terminal = pageRevision.value === 8;
        return {
          status: "page",
          view: "attention",
          rows: [{
            itemId: "attention-eventless",
            itemRevision: 2,
            fact: {
              freshness: "live",
              source: "fabric",
              revision: 2,
              observedAt,
              value: {
                summary: {
                  kind: "attention",
                  label: "FYI",
                  priority: "advisory",
                  title: "Delivery transition",
                  nativeNotification: {
                    targetIntegration: "native-desktop",
                    status: "available",
                    journalState: terminal ? "sent" : "pending",
                    deliveryItemRevision: 2,
                    claimGeneration: terminal ? 2 : 1,
                    integrationState: "available",
                    observedAt,
                  },
                },
                detailRef: { kind: "system", componentId: "native", expectedRevision: 2 },
                actionAvailability: { state: "read-only", reason: "state-ineligible" },
              },
            },
          }],
          nextCursor: 1,
          hasMore: false,
          snapshotRevision: request.snapshotRevision,
          readTransactionId: `attention-${String(request.snapshotRevision)}`,
        };
      }),
    });
    const adapter = new ConsoleProtocolAdapter({
      binding: binding(port),
      credential,
      projectId,
    });

    const initial = await adapter.open();
    const refreshed = await adapter.poll();
    const initialNotification = initial.pages.attention.rows[0]?.summary?.kind === "attention"
      ? initial.pages.attention.rows[0].summary.nativeNotification
      : null;
    const refreshedNotification = refreshed.pages.attention.rows[0]?.summary?.kind === "attention"
      ? refreshed.pages.attention.rows[0].summary.nativeNotification
      : null;

    expect(initialNotification).toMatchObject({ kind: "daemon-journal", journalState: "pending" });
    expect(refreshedNotification).toMatchObject({ kind: "daemon-journal", journalState: "sent" });
    expect(port.events).toHaveBeenCalledTimes(1);
    expect(port.snapshot).toHaveBeenCalledTimes(2);
    expect(refreshed.cursor).toBe(70);
  });

  it("fails closed when the negotiated client lacks the Console feature surface", async () => {
    const negotiated = {
      kind: "operator",
      features: ["operator-projection.v1"],
      projection: {},
    } as never;
    const result = bindConsoleProtocolClient(negotiated);
    expect(result).toStrictEqual({
      ok: false,
      missingFeatures: [
        "operator-projection.v2",
        "scoped-gate-read.v1",
        "run-session-projection.v1",
        "declared-run-progress.v2",
        "run-identity-projection.v2",
        "activity-narrative-grouping.v1",
        "artifact-content-read.v1",
      ],
    });

    const adapter = new ConsoleProtocolAdapter({
      binding: result,
      credential,
      projectId,
    });
    const unavailable = await adapter.open();
    expect(unavailable).toMatchObject({
      connection: {
        state: "unsupported",
        missingFeatures: [
          "operator-projection.v2",
          "scoped-gate-read.v1",
          "run-session-projection.v1",
          "declared-run-progress.v2",
          "run-identity-projection.v2",
          "activity-narrative-grouping.v1",
          "artifact-content-read.v1",
        ],
      },
      snapshot: null,
      canMutate: false,
    });
  });

  it("rejects stale Console surfaces that omit a required negotiated feature", () => {
    const result = bindConsoleProtocolClient({
      kind: "operator",
      features: [
        "operator-projection.v1",
        "operator-projection.v2",
        "scoped-gate-read.v1",
        "artifact-content-read.v1",
      ],
      projection: {},
      console: {
        readOnly: true,
        gates: { read: vi.fn() },
        projection: { viewPage: vi.fn(), readDetail: vi.fn() },
      },
      artifacts: { readContent: vi.fn() },
    } as never);

    expect(result).toStrictEqual({
      ok: false,
      missingFeatures: [
        "run-session-projection.v1",
        "declared-run-progress.v2",
        "run-identity-projection.v2",
        "activity-narrative-grouping.v1",
      ],
    });
  });

  it("binds negotiated message and repository reads through the Console protocol port", () => {
    const messageRead = vi.fn();
    const repositoryRead = vi.fn();
    const artifactRead = vi.fn();
    const reviewCompletionRead = vi.fn();
    const reviewEvidenceList = vi.fn();
    const routeRecoveryRead = vi.fn();
    const contextPressureRead = vi.fn();
    const topologyCurrentRead = vi.fn();
    const negotiated = {
      kind: "operator",
      features: [
        "operator-projection.v1",
        "operator-projection.v2",
        "scoped-gate-read.v1",
        "run-session-projection.v1",
        "declared-run-progress.v2",
        "run-identity-projection.v2",
        "activity-narrative-grouping.v1",
        "artifact-content-read.v1",
        "message-body-read.v1",
        "operator-repository-read.v1",
      ],
      projection: {},
      operations: {
        [FABRIC_OPERATIONS.reviewCompletionRead]: reviewCompletionRead,
        [FABRIC_OPERATIONS.reviewEvidenceList]: reviewEvidenceList,
        [FABRIC_OPERATIONS.providerRouteIntegrityRecoveryRead]: routeRecoveryRead,
        [FABRIC_OPERATIONS.providerContextPressureRead]: contextPressureRead,
        [FABRIC_OPERATIONS.topologyWaveCurrentRead]: topologyCurrentRead,
      },
      console: {
        readOnly: true,
        gates: { read: vi.fn() },
        projection: { viewPage: vi.fn(), readDetail: vi.fn() },
      },
      messages: { read: messageRead },
      repository: { read: repositoryRead },
      artifacts: { readContent: artifactRead },
    } as never;

    const bound = bindConsoleProtocolClient(negotiated);

    expect(bound.ok).toBe(true);
    if (!bound.ok) return;
    expect(Reflect.get(bound.port, "readMessageBody")).toBe(messageRead);
    expect(Reflect.get(bound.port, "readRepository")).toBe(repositoryRead);
    expect(bound.port.readArtifactContent).toEqual(expect.any(Function));
    expect(bound.port.reviewCompletionRead).toBe(reviewCompletionRead);
    expect(bound.port.reviewEvidenceList).toBe(reviewEvidenceList);
    expect(bound.port.providerRouteIntegrityRecoveryRead).toBe(routeRecoveryRead);
    expect(bound.port.providerContextPressureRead).toBe(contextPressureRead);
    expect(bound.port.topologyWaveCurrentRead).toBe(topologyCurrentRead);

    const withoutOptionalReads = bindConsoleProtocolClient({
      kind: "operator",
      features: [
        "operator-projection.v1",
        "operator-projection.v2",
        "scoped-gate-read.v1",
        "run-session-projection.v1",
        "declared-run-progress.v2",
        "run-identity-projection.v2",
        "activity-narrative-grouping.v1",
        "artifact-content-read.v1",
      ],
      projection: {},
      operations: {},
      console: {
        readOnly: true,
        gates: { read: vi.fn() },
        projection: { viewPage: vi.fn(), readDetail: vi.fn() },
      },
      artifacts: { readContent: artifactRead },
    } as never);
    expect(withoutOptionalReads.ok).toBe(true);
    if (!withoutOptionalReads.ok) return;
    expect(withoutOptionalReads.port.readMessageBody).toBeNull();
    expect(withoutOptionalReads.port.readRepository).toBeNull();
    expect(withoutOptionalReads.port.readArtifactContent).toEqual(expect.any(Function));
    expect(withoutOptionalReads.port.reviewCompletionRead).toBeNull();
    expect(withoutOptionalReads.port.reviewEvidenceList).toBeNull();
    expect(withoutOptionalReads.port.providerRouteIntegrityRecoveryRead).toBeNull();
    expect(withoutOptionalReads.port.providerContextPressureRead).toBeNull();
    expect(withoutOptionalReads.port.topologyWaveCurrentRead).toBeNull();
  });

  it("fetches exact bounded artifact and diff content through the public read port", async () => {
    const pageContent = ["# Reviewed spec\n", "safe body"] as const;
    const renderedContent = pageContent.join("");
    const artifactDigest = `sha256:${"b".repeat(64)}` as Sha256Digest;
    const renderedDigest = `sha256:${createHash("sha256").update(renderedContent).digest("hex")}` as Sha256Digest;
    let tamperPageDigest = false;
    const artifactRead = vi.fn(async (
      request: ArtifactContentReadRequest,
    ): Promise<ArtifactContentReadResult> => {
      const pageIndex = request.cursor === null ? 0 : 1;
      const content = pageContent[pageIndex] ?? "";
      return {
        available: true,
        artifactRef: { path: "docs/spec.md" as never, digest: artifactDigest },
        mediaType: "text/markdown",
        content,
        totalBytes: 43,
        totalLines: 2,
        renderedTotalBytes: Buffer.byteLength(renderedContent),
        renderedTotalLines: 2,
        pageIndex,
        lineFragment: "whole",
        pageContentDigest: (tamperPageDigest
          ? `sha256:${"f".repeat(64)}`
          : `sha256:${createHash("sha256").update(content).digest("hex")}`) as Sha256Digest,
        renderedArtifactDigest: renderedDigest,
        nextCursor: pageIndex === 0 ? "cursor-page-1" : null,
        transformation: "terminal-neutralised",
        terminalNeutralised: true,
        capabilityValuesRedacted: true,
        credentialValuesRedacted: true,
      };
    });
    const port = fakePort({
      viewPage: vi.fn(async (request) => {
        if (request.view !== "evidence") {
          return emptyPage(request.view, request.snapshotRevision);
        }
        return {
          status: "page",
          view: "evidence",
          rows: [{
            itemId: "artifact-spec",
            itemRevision: 7,
            fact: {
              freshness: "live",
              source: "fabric",
              revision: 7,
              observedAt,
              value: {
                summary: {
                  kind: "evidence",
                  evidenceKind: "artifact",
                  status: "informational",
                  provenance: "fabric:chair",
                },
                detailRef: { kind: "evidence", evidenceId: "artifact-spec", expectedRevision: 7 },
                actionAvailability: { state: "read-only", reason: "state-ineligible" },
              },
            },
          }],
          nextCursor: 1,
          hasMore: false,
          snapshotRevision: request.snapshotRevision,
          readTransactionId: "evidence-page",
        } as OperatorViewPageResult;
      }),
      readDetail: vi.fn(async () => ({
        status: "current",
        detailRef: { kind: "evidence", evidenceId: "artifact-spec", expectedRevision: 7 },
        detail: {
          freshness: "live",
          source: "fabric",
          revision: 7,
          observedAt,
          value: {
            kind: "evidence",
            evidenceId: "artifact-spec",
            evidenceKind: "artifact",
            artifactRef: { path: "docs/spec.md", digest: artifactDigest },
            sourceKind: "project-file",
            publisherKind: "agent",
            publisherRef: "chair-1",
            projectSessionId: null,
            coordinationRunId: null,
            taskId: null,
            createdAt: observedAt,
            status: "informational",
          },
        },
        snapshotRevision: 1,
        readTransactionId: "evidence-detail",
      }) as never),
      readArtifactContent: artifactRead,
    });
    const adapter = new ConsoleProtocolAdapter({ binding: binding(port), credential, projectId });
    await adapter.open();

    const inspection = await adapter.inspect({
      view: "evidence",
      itemId: "artifact-spec",
      itemRevision: revisionFromProtocol(7),
      projectionRevision: revisionFromProtocol(1),
    });

    expect(inspection).toMatchObject({
      kind: "artifact",
      state: "current",
      result: {
        artifactRef: { path: "docs/spec.md", digest: artifactDigest },
        content: renderedContent,
        renderedArtifactDigest: renderedDigest,
        transformation: "terminal-neutralised",
        terminalNeutralised: true,
        capabilityValuesRedacted: true,
        credentialValuesRedacted: true,
        coverage: {
          complete: true,
          verified: true,
          pageCount: 2,
        },
        reviewDisposition: "confirm-terminal-neutralised",
      },
    });
    expect(artifactRead).toHaveBeenNthCalledWith(1, expect.objectContaining({
      credential,
      projectId,
      evidenceId: "artifact-spec",
      expectedEvidenceRevision: 7,
      artifactRef: { path: "docs/spec.md", digest: artifactDigest },
      cursor: null,
      maximumBytes: 131_072,
      maximumLines: 2_000,
    }));
    expect(artifactRead).toHaveBeenNthCalledWith(2, expect.objectContaining({
      cursor: "cursor-page-1",
    }));

    tamperPageDigest = true;
    await expect(adapter.inspect({
      view: "evidence",
      itemId: "artifact-spec",
      itemRevision: revisionFromProtocol(7),
      projectionRevision: revisionFromProtocol(1),
    })).resolves.toMatchObject({
      kind: "artifact",
      state: "unavailable",
      reason: "contract-invalid",
    });
  });

  it("discards artifact coverage when a daemon cursor repeats", async () => {
    const content = "abcdefgh";
    const contentDigest = `sha256:${createHash("sha256").update(content).digest("hex")}` as Sha256Digest;
    const pageDigest = (page: string): Sha256Digest =>
      `sha256:${createHash("sha256").update(page).digest("hex")}` as Sha256Digest;
    const artifactRead = vi.fn(async (
      request: ArtifactContentReadRequest,
    ): Promise<ArtifactContentReadResult> => {
      const first = request.cursor === null;
      const page = first ? "abcd" : "efgh";
      return {
        available: true,
        artifactRef: { path: "docs/spec.md" as never, digest: contentDigest },
        mediaType: "text/markdown",
        content: page,
        totalBytes: 8,
        totalLines: 1,
        renderedTotalBytes: 8,
        renderedTotalLines: 1,
        pageIndex: first ? 0 : 1,
        lineFragment: first ? "start" : "end",
        pageContentDigest: pageDigest(page),
        renderedArtifactDigest: contentDigest,
        nextCursor: "repeated-cursor",
        transformation: "none",
        terminalNeutralised: true,
        capabilityValuesRedacted: true,
        credentialValuesRedacted: true,
      };
    });
    const port = fakePort({
      viewPage: vi.fn(async (request) => request.view === "evidence"
        ? {
            status: "page",
            view: "evidence",
            rows: [{
              itemId: "artifact-spec",
              itemRevision: 7,
              fact: {
                freshness: "live",
                source: "fabric",
                revision: 7,
                observedAt,
                value: {
                  summary: {
                    kind: "evidence",
                    evidenceKind: "artifact",
                    status: "informational",
                    provenance: "agent:chair-1",
                  },
                  detailRef: { kind: "evidence", evidenceId: "artifact-spec", expectedRevision: 7 },
                  actionAvailability: { state: "read-only", reason: "state-ineligible" },
                },
              },
            }],
            nextCursor: 1,
            hasMore: false,
            snapshotRevision: request.snapshotRevision,
            readTransactionId: "evidence-page",
          } as OperatorViewPageResult
        : emptyPage(request.view, request.snapshotRevision)),
      readDetail: vi.fn(async () => ({
        status: "current",
        detailRef: { kind: "evidence", evidenceId: "artifact-spec", expectedRevision: 7 },
        detail: {
          freshness: "live",
          source: "fabric",
          revision: 7,
          observedAt,
          value: {
            kind: "evidence",
            evidenceId: "artifact-spec",
            evidenceKind: "artifact",
            artifactRef: { path: "docs/spec.md", digest: contentDigest },
            sourceKind: "project-file",
            publisherKind: "agent",
            publisherRef: "chair-1",
            projectSessionId: null,
            coordinationRunId: null,
            taskId: null,
            createdAt: observedAt,
            status: "informational",
          },
        },
        snapshotRevision: 1,
        readTransactionId: "evidence-detail",
      }) as OperatorDetailReadResult),
      readArtifactContent: artifactRead,
    });
    const adapter = new ConsoleProtocolAdapter({ binding: binding(port), credential, projectId });
    await adapter.open();

    await expect(adapter.inspect({
      view: "evidence",
      itemId: "artifact-spec",
      itemRevision: revisionFromProtocol(7),
      projectionRevision: revisionFromProtocol(1),
    })).resolves.toMatchObject({
      kind: "artifact",
      state: "unavailable",
      reason: "contract-invalid",
    });
    expect(artifactRead).toHaveBeenCalledTimes(2);
  });

  it("drills into every grouped member and labels unavailable message detail", async () => {
    const group: ActivityNarrativeGroup = {
      groupId: "activity_group_01",
      ordinal: 1,
      kind: "task",
      actorIds: ["agent_01"],
      target: { kind: "task", id: "task_01" },
      eventKinds: ["message-persisted", "tool-invoked"],
      occurredAtRange: { first: observedAt, last: observedAt },
      sourceRange: { first: 3, last: 4 },
      count: 2,
      evidenceLinkCount: 0,
      evidenceLinksDigest:
        "sha256:4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945" as never,
      evidenceLinksTruncated: false,
      evidenceLinks: [],
      members: [
        {
          ordinal: 1,
          eventId: "event_message",
          eventKind: "message-persisted",
          actorId: "agent_01",
          target: { kind: "task", id: "task_01" },
          occurredAt: observedAt,
          sourceRevision: 3,
          messageBodyRef: {
            projectSessionId: "session_01" as never,
            messageId: "message_01" as never,
            expectedRevision: 1,
          },
          detailAvailability: "available",
          evidenceLinkCount: 0,
          evidenceLinksDigest:
            "sha256:4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945" as never,
        },
        {
          ordinal: 2,
          eventId: "event_tool",
          eventKind: "tool-invoked",
          actorId: "agent_01",
          target: { kind: "task", id: "task_01" },
          occurredAt: observedAt,
          sourceRevision: 4,
          detailAvailability: "available",
          evidenceLinkCount: 0,
          evidenceLinksDigest:
            "sha256:4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945" as never,
        },
      ],
    };
    const toolContent = '{"tool":"read"}';
    const toolDigest =
      `sha256:${createHash("sha256").update(toolContent).digest("hex")}`;
    let memberDetailMode: "success" | "resnapshot" | "zero-progress" =
      "success";
    const readDetail = vi.fn(async (request) => (
      "eventId" in request.detailRef
        ? memberDetailMode === "resnapshot"
          ? {
              status: "resnapshot-required",
              reason: "detail-revision-changed",
              currentSnapshotRevision: 2,
            }
          : {
              status: "current",
              detailRef: request.detailRef,
              detail: {
                freshness: "live",
                source: "fabric",
                revision: 4,
                observedAt,
                value: {
                  kind: "activity",
                  groupId: group.groupId,
                  eventId: "event_tool",
                  sourceRevision: 4,
                  contentOffset: 0,
                  content: memberDetailMode === "zero-progress"
                    ? ""
                    : toolContent,
                  contentBytes: Buffer.byteLength(toolContent),
                  contentDigest: toolDigest,
                  transformation: "none",
                  nextDetailRef: memberDetailMode === "zero-progress"
                    ? request.detailRef
                    : null,
                },
              },
              snapshotRevision: 1,
              readTransactionId: "activity-member-detail",
            }
        : {
            status: "current",
            detailRef: request.detailRef,
            detail: {
              freshness: "live",
              source: "fabric",
              revision: 4,
              observedAt,
              value: {
                kind: "activity",
                group,
                memberDetails: [
                  {
                    eventId: "event_message",
                    status: "available",
                    content: '{"messageId":"message_01"}',
                    transformation: "none",
                  },
                  {
                    eventId: "event_tool",
                    status: "referenced",
                    contentBytes: Buffer.byteLength(toolContent),
                    contentDigest: toolDigest,
                    detailRef: {
                      kind: "activity",
                      groupId: group.groupId,
                      eventId: "event_tool",
                      expectedRevision: 4,
                      contentOffset: 0,
                    },
                  },
                ],
              },
            },
            snapshotRevision: 1,
            readTransactionId: "activity-group-detail",
          }
    ) as unknown as OperatorDetailReadResult);
    const port = fakePort({
      viewPage: vi.fn(async (request) => {
        if (request.view !== "activity") {
          return emptyPage(request.view, request.snapshotRevision);
        }
        return {
          status: "page",
          view: "activity",
          rows: [{
            itemId: group.groupId,
            itemRevision: 4,
            fact: {
              freshness: "live",
              source: "fabric",
              revision: 4,
              observedAt,
              value: {
                summary: {
                  kind: "activity",
                  summary: "task activity",
                  occurredAt: observedAt,
                  group,
                },
                detailRef: {
                  kind: "activity",
                  groupId: group.groupId,
                  expectedRevision: 4,
                },
                actionAvailability: {
                  state: "read-only",
                  reason: "state-ineligible",
                },
              },
            },
          }],
          nextCursor: 1,
          hasMore: false,
          snapshotRevision: request.snapshotRevision,
          readTransactionId: "activity-group-read",
        } as unknown as OperatorViewPageResult;
      }),
      readDetail,
      readMessageBody: vi.fn(async (): Promise<MessageBodyReadResult> => ({
        available: false,
        messageId: "message_01" as never,
        revision: 1,
        reason: "expired",
      })),
    });
    const adapter = new ConsoleProtocolAdapter({
      binding: binding(port),
      credential,
      projectId,
    });
    await adapter.open();

    const inspection = await adapter.inspect({
      view: "activity",
      itemId: group.groupId,
      itemRevision: revisionFromProtocol(4),
      projectionRevision: revisionFromProtocol(1),
    });

    expect(inspection).toMatchObject({
      kind: "activity",
      state: "current",
      detail: {
        group: {
          count: 2,
          sourceRange: { first: 3, last: 4 },
        },
        memberDetails: [
          { eventId: "event_message", status: "available" },
          {
            eventId: "event_tool",
            status: "available",
            content: '{"tool":"read"}',
          },
        ],
      },
      messages: [{
        eventId: "event_message",
        state: "unavailable",
        reason: "message-expired",
      }],
    });
    memberDetailMode = "resnapshot";
    await expect(adapter.inspect({
      view: "activity",
      itemId: group.groupId,
      itemRevision: revisionFromProtocol(4),
      projectionRevision: revisionFromProtocol(1),
    })).resolves.toMatchObject({
      kind: "activity",
      state: "current",
      detail: {
        memberDetails: [
          { eventId: "event_message", status: "available" },
          {
            eventId: "event_tool",
            status: "unavailable",
            reason: "projection-changed",
          },
        ],
      },
    });
    memberDetailMode = "zero-progress";
    await expect(adapter.inspect({
      view: "activity",
      itemId: group.groupId,
      itemRevision: revisionFromProtocol(4),
      projectionRevision: revisionFromProtocol(1),
    })).resolves.toMatchObject({
      kind: "activity",
      state: "current",
      detail: {
        memberDetails: [
          { eventId: "event_message", status: "available" },
          {
            eventId: "event_tool",
            status: "unavailable",
            reason: "contract-invalid",
          },
        ],
      },
    });
  });

  it("renders a truthful System detail and remediation for every bootstrap reason", () => {
    const reasons: readonly BootstrapUnavailableReason[] = [
      "feature-unavailable",
      "configuration-missing",
      "schema-cutover-required",
      "authority-unavailable",
      "daemon-unreachable",
      "daemon-incompatible",
      "socket-unavailable",
      "daemon-election-conflict",
      "daemon-spawn-failed",
      "bootstrap-receipt-invalid",
      "start-failed",
    ];
    const details = new Set<string>();
    for (const reason of reasons) {
      const dataset = createBootstrapUnavailableDataset(reason, 1_000);
      // The transport axis stays one honest label across every reason.
      expect(dataset.connection).toStrictEqual({
        state: "unavailable",
        reason: "bootstrap-unavailable",
      });
      const row = dataset.pages.system.rows[0];
      expect(row?.stableId).toBe("bootstrap");
      expect(row?.freshness).toMatchObject({ state: "unavailable", reason });
      const summary = row?.summary;
      expect(summary?.kind).toBe("system");
      expect(summary?.systemKind).toBe(
        reason === "feature-unavailable" ? "daemon" : summary?.systemKind,
      );
      expect(summary?.state).toBe("unavailable");
      expect(typeof summary?.detail).toBe("string");
      expect((summary?.detail ?? "").length).toBeGreaterThan(0);
      details.add(summary?.detail ?? "");
    }
    // Each reason yields its own distinct, non-collapsed operator detail.
    expect(details.size).toBe(reasons.length);
  });

  it("names the exact daemon finding rather than a generic start-failed collapse", () => {
    const unreachable = createBootstrapUnavailableDataset("daemon-unreachable", 1_000);
    const generic = createBootstrapUnavailableDataset("start-failed", 1_000);
    const unreachableDetail = unreachable.pages.system.rows[0]?.summary?.detail ?? "";
    const genericDetail = generic.pages.system.rows[0]?.summary?.detail ?? "";
    expect(unreachableDetail).toContain("no reachable Fabric daemon");
    expect(unreachableDetail).not.toBe(genericDetail);
  });

  it("imports only the public protocol package across the Console source tree", async () => {
    const sourceRoot = fileURLToPath(new URL("../src/", import.meta.url));
    const files = (await readdir(sourceRoot, { recursive: true })).filter(
      (file) => file.endsWith(".ts"),
    );
    const source = (
      await Promise.all(files.map((file) => readFile(`${sourceRoot}${file}`, "utf8")))
    ).join("\n");

    expect(source).not.toMatch(
      /(?:from|import\()\s*["'][^"']*(?:agent-fabric\/src|herdr\/|\.\.\/agent-fabric(?!-protocol))/iu,
    );
    expect(source).not.toMatch(/@local\/agent-fabric-(?!protocol)/);
  });
});
