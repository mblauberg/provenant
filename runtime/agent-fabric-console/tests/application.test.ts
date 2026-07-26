import { describe, expect, it, vi } from "vitest";

import type {
  CanonicalRelativePath,
  OperatorCapabilityCredential,
  OperatorDetailReadRequest,
  OperatorDetailReadResult,
  OperatorProjectionSnapshot,
  OperatorViewPageRequest,
  OperatorViewPageResult,
  GitRepositoryReadRequest,
  GitRepositoryProjection,
  GitRepositoryReadResult,
  MessageBodyReadResult,
  MessageId,
  ProjectId,
  ProjectSessionId,
  Sha256Digest,
  Timestamp,
  ProjectionEventsResult,
} from "@local/agent-fabric-protocol";
import {
  guidedWorkflowPrompt,
  sessionSwitchBlockReason,
  startFabricConsoleApplication,
  workflowSessionSwitchBlocked,
  type ConsoleBootstrapResult,
  type ConsoleBootstrapPort,
} from "../src/application.js";
import {
  reduceFabricPointer,
  renderFabricConsoleFrame,
} from "../src/index.js";
import type {
  ConsoleProtocolBinding,
  ConsoleProtocolPort,
} from "../src/protocol-adapter.js";
import { FABRIC_VIEWS, revisionFromProtocol } from "../src/model.js";
import type { ConsoleWorkflowReview } from "../src/workflow.js";

const timestamp = "2026-07-11T12:00:00.000Z" as Timestamp;
const digest = (`sha256:${"f".repeat(64)}`) as Sha256Digest;
const projectId = "project-application" as ProjectId;
const credential = {
  capabilityId: "capability-application",
  token: "token-never-render",
} as OperatorCapabilityCredential;

function currentBinding(
  port: ConsoleProtocolPort,
  readOnly: boolean,
  actions: Extract<ConsoleProtocolBinding, { ok: true }>["actions"],
): ConsoleProtocolBinding {
  return {
    ok: true,
    port,
    readOnly,
    actions,
    nativeNotificationProjection: "daemon-journal",
    runSessionProjection: "exact",
    compatibility: { mode: "current" },
  };
}

function snapshot(): OperatorProjectionSnapshot {
  return {
    schemaVersion: 1,
    snapshotRevision: 1,
    readTransactionId: "application-snapshot",
    project: {
      freshness: "live",
      source: "fabric",
      revision: 1,
      observedAt: timestamp,
      value: { projectId, canonicalRoot: "/repo" },
    },
    session: {
      freshness: "unavailable",
      source: "fabric",
      revision: 1,
      observedAt: timestamp,
      reason: "no session selected",
    },
    runs: {
      freshness: "live",
      source: "fabric",
      revision: 1,
      observedAt: timestamp,
      value: [],
    },
    attention: {
      freshness: "live",
      source: "fabric",
      revision: 1,
      observedAt: timestamp,
      value: [],
    },
    capacity: {
      freshness: "unavailable",
      source: "fabric",
      revision: 1,
      observedAt: timestamp,
      reason: "unknown",
    },
    cursor: 1,
    stateDigest: digest,
  };
}

function repositoryProjection(): GitRepositoryProjection {
  return {
    freshness: "live",
    source: "git",
    revision: 1,
    observedAt: timestamp,
    canonicalRepositoryRoot: "/repo",
    canonicalWorktreePath: "/repo",
    repositoryStateDigest: digest,
    head: { detached: false, refName: "refs/heads/main", objectDigest: digest },
    headDigest: digest,
    indexDigest: digest,
    worktreeDigest: digest,
    remoteDigest: digest,
    changes: {
      staged: { paths: ["src/staged.ts"], truncated: false },
      unstaged: { paths: ["src/changed.ts"], truncated: false },
      untracked: { paths: [], truncated: false },
      conflicted: { paths: [], truncated: false },
    },
    operationState: { kind: "clean" },
    upstream: {
      remoteName: "origin",
      branchName: "refs/remotes/origin/main",
      ahead: 1,
      behind: 0,
    },
    diff: {
      selector: { kind: "working-tree" },
      artifactRef: {
        path: "private/git-diffs/current.patch" as CanonicalRelativePath,
        digest,
      },
      baseDigest: digest,
      targetDigest: digest,
    },
    log: {
      items: [{
        objectDigest: digest,
        parentObjectDigests: [],
        subject: "Bind typed reads",
        authorTimestamp: timestamp,
      }],
      hasMore: false,
      nextCursor: null,
    },
    branches: {
      items: [{ refName: "refs/heads/main", objectDigest: digest, checkedOut: true, upstream: null }],
      truncated: false,
    },
    worktrees: {
      items: [{
        canonicalPath: "/repo",
        head: { detached: false, refName: "refs/heads/main", objectDigest: digest },
        current: true,
        locked: false,
      }],
      truncated: false,
    },
    hostedChecks: {
      freshness: "unavailable",
      source: "github",
      revision: 1,
      observedAt: timestamp,
      reason: "GitHub not configured",
    },
  };
}

function protocolPort(): ConsoleProtocolPort {
  return {
    snapshot: vi.fn(async () => snapshot()),
    events: vi.fn(async (): Promise<ProjectionEventsResult> => ({
      status: "continuation",
      events: [],
      nextCursor: 1,
      hasMore: false,
      snapshotRevision: 1,
      readTransactionId: "events",
    })),
    viewPage: vi.fn(async (request) => ({
      status: "page",
      view: request.view,
      rows: [],
      nextCursor: 0,
      hasMore: false,
      snapshotRevision: request.snapshotRevision,
      readTransactionId: `page-${request.view}`,
    }) as never),
    readDetail: vi.fn(async (): Promise<OperatorDetailReadResult> => ({
      status: "resnapshot-required",
      reason: "snapshot-mismatch",
      currentSnapshotRevision: 1,
    })),
    readGate: vi.fn(async () => {
      throw new Error("unused");
    }),
    readMessageBody: null,
    readRepository: null,
    readArtifactContent: null,
  };
}

function activityMessagePage(
  request: OperatorViewPageRequest,
  input: Readonly<{
    projectSessionId: ProjectSessionId;
    messageId: MessageId;
  }>,
): OperatorViewPageResult {
  const group = activityMessageGroup(input);
  if (request.view !== "activity") {
    return {
      status: "page",
      view: request.view,
      rows: [],
      nextCursor: 0,
      hasMore: false,
      snapshotRevision: request.snapshotRevision,
      readTransactionId: `page-${request.view}`,
    } as OperatorViewPageResult;
  }
  return {
    status: "page",
    view: "activity",
    rows: [{
      itemId: group.groupId,
      itemRevision: 9,
      fact: {
        freshness: "live",
        source: "fabric",
        revision: 9,
        observedAt: timestamp,
        value: {
          summary: {
            kind: "activity",
            summary: "Message preview",
            occurredAt: timestamp,
            group,
          },
          detailRef: {
            kind: "activity",
            groupId: group.groupId,
            expectedRevision: 9,
          },
          actionAvailability: { state: "read-only", reason: "state-ineligible" },
        },
      },
    }],
    nextCursor: 1,
    hasMore: false,
    snapshotRevision: request.snapshotRevision,
    readTransactionId: "page-activity",
  } as OperatorViewPageResult;
}

function activityMessageGroup(
  input: Readonly<{
    projectSessionId: ProjectSessionId;
    messageId: MessageId;
  }>,
) {
  return {
    groupId: "activity-group-must-not-be-derived",
    ordinal: 1,
    kind: "message",
    actorIds: ["agent-message"],
    target: { kind: "message", id: input.messageId },
    eventKinds: ["message-persisted"],
    occurredAtRange: { first: timestamp, last: timestamp },
    sourceRange: { first: 9, last: 9 },
    count: 1,
    evidenceLinkCount: 0,
    evidenceLinksDigest:
      "sha256:4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945" as never,
    evidenceLinksTruncated: false,
    evidenceLinks: [],
    members: [{
      ordinal: 1,
      eventId: "event-must-not-be-derived",
      eventKind: "message-persisted",
      actorId: "agent-message",
      target: { kind: "message", id: input.messageId },
      occurredAt: timestamp,
      sourceRevision: 9,
      messageBodyRef: {
        projectSessionId: input.projectSessionId,
        messageId: input.messageId,
        expectedRevision: 4,
      },
      detailAvailability: "available",
      evidenceLinkCount: 0,
      evidenceLinksDigest:
        "sha256:4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945" as never,
    }],
  } as const;
}

function activityMessageDetail(
  input: Readonly<{
    projectSessionId: ProjectSessionId;
    messageId: MessageId;
  }>,
): OperatorDetailReadResult {
  const group = activityMessageGroup(input);
  return {
    status: "current",
    detailRef: {
      kind: "activity",
      groupId: group.groupId,
      expectedRevision: 9,
    },
    detail: {
      freshness: "live",
      source: "fabric",
      revision: 9,
      observedAt: timestamp,
      value: {
        kind: "activity",
        group,
        memberDetails: [{
          eventId: "event-must-not-be-derived",
          status: "available",
          content: '{"messageId":"message-exact"}',
          transformation: "none",
        }],
      },
    },
    snapshotRevision: 1,
    readTransactionId: "activity-group-detail",
  };
}

const runtimeDependencies = {
  render: renderFabricConsoleFrame,
  reducePointer: reduceFabricPointer,
};

describe("typed Console application bootstrap boundary", () => {
  it("blocks session navigation while operator-action custody is unresolved", () => {
    expect(sessionSwitchBlockReason({
      pendingCommandIds: ["command-pending"],
      lastActionStatus: null,
    })).toBe("SESSION_SWITCH_BLOCKED_UNRESOLVED_ACTION");
    expect(sessionSwitchBlockReason({
      pendingCommandIds: [],
      lastActionStatus: { status: "ambiguous" } as never,
    })).toBe("SESSION_SWITCH_BLOCKED_UNRESOLVED_ACTION");
    expect(sessionSwitchBlockReason({
      pendingCommandIds: [],
      lastActionStatus: { status: "committed" } as never,
    })).toBeNull();
    expect(workflowSessionSwitchBlocked("pending")).toBe(true);
    expect(workflowSessionSwitchBlocked("ambiguous")).toBe(true);
    expect(workflowSessionSwitchBlocked("committed")).toBe(false);
  });

  it("names the exact required field in each guided workflow prompt", () => {
    const binding = {
      itemId: "prompt-item",
      itemRevision: revisionFromProtocol(1),
      projectionRevision: revisionFromProtocol(1),
    };
    expect(guidedWorkflowPrompt("accept", { ...binding, view: "evidence" }))
      .toContain("intake=<stable-id>");
    expect(guidedWorkflowPrompt("promotion", { ...binding, view: "project" }))
      .toContain("gate=<stable-id>");
    expect(guidedWorkflowPrompt("accept", { ...binding, view: "attention" }))
      .toContain("selected gate is revision-bound");
    expect(guidedWorkflowPrompt("accept", { ...binding, view: "attention" }))
      .not.toContain("gate=<stable-id>");
  });

  it("renders an honest non-mutating System state when bootstrap is unavailable", async () => {
    const bootstrap: ConsoleBootstrapPort = {
      startOrAttach: vi.fn(async (): Promise<ConsoleBootstrapResult> => ({
        status: "unavailable",
        reason: "feature-unavailable",
      })),
    };
    const draw = vi.fn();

    const application = await startFabricConsoleApplication({
      bootstrap,
      projectRoot: "/repo",
      surface: "standalone",
      viewport: { columns: 80, rows: 24 },
      draw,
      eventId: () => "event-1",
      confirmationId: () => "confirmation-1",
      ...runtimeDependencies,
    });

    expect(application.dataset).toMatchObject({
      connection: { state: "unavailable", reason: "bootstrap-unavailable" },
      canMutate: false,
    });
    expect(application.controller.state.activeView).toBe("system");
    expect(application.dataset.pages.system.rows[0]).toMatchObject({
      stableId: "bootstrap",
      freshness: {
        state: "unavailable",
        reason: "feature-unavailable",
      },
    });
    expect(application.frame.rows.join("\n")).toContain(
      "the required Console feature was not negotiated",
    );
    expect(JSON.stringify(application.dataset)).not.toContain("token-never-render");
    await application.close("operator");
  });

  it("renders schema cutover as an explicit preserved-state human gate", async () => {
    const application = await startFabricConsoleApplication({
      bootstrap: {
        startOrAttach: vi.fn(async (): Promise<ConsoleBootstrapResult> => ({
          status: "unavailable",
          reason: "schema-cutover-required",
        })),
      },
      projectRoot: "/repo",
      surface: "standalone",
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      eventId: () => "event-cutover",
      confirmationId: () => "confirmation-cutover",
      ...runtimeDependencies,
    });

    expect(application.dataset.pages.system.rows[0]?.summary).toMatchObject({
      kind: "system",
      systemKind: "daemon",
      state: "unavailable",
      detail: "CUTOVER REQUIRED — existing database preserved",
    });
    expect(application.frame.rows.join("\n")).toContain(
      "CUTOVER REQUIRED — existing database preserved",
    );
    expect(application.dataset.canMutate).toBe(false);
    await application.close("operator");
  });

  it("renders protocol incompatibility as an empty connection failure rather than a fallback row", async () => {
    const bootstrap: ConsoleBootstrapPort = {
      startOrAttach: vi.fn(async (): Promise<ConsoleBootstrapResult> => ({
        status: "protocol-incompatible",
        primary: { code: "PROTOCOL_INVALID", message: "unknown optional feature" },
        result: {
          code: "PROTOCOL_INCOMPATIBLE",
          message: "unnegotiated notification field",
          operation: "fabric.v1.operator-projection.snapshot",
          closedReason: "unnegotiated-field",
        },
      })),
    };

    const application = await startFabricConsoleApplication({
      bootstrap,
      projectRoot: "/repo",
      surface: "standalone",
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      eventId: () => "event-incompatible",
      confirmationId: () => "confirmation-incompatible",
      ...runtimeDependencies,
    });

    expect(application.dataset.connection).toStrictEqual({
      state: "protocol-incompatible",
      code: "CONSOLE_PROTOCOL_INCOMPATIBLE",
      message: "unnegotiated notification field",
      operation: "fabric.v1.operator-projection.snapshot",
      closedReason: "unnegotiated-field",
      primary: { code: "PROTOCOL_INVALID", message: "unknown optional feature" },
    });
    expect(application.dataset.pages.attention.rows).toStrictEqual([]);
    expect(application.dataset.canMutate).toBe(false);
    expect(application.frame.rows.join("\n")).toContain("PROTOCOL-INCOMPATIBLE");
    expect(application.frame.rows.join("\n")).not.toContain("feature-not-negotiated");
    await application.close("operator");
  });

  it.each(["standalone", "herdr"] as const)(
    "uses the same public projection protocol on the %s surface and never replays commands",
    async (surface) => {
      const port = protocolPort();
      const detach = vi.fn(async () => {});
      const close = vi.fn(async () => {});
      const bootstrap: ConsoleBootstrapPort = {
        startOrAttach: vi.fn(async (): Promise<ConsoleBootstrapResult> => ({
          status: "connected",
          binding: currentBinding(port, true, null),
          credential,
          projectId,
          detach,
          close,
        })),
      };
      const application = await startFabricConsoleApplication({
        bootstrap,
        projectRoot: "/repo",
        surface,
        viewport: { columns: 80, rows: 24 },
        draw: () => {},
        eventId: () => "event-1",
        confirmationId: () => "confirmation-1",
        ...runtimeDependencies,
      });

      expect(application.dataset.connection).toStrictEqual({
        state: "live",
        compatibility: { mode: "current" },
      });
      expect(application.dataset.canMutate).toBe(false);
      expect(Object.keys(application.dataset.pages)).toStrictEqual(FABRIC_VIEWS);
      await application.refresh();
      await Promise.all([
        application.close("operator"),
        application.close("operator"),
      ]);

      expect(bootstrap.startOrAttach).toHaveBeenCalledWith({
        projectRoot: "/repo",
        surface,
      });
      expect(port.snapshot).toHaveBeenCalledTimes(1);
      expect(port.events).toHaveBeenCalledTimes(1);
      expect(detach).toHaveBeenCalledTimes(1);
      expect(close).toHaveBeenCalledTimes(1);
    },
  );

  it("renders and opens exact zero-run session choices from the project selector", async () => {
    const targetSessionId = "session-application-zero-run" as ProjectSessionId;
    const choices = [
      {
        projectSessionId: targetSessionId,
        mode: "coordinated" as const,
        state: "draft" as const,
        revision: 1,
        generation: 1,
        lastEventAt: timestamp,
      },
      {
        projectSessionId: "session-application-existing" as ProjectSessionId,
        mode: "independent" as const,
        state: "active" as const,
        revision: 2,
        generation: 1,
        lastEventAt: timestamp,
      },
    ];
    const port = protocolPort();
    const selectProjectSession = vi.fn();
    const sessionSelection = {
      choices,
      selectProjectSession,
      selectProject: vi.fn(),
    };
    const common = {
      credential,
      projectId,
      sessionSelection,
      detach: async () => {},
      close: async () => {},
    };
    const selectedConnection = {
      status: "connected" as const,
      binding: currentBinding(port, true, null),
      ...common,
      projectSessionId: targetSessionId,
    };
    selectProjectSession.mockResolvedValue(selectedConnection);
    const application = await startFabricConsoleApplication({
      bootstrap: {
        startOrAttach: async () => ({
          status: "connected" as const,
          binding: currentBinding(port, true, null),
          ...common,
        }),
      },
      projectRoot: "/repo",
      surface: "standalone",
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      eventId: () => "session-choice-input",
      confirmationId: () => "session-choice-confirmation",
      ...runtimeDependencies,
    });

    await application.handleInput({ kind: "key", key: "text", text: "s" });
    expect(application.controller.state.activeView).toBe("project");
    expect(application.ui.focusId).toBe(`session:select:${targetSessionId}`);
    expect(application.frame.hitRegions.map(({ id }) => id)).toEqual(
      expect.arrayContaining(choices.map(({ projectSessionId }) => `session:select:${projectSessionId}`)),
    );
    expect(application.frame.rows.join("\n")).toContain(targetSessionId);

    await application.handleInput({ kind: "key", key: "enter" });
    expect(selectProjectSession).toHaveBeenCalledWith(targetSessionId);
    expect(application.dataset.projectSessions?.selectedProjectSessionId).toBe(targetSessionId);
    await application.close("operator");
  });

  it("discards a late poll from the previously selected session", async () => {
    const sessionA = "session-application-race-a" as ProjectSessionId;
    const sessionB = "session-application-race-b" as ProjectSessionId;
    const sessionSnapshot = (projectSessionId: ProjectSessionId): OperatorProjectionSnapshot => ({
      ...snapshot(),
      session: {
        freshness: "live",
        source: "fabric",
        revision: 1,
        observedAt: timestamp,
        value: {
          projectSessionId,
          projectId,
          mode: "independent",
          state: "active",
          revision: 1,
          generation: 1,
          authorityRef: digest,
          budgetRef: "budget-application-race",
          launchPacketRef: { path: "launch/race.json" as never, digest },
          membershipRevision: 1,
          origin: { kind: "operator-launch", operatorId: "operator-application-race" as never },
        },
      },
    });
    let releasePoll: ((value: ProjectionEventsResult) => void) | undefined;
    const staleEvents = vi.fn(async () => await new Promise<ProjectionEventsResult>((resolve) => {
      releasePoll = resolve;
    }));
    const portA: ConsoleProtocolPort = {
      ...protocolPort(),
      snapshot: vi.fn(async () => sessionSnapshot(sessionA)),
      events: staleEvents,
    };
    const portB: ConsoleProtocolPort = {
      ...protocolPort(),
      snapshot: vi.fn(async () => sessionSnapshot(sessionB)),
    };
    const choices = [sessionA, sessionB].map((projectSessionId) => ({
      projectSessionId,
      mode: "independent" as const,
      state: "active" as const,
      revision: 1,
      generation: 1,
      lastEventAt: timestamp,
    }));
    const selectProjectSession = vi.fn(async (projectSessionId: ProjectSessionId) => ({
      status: "connected" as const,
      binding: currentBinding(projectSessionId === sessionB ? portB : portA, true, null),
      credential,
      projectId,
      projectSessionId,
      sessionSelection,
      detach: async () => {},
      close: async () => {},
    }));
    const sessionSelection = {
      choices,
      selectProjectSession,
      selectProject: vi.fn(),
    };
    const application = await startFabricConsoleApplication({
      bootstrap: {
        startOrAttach: async () => ({
          status: "connected" as const,
          binding: currentBinding(portA, true, null),
          credential,
          projectId,
          projectSessionId: sessionA,
          sessionSelection,
          detach: async () => {},
          close: async () => {},
        }),
      },
      projectRoot: "/repo",
      surface: "standalone",
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      eventId: () => "session-race-input",
      confirmationId: () => "session-race-confirmation",
      ...runtimeDependencies,
    });

    const lateRefresh = application.refresh();
    await vi.waitFor(() => expect(staleEvents).toHaveBeenCalledOnce());
    await application.handleActivation({
      regionId: `session:select:${sessionB}`,
      binding: null,
      provenance: "keyboard",
      eventId: "select-session-b",
    });
    releasePoll?.({
      status: "continuation",
      events: [],
      nextCursor: 1,
      hasMore: false,
      snapshotRevision: 1,
      readTransactionId: "stale-session-a-poll",
    });
    await lateRefresh;

    const currentSession = application.dataset.snapshot?.session;
    expect(currentSession?.freshness === "live"
      ? currentSession.value?.projectSessionId
      : null).toBe(sessionB);
    await application.close("operator");
  });

  it("opens an exact run session from the project selector and returns with s", async () => {
    const projectSessionId = "session-application-independent" as ProjectSessionId;
    const runId = "run-application-independent" as never;
    const choice = {
      projectSessionId,
      mode: "independent" as const,
      state: "active" as const,
      revision: 2,
      generation: 1,
      lastEventAt: timestamp,
    };
    const projectSnapshot: OperatorProjectionSnapshot = {
      ...snapshot(),
      runs: {
        freshness: "live",
        source: "fabric",
        revision: 2,
        observedAt: timestamp,
        value: [{
          projectSessionId,
          runId,
          phase: "active",
          chairAgentId: "chair-application" as never,
          nextMilestone: "verification",
          health: "healthy",
        }],
      },
    };
    const basePort = protocolPort();
    const port: ConsoleProtocolPort = {
      ...basePort,
      snapshot: vi.fn(async () => projectSnapshot),
      viewPage: vi.fn(async (request): Promise<OperatorViewPageResult> => {
      if (request.view !== "runs") {
        return {
          status: "page",
          view: request.view,
          rows: [],
          nextCursor: 0,
          hasMore: false,
          snapshotRevision: request.snapshotRevision,
          readTransactionId: `page-${request.view}`,
        } as OperatorViewPageResult;
      }
      return {
        status: "page",
        view: "runs",
        rows: [{
          itemId: runId,
          itemRevision: 2,
          fact: {
            freshness: "live",
            source: "fabric",
            revision: 2,
            observedAt: timestamp,
            value: {
              summary: {
                kind: "run",
                projectSessionId,
                phase: "active",
                health: "healthy",
                nextMilestone: "verification",
                declaredProgress: { plan: "open", counts: { blocked: 0, ready: 0, active: 1, complete: 0, cancelled: 0, degraded: 0 } },
                identity: {
                  runKind: "coordination",
                  chairAgentId: "agent_application_chair" as never,
                  acceptedScopeRef: null,
                  currentPlanRef: null,
                  planRevision: null,
                  workstreams: [],
                  lastEventAt: timestamp,
                },
              },
              detailRef: {
                kind: "run",
                projectSessionId,
                coordinationRunId: runId,
                expectedRevision: 2,
              },
              actionAvailability: { state: "read-only", reason: "authority-insufficient" },
            },
          },
        }],
        nextCursor: 1,
        hasMore: false,
        snapshotRevision: request.snapshotRevision,
        readTransactionId: "page-runs-selector",
      };
      }),
    };
    const actions = {
      preview: vi.fn(async () => { throw new Error("unused"); }),
      commit: vi.fn(async () => { throw new Error("unused"); }),
      status: vi.fn(async () => { throw new Error("unused"); }),
      reconcile: vi.fn(async () => { throw new Error("unused"); }),
    };
    const actionPlanner = {
      plan: vi.fn(async () => null),
      confirmation: vi.fn(async () => { throw new Error("unused"); }),
    };
    const selectProjectSession = vi.fn<(
      projectSessionId: ProjectSessionId,
    ) => Promise<Extract<ConsoleBootstrapResult, { status: "connected" }>>>();
    const selectProject = vi.fn<() => Promise<Extract<
      ConsoleBootstrapResult,
      { status: "connected" }
    >>>();
    const sessionSelection = { choices: [choice], selectProjectSession, selectProject };
    const close = vi.fn(async () => {});
    const detach = vi.fn(async () => {});
    const projectConnection = {
      status: "connected" as const,
      binding: currentBinding(port, true, null),
      credential,
      projectId,
      sessionSelection,
      detach,
      close,
    };
    const selectedConnection = {
      status: "connected" as const,
      binding: currentBinding(port, false, actions),
      credential,
      projectId,
      projectSessionId,
      actionPlanner,
      sessionSelection,
      detach,
      close,
    };
    selectProjectSession.mockResolvedValue(selectedConnection);
    selectProject.mockResolvedValue(projectConnection);
    const application = await startFabricConsoleApplication({
      bootstrap: { startOrAttach: async () => projectConnection },
      projectRoot: "/repo",
      surface: "standalone",
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      eventId: () => "session-switch-input",
      confirmationId: () => "session-switch-confirmation",
      ...runtimeDependencies,
    });
    expect(application.dataset).toMatchObject({
      canMutate: false,
      projectSessions: { selectedProjectSessionId: null },
    });
    await application.handleInput({ kind: "key", key: "text", text: "3" });
    expect(application.frame.rows.join("\n")).toContain(projectSessionId);

    await application.handleActivation({
      regionId: `row:runs:${String(runId)}`,
      binding: {
        view: "runs",
        itemId: runId,
        itemRevision: revisionFromProtocol(2),
        projectionRevision: revisionFromProtocol(1),
      },
      provenance: "keyboard",
      eventId: "open-exact-session",
    });
    expect(selectProjectSession).toHaveBeenCalledWith(projectSessionId);
    expect(application.dataset).toMatchObject({
      canMutate: true,
      projectSessions: { selectedProjectSessionId: projectSessionId },
    });
    expect(application.frame.rows.at(-1)).toContain("s sessions");
    expect(application.frame.rows.at(-1)).toContain("q detach");

    await application.handleInput({ kind: "key", key: "text", text: "s" });
    expect(selectProject).toHaveBeenCalledOnce();
    expect(application.dataset).toMatchObject({
      canMutate: false,
      projectSessions: { selectedProjectSessionId: null },
    });
    await application.close("operator");
  });

  it("enables mutations from the production planner returned by bootstrap", async () => {
    const port = protocolPort();
    const actions = {
      preview: vi.fn(async () => { throw new Error("unused"); }),
      commit: vi.fn(async () => { throw new Error("unused"); }),
      status: vi.fn(async () => { throw new Error("unused"); }),
      reconcile: vi.fn(async () => { throw new Error("unused"); }),
    };
    const actionPlanner = {
      plan: vi.fn(async () => null),
      confirmation: vi.fn(async () => { throw new Error("unused"); }),
    };
    const application = await startFabricConsoleApplication({
      bootstrap: {
        startOrAttach: async () => ({
          status: "connected",
          binding: currentBinding(port, false, actions),
          credential,
          projectId,
          actionPlanner,
          detach: async () => {},
          close: async () => {},
        }),
      },
      projectRoot: "/repo",
      surface: "standalone",
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      eventId: () => "event-production-planner",
      confirmationId: () => "confirmation-production-planner",
      ...runtimeDependencies,
    });

    expect(application.dataset.canMutate).toBe(true);
    await application.close("operator");
  });

  it("routes the typed palette through full-frame Review and a distinct confirmation gesture", async () => {
    const port = protocolPort();
    const review: ConsoleWorkflowReview = {
      workflowId: "workflow_application",
      kind: "intake-draft-create",
      source: "local-typed-preview",
      stage: "review",
      previewDigest: digest,
      expectedRevision: "1" as never,
      consequenceClass: "routine",
      confirmationMode: "explicit",
      summary: "intake-draft-create",
      details: [{ label: "summary", value: '"Discuss scope"' }],
      evidence: [],
      openedByEventId: "workflow-event-1",
      armedByEventId: null,
      result: null,
      failure: null,
    };
    const prepare = vi.fn(async () => review);
    const arm = vi.fn((current: ConsoleWorkflowReview, eventId: string) => ({
      ...current,
      stage: "confirm" as const,
      armedByEventId: eventId,
    }));
    const commit = vi.fn(async ({ review: current }: { review: ConsoleWorkflowReview }) => ({
      reconnectProjectSessionId: null,
      review: {
        ...current,
        stage: "committed" as const,
        result: "intake-draft-create | intake_application | r1",
      },
    }));
    let sequence = 0;
    const application = await startFabricConsoleApplication({
      bootstrap: {
        startOrAttach: async () => ({
          status: "connected",
          binding: currentBinding(port, false, null),
          credential,
          projectId,
          workflowPlanner: {
            capabilities: {
              intake: { state: "available" },
              gate: { state: "unavailable", reason: "fixture" },
              launch: { state: "unavailable", reason: "fixture" },
              git: { state: "unavailable", reason: "fixture" },
              promotion: { state: "unavailable", reason: "fixture" },
            },
            prepare,
            prepareGuided: vi.fn(async () => review),
            arm,
            commit,
            observe: vi.fn(async ({ review: current }: { review: ConsoleWorkflowReview }) => current),
          },
          detach: async () => {},
          close: async () => {},
        }),
      },
      projectRoot: "/repo",
      surface: "standalone",
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      eventId: () => `workflow-event-${String(++sequence)}`,
      confirmationId: () => "confirmation-workflow",
      ...runtimeDependencies,
    });

    await application.handleInput({ kind: "key", key: "text", text: ":" });
    await application.handleInput({
      kind: "paste",
      text: '{"kind":"intake-draft-create","request":{"summary":"Discuss scope"}}',
    });
    await application.handleInput({ kind: "key", key: "enter" });

    expect(prepare).toHaveBeenCalledOnce();
    expect(commit).not.toHaveBeenCalled();
    expect(application.frame.rows.join("\n")).toContain("REVIEW REVIEW");
    expect(application.frame.rows.join("\n")).toContain("Workflow: intake-draft-create");

    await application.handleInput({ kind: "key", key: "enter" });
    expect(arm).toHaveBeenCalledOnce();
    expect(commit).not.toHaveBeenCalled();
    expect(application.frame.rows.join("\n")).toContain("REVIEW CONFIRM");

    await application.handleInput({ kind: "key", key: "text", text: "3" });
    expect(commit).toHaveBeenCalledOnce();
    expect(application.frame.rows.join("\n")).toContain("REVIEW COMMITTED");
    expect(application.frame.rows.join("\n")).toContain("intake_application");
    await application.close("operator");
  });

  it("reconciles an asynchronous workflow commit that completes during an inert resize", async () => {
    const port = protocolPort();
    const review: ConsoleWorkflowReview = {
      workflowId: "workflow_inert_completion",
      kind: "intake-draft-create",
      source: "local-typed-preview",
      stage: "review",
      previewDigest: digest,
      expectedRevision: "1" as never,
      consequenceClass: "routine",
      confirmationMode: "explicit",
      summary: "intake-draft-create",
      details: [{ label: "summary", value: '"Persist completion"' }],
      evidence: [],
      openedByEventId: "workflow-inert-1",
      armedByEventId: null,
      result: null,
      failure: null,
    };
    const committedReview: ConsoleWorkflowReview = {
      ...review,
      stage: "committed",
      armedByEventId: "workflow-inert-arm",
      result: "intake-draft-create | inert_completion | r1",
    };
    let resolveCommit!: (value: {
      reconnectProjectSessionId: null;
      review: ConsoleWorkflowReview;
    }) => void;
    const deferredCommit = new Promise<{
      reconnectProjectSessionId: null;
      review: ConsoleWorkflowReview;
    }>((resolve) => {
      resolveCommit = resolve;
    });
    const commit = vi.fn(() => deferredCommit);
    let sequence = 0;
    const application = await startFabricConsoleApplication({
      bootstrap: {
        startOrAttach: async () => ({
          status: "connected",
          binding: currentBinding(port, false, null),
          credential,
          projectId,
          workflowPlanner: {
            capabilities: {
              intake: { state: "available" },
              gate: { state: "unavailable", reason: "fixture" },
              launch: { state: "unavailable", reason: "fixture" },
              git: { state: "unavailable", reason: "fixture" },
              promotion: { state: "unavailable", reason: "fixture" },
            },
            prepare: vi.fn(async () => review),
            prepareGuided: vi.fn(async () => review),
            arm: vi.fn((current: ConsoleWorkflowReview, eventId: string) => ({
              ...current,
              stage: "confirm" as const,
              armedByEventId: eventId,
            })),
            commit,
            observe: vi.fn(async ({ review: current }: { review: ConsoleWorkflowReview }) => current),
          },
          detach: async () => {},
          close: async () => {},
        }),
      },
      projectRoot: "/repo",
      surface: "standalone",
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      eventId: () => `workflow-inert-${String(++sequence)}`,
      confirmationId: () => "confirmation-workflow-inert",
      ...runtimeDependencies,
    });

    await application.handleInput({ kind: "key", key: "text", text: ":" });
    await application.handleInput({
      kind: "paste",
      text: '{"kind":"intake-draft-create","request":{"summary":"Persist completion"}}',
    });
    await application.handleInput({ kind: "key", key: "enter" });
    await application.handleInput({ kind: "key", key: "enter" });
    const completion = application.handleInput({ kind: "key", key: "text", text: "3" });
    await vi.waitFor(() => expect(commit).toHaveBeenCalledOnce());

    expect(application.resize({ columns: 29, rows: 5 }).mode).toBe("inert");
    resolveCommit({ reconnectProjectSessionId: null, review: committedReview });
    await completion;
    expect(application.frame.mode).toBe("inert");

    const restored = application.resize({ columns: 80, rows: 24 });
    expect(restored.rows.join("\n")).toContain("REVIEW COMMITTED");
    expect(restored.rows.join("\n")).toContain("inert_completion");
    expect(commit).toHaveBeenCalledOnce();
    await application.close("operator");
  });

  it("opens a guided evidence workflow, preserves it across resize, and submits structured input", async () => {
    const port = protocolPort();
    const review: ConsoleWorkflowReview = {
      workflowId: "workflow_guided_application",
      kind: "intake-revise",
      source: "local-typed-preview",
      stage: "review",
      previewDigest: digest,
      expectedRevision: revisionFromProtocol(4),
      consequenceClass: "consequential",
      confirmationMode: "explicit",
      summary: "Discuss reviewed evidence",
      details: [],
      evidence: [],
      openedByEventId: "guided-submit",
      armedByEventId: null,
      result: null,
      failure: null,
    };
    const prepareGuided = vi.fn(async () => review);
    const workflowPlanner = {
      capabilities: {
        intake: { state: "available" as const },
        gate: { state: "available" as const },
        launch: { state: "unavailable" as const, reason: "typed-planner-unregistered" },
        git: { state: "unavailable" as const, reason: "typed-planner-unregistered" },
        promotion: { state: "unavailable" as const, reason: "typed-planner-unregistered" },
      },
      prepare: vi.fn(async () => review),
      prepareGuided,
      arm: vi.fn((current: ConsoleWorkflowReview) => current),
      commit: vi.fn(async (input: { review: ConsoleWorkflowReview }) => ({
        review: input.review,
        reconnectProjectSessionId: null,
      })),
      observe: vi.fn(async (input: { review: ConsoleWorkflowReview }) => input.review),
    };
    let event = 0;
    const application = await startFabricConsoleApplication({
      bootstrap: {
        startOrAttach: async () => ({
          status: "connected",
          binding: currentBinding(port, false, null),
          credential,
          projectId,
          workflowPlanner,
          detach: async () => {},
          close: async () => {},
        }),
      },
      projectRoot: "/repo",
      surface: "standalone",
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      eventId: () => `guided-${String(++event)}`,
      confirmationId: () => "guided-confirmation",
      ...runtimeDependencies,
    });
    const binding = {
      view: "evidence" as const,
      itemId: "evidence-guided",
      itemRevision: revisionFromProtocol(7),
      projectionRevision: revisionFromProtocol(1),
    };
    application.controller.updateDataset({
      ...application.dataset,
      pages: {
        ...application.dataset.pages,
        evidence: {
          ...application.dataset.pages.evidence,
          rows: [{
            view: "evidence",
            stableId: binding.itemId,
            revision: binding.itemRevision,
            urgency: "normal",
            freshness: {
              state: "live",
              source: "fabric",
              revision: binding.itemRevision,
              observedAt: timestamp,
              ageMs: 0,
            },
            summary: {
              kind: "evidence",
              evidenceKind: "artifact",
              status: "informational",
              provenance: "agent:chair",
            },
            detailRef: {
              kind: "evidence",
              evidenceId: binding.itemId,
              expectedRevision: 7,
            },
            actionAvailability: { state: "read-only", reason: "state-ineligible" },
          }],
          snapshotRevision: binding.projectionRevision,
        },
      },
    });
    application.controller.activateView("evidence");
    application.controller.select("evidence", binding.itemId);
    application.repaint();

    await application.handleActivation({
      regionId: "workflow:discuss",
      binding,
      provenance: "keyboard",
      eventId: "guided-open",
    });
    expect(application.ui).toMatchObject({
      inputMode: "guided",
      guidedWorkflow: { action: "discuss", binding },
    });
    expect(prepareGuided).not.toHaveBeenCalled();

    application.resize({ columns: 54, rows: 16 });
    expect(application.ui).toMatchObject({
      inputMode: "guided",
      guidedWorkflow: { action: "discuss", binding },
    });
    await application.handleInput({ kind: "paste", text: "intake=intake-guided" });
    await application.handleInput({ kind: "key", key: "enter" });

    expect(prepareGuided).toHaveBeenCalledWith(expect.objectContaining({
      action: "discuss",
      binding,
      raw: "intake=intake-guided",
    }));
    expect(application.ui.workflowReview).toBe(review);
    expect(application.ui.guidedWorkflow).toBeNull();
    await application.close("operator");
  });

  it("reattaches with session-bound authority after a reviewed project-session creation", async () => {
    const port = protocolPort();
    const review: ConsoleWorkflowReview = {
      workflowId: "workflow_project_create",
      kind: "project-session-create",
      source: "local-typed-preview",
      stage: "review",
      previewDigest: digest,
      expectedRevision: "1" as never,
      consequenceClass: "consequential",
      confirmationMode: "explicit",
      summary: "project-session-create",
      details: [{ label: "projectSessionId", value: '"session-created"' }],
      evidence: [],
      openedByEventId: "create-event-1",
      armedByEventId: null,
      result: null,
      failure: null,
    };
    const workflowPlanner = {
      prepare: vi.fn(async () => review),
      arm: vi.fn((current: ConsoleWorkflowReview, eventId: string) => ({
        ...current,
        stage: "confirm" as const,
        armedByEventId: eventId,
      })),
      commit: vi.fn(async ({ review: current }: { review: ConsoleWorkflowReview }) => ({
        reconnectProjectSessionId: "session-created" as ProjectSessionId,
        review: {
          ...current,
          stage: "committed" as const,
          result: "project-session-create | session-created | draft | r1",
        },
      })),
    };
    const firstDetach = vi.fn(async () => {});
    const firstClose = vi.fn(async () => {});
    const nextDetach = vi.fn(async () => {});
    const nextClose = vi.fn(async () => {});
    const startOrAttach = vi.fn()
      .mockResolvedValueOnce({
        status: "connected",
        binding: currentBinding(port, false, null),
        credential,
        projectId,
        workflowPlanner,
        detach: firstDetach,
        close: firstClose,
      })
      .mockResolvedValueOnce({
        status: "connected",
        binding: currentBinding(port, false, null),
        credential,
        projectId,
        projectSessionId: "session-created" as ProjectSessionId,
        workflowPlanner,
        detach: nextDetach,
        close: nextClose,
      });
    let sequence = 0;
    const application = await startFabricConsoleApplication({
      bootstrap: { startOrAttach },
      projectRoot: "/repo",
      surface: "standalone",
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      eventId: () => `create-event-${String(++sequence)}`,
      confirmationId: () => "confirmation-create",
      ...runtimeDependencies,
    });

    await application.handleInput({ kind: "key", key: "text", text: ":" });
    await application.handleInput({ kind: "paste", text: "{}" });
    await application.handleInput({ kind: "key", key: "enter" });
    await application.handleInput({ kind: "key", key: "enter" });
    await application.handleInput({ kind: "key", key: "text", text: "3" });

    expect(startOrAttach).toHaveBeenCalledTimes(2);
    expect(startOrAttach).toHaveBeenNthCalledWith(2, expect.objectContaining({
      projectSessionId: "session-created",
    }));
    expect(firstDetach).toHaveBeenCalledWith({ reason: "operator" });
    expect(firstClose).toHaveBeenCalledOnce();
    expect(application.frame.rows.join("\n")).toContain("session-created");

    await application.close("operator");
    expect(nextDetach).toHaveBeenCalledWith({ reason: "operator" });
    expect(nextClose).toHaveBeenCalledOnce();
  });

  it("reads an Activity message only from its exact revision-bound messageBodyRef on activation", async () => {
    const projectSessionId = "session-application" as ProjectSessionId;
    const messageId = "message-exact" as MessageId;
    let staleMessage = false;
    const readMessageBody = vi.fn(async (): Promise<MessageBodyReadResult> => {
      if (staleMessage) {
        throw Object.assign(new Error("message revision changed"), {
          code: "STALE_REVISION",
        });
      }
      return {
        available: true,
        messageId,
        revision: 4,
        body: "Full ordinary body line one.\nSecond line remains readable.",
        terminalNeutralised: true,
        capabilityValuesRedacted: true,
        artifactRefs: [],
      };
    });
    const port: ConsoleProtocolPort = {
      ...protocolPort(),
      readMessageBody,
      readDetail: vi.fn(async () =>
        activityMessageDetail({ projectSessionId, messageId })
      ),
      viewPage: vi.fn(async (request) =>
        activityMessagePage(request, { projectSessionId, messageId }),
      ),
    };
    const bootstrap: ConsoleBootstrapPort = {
      startOrAttach: async () => ({
        status: "connected",
        binding: currentBinding(port, true, null),
        credential,
        projectId,
        projectSessionId,
        detach: async () => {},
        close: async () => {},
      }),
    };
    const application = await startFabricConsoleApplication({
      bootstrap,
      projectRoot: "/repo",
      surface: "standalone",
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      eventId: () => "event-read-message",
      confirmationId: () => "confirmation-unused",
      ...runtimeDependencies,
    });

    await application.handleInput({ kind: "key", key: "text", text: "7" });
    await application.handleInput({ kind: "key", key: "down" });
    await application.handleInput({ kind: "key", key: "enter" });

    expect(readMessageBody).toHaveBeenCalledWith({
      credential,
      projectSessionId,
      messageId,
      expectedRevision: 4,
    });
    expect(application.frame.rows.join("\n")).toContain("Full ordinary body line one.");
    expect(application.frame.presentation.focusId).toBe(
      "detail:activity:activity-group-must-not-be-derived",
    );
    expect(application.frame.rows.join("\n")).toContain(
      "Message: message-exact r4",
    );
    expect(JSON.stringify(readMessageBody.mock.calls)).not.toContain("event-must-not-be-derived");

    staleMessage = true;
    await application.handleInput({ kind: "key", key: "shift-tab" });
    await application.handleInput({ kind: "key", key: "shift-tab" });
    await application.handleInput({ kind: "key", key: "enter" });
    expect(application.dataset.inspection).toMatchObject({
      kind: "activity",
      state: "current",
      messages: [{
        state: "unavailable",
        reason: "projection-changed",
      }],
    });
    await application.close("operator");
  });

  it("keeps the projection live and renders Activity reads honestly when message bodies are unavailable", async () => {
    const projectSessionId = "session-application" as ProjectSessionId;
    const messageId = "message-exact" as MessageId;
    const port: ConsoleProtocolPort = {
      ...protocolPort(),
      readMessageBody: null,
      readDetail: vi.fn(async () =>
        activityMessageDetail({ projectSessionId, messageId })
      ),
      viewPage: vi.fn(async (request) =>
        activityMessagePage(request, { projectSessionId, messageId }),
      ),
    };
    const application = await startFabricConsoleApplication({
      bootstrap: {
        startOrAttach: async () => ({
          status: "connected",
          binding: currentBinding(port, true, null),
          credential,
          projectId,
          projectSessionId,
          detach: async () => {},
          close: async () => {},
        }),
      },
      projectRoot: "/repo",
      surface: "standalone",
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      eventId: () => "event-read-unavailable",
      confirmationId: () => "confirmation-unused",
      ...runtimeDependencies,
    });

    await application.handleInput({ kind: "key", key: "text", text: "7" });
    await application.handleInput({ kind: "key", key: "down" });
    await application.handleInput({ kind: "key", key: "enter" });

    expect(application.dataset.inspection).toMatchObject({
      kind: "activity",
      state: "current",
      messages: [{
        state: "unavailable",
        reason: "feature-unavailable",
      }],
    });
    expect(application.dataset.connection).toStrictEqual({
      state: "live",
      compatibility: { mode: "current" },
    });
    expect(application.dataset.canMutate).toBe(false);
    expect(application.frame.rows.join("\n")).toContain("feature-unavailable");
    await application.close("operator");
  });

  it("reads Project detail then requests and renders the fixed typed repository projection", async () => {
    const projectSessionId = "session-application" as ProjectSessionId;
    let selectedWorktree: string | null = null;
    let corruptDetailIdentity = false;
    let repositoryResnapshotRequired = false;
    const readDetail = vi.fn(async (
      request: OperatorDetailReadRequest,
    ): Promise<OperatorDetailReadResult> => ({
      status: "current",
      detailRef: corruptDetailIdentity
        ? { kind: "project", projectId, expectedRevision: 99 }
        : request.detailRef,
      detail: {
        freshness: "live",
        source: "fabric",
        revision: 3,
        observedAt: timestamp,
        value: {
          kind: "project",
          projectId,
          canonicalRoot: "/repo",
          goal: "Ship typed reads",
          acceptedScopeRef: null,
          repositoryRevision: "repo-r3",
          ...(selectedWorktree === null
            ? {}
            : {
                repository: {
                  ...repositoryProjection(),
                  canonicalWorktreePath: selectedWorktree,
                },
              }),
        },
      },
      snapshotRevision: corruptDetailIdentity
        ? request.snapshotRevision + 1
        : request.snapshotRevision,
      readTransactionId: "project-detail-read",
    }));
    const readRepository = vi.fn(async (
      request: GitRepositoryReadRequest,
    ): Promise<GitRepositoryReadResult> => {
      if (repositoryResnapshotRequired) {
        throw Object.assign(new Error("repository changed during observation"), {
          code: "PROJECTION_RESNAPSHOT_REQUIRED",
        });
      }
      const canonicalWorktreePath = request.target.kind === "project-root"
        ? "/repo"
        : request.target.canonicalWorktreePath;
      const repository = repositoryProjection();
      const currentWorktree = repository.worktrees.items[0];
      if (currentWorktree === undefined) {
        throw new Error("repository fixture requires a current worktree");
      }
      return {
        status: "current",
        projectId,
        projectSessionId,
        snapshotRevision: request.snapshotRevision,
        readTransactionId: "repository-read",
        repository: {
          ...repository,
          canonicalWorktreePath,
          worktrees: {
            items: [{
              ...currentWorktree,
              canonicalPath: canonicalWorktreePath,
            }],
            truncated: false,
          },
        },
      };
    });
    const port: ConsoleProtocolPort = {
      ...protocolPort(),
      readDetail,
      readRepository,
      viewPage: vi.fn(async (request) => {
        if (request.view !== "project") {
          return {
            status: "page",
            view: request.view,
            rows: [],
            nextCursor: 0,
            hasMore: false,
            snapshotRevision: request.snapshotRevision,
            readTransactionId: `page-${request.view}`,
          } as OperatorViewPageResult;
        }
        return {
          status: "page",
          view: "project",
          rows: [{
            itemId: "project-row",
            itemRevision: 3,
            fact: {
              freshness: "live",
              source: "fabric",
              revision: 3,
              observedAt: timestamp,
              value: {
                summary: {
                  kind: "project",
                  goal: "Ship typed reads",
                  acceptedScopeRef: null,
                  repositoryRevision: "repo-r3",
                },
                detailRef: { kind: "project", projectId, expectedRevision: 3 },
                actionAvailability: { state: "read-only", reason: "state-ineligible" },
              },
            },
          }],
          nextCursor: 1,
          hasMore: false,
          snapshotRevision: request.snapshotRevision,
          readTransactionId: "page-project",
        } as OperatorViewPageResult;
      }),
    };
    const application = await startFabricConsoleApplication({
      bootstrap: {
        startOrAttach: async () => ({
          status: "connected",
          binding: currentBinding(port, true, null),
          credential,
          projectId,
          projectSessionId,
          detach: async () => {},
          close: async () => {},
        }),
      },
      projectRoot: "/repo",
      surface: "standalone",
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      eventId: () => "event-read-repository",
      confirmationId: () => "confirmation-unused",
      ...runtimeDependencies,
    });

    await application.handleInput({ kind: "key", key: "text", text: "2" });
    await application.handleInput({ kind: "key", key: "down" });
    await application.handleInput({ kind: "key", key: "enter" });

    expect(readDetail).toHaveBeenCalledWith({
      credential,
      projectId,
      projectSessionId,
      snapshotRevision: 1,
      detailRef: { kind: "project", projectId, expectedRevision: 3 },
    });
    expect(readRepository).toHaveBeenCalledWith({
      credential,
      projectId,
      projectSessionId,
      snapshotRevision: 1,
      target: { kind: "project-root" },
      diff: { kind: "working-tree" },
      log: { limit: 32 },
    });
    const rendered = application.frame.rows.join("\n");
    expect(rendered).toContain("Git: LIVE");
    expect(rendered).toContain("GitHub checks: UNAVAILABLE r1");
    expect(application.dataset.connection).toStrictEqual({
      state: "live",
      compatibility: { mode: "current" },
    });

    selectedWorktree = "/repo/.worktrees/selected";
    await application.handleInput({ kind: "key", key: "shift-tab" });
    await application.handleInput({ kind: "key", key: "shift-tab" });
    await application.handleInput({ kind: "key", key: "enter" });
    expect(readRepository).toHaveBeenLastCalledWith({
      credential,
      projectId,
      projectSessionId,
      snapshotRevision: 1,
      target: {
        kind: "session-worktree",
        canonicalWorktreePath: "/repo/.worktrees/selected",
      },
      diff: { kind: "working-tree" },
      log: { limit: 32 },
    });

    const repositoryReadCount = readRepository.mock.calls.length;
    corruptDetailIdentity = true;
    await application.handleInput({ kind: "key", key: "shift-tab" });
    await application.handleInput({ kind: "key", key: "shift-tab" });
    await application.handleInput({ kind: "key", key: "enter" });
    expect(readRepository).toHaveBeenCalledTimes(repositoryReadCount);
    expect(application.dataset.inspection).toMatchObject({
      kind: "repository",
      state: "unavailable",
      reason: "contract-invalid",
    });

    corruptDetailIdentity = false;
    repositoryResnapshotRequired = true;
    await application.handleInput({ kind: "key", key: "shift-tab" });
    await application.handleInput({ kind: "key", key: "shift-tab" });
    await application.handleInput({ kind: "key", key: "enter" });
    expect(application.dataset.inspection).toMatchObject({
      kind: "repository",
      state: "unavailable",
      reason: "repository-resnapshot-required",
    });
    await application.close("operator");
  });
});
