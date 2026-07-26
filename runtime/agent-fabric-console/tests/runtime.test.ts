import { createHash } from "node:crypto";

import { describe, expect, it, vi } from "vitest";

import type {
  OperatorActionAvailability,
  ProjectId,
  Sha256Digest,
  Timestamp,
} from "@local/agent-fabric-protocol";
import type {
  ActionReview,
  ActionReviewStage,
  ConsoleControllerState,
} from "../src/controller.js";
import {
  FabricConsoleRuntime,
  type FabricRuntimeController,
} from "../src/runtime.js";
import {
  FABRIC_VIEWS,
  createEmptyViewPages,
  revisionFromProtocol,
  type ConsoleRow,
  type FabricView,
} from "../src/model.js";
import { createFabricUiState } from "../src/presenter.js";
import type { FabricConsoleDataset } from "../src/protocol-adapter.js";
import type { ConsoleWorkflowReview } from "../src/workflow.js";
import {
  reduceFabricPointer,
  renderFabricConsoleFrame,
} from "../src/index.js";

const timestamp = "2026-07-11T12:00:00.000Z" as Timestamp;
const digest = (`sha256:${"a".repeat(64)}`) as Sha256Digest;
const nativeNotification = {
  kind: "daemon-journal",
  targetIntegration: "native-desktop",
  status: "available",
  journalState: "sent",
  deliveryItemRevision: 7,
  claimGeneration: null,
  integrationState: "available",
  observedAt: timestamp,
} as const;
const available: OperatorActionAvailability = {
  state: "available",
  actions: ["resume"],
  requiresPreview: true,
};

function longBoundReview(
  label: "Git" | "Launch" | "Promotion",
  previewDigest: string = digest,
  stage: "review" | "confirm" = "review",
): ConsoleWorkflowReview {
  return {
    workflowId: `workflow-${label.toLowerCase()}`,
    kind: "operator-action",
    source: "daemon-preview",
    stage,
    previewDigest,
    expectedRevision: revisionFromProtocol(7),
    consequenceClass: label === "Promotion" ? "promotion" : "consequential",
    confirmationMode: "explicit",
    summary: `${label} exact typed operation`,
    details: Array.from({ length: 18 }, (_, index) => ({
      label: `${label} binding ${String(index + 1)}`,
      value: `${label.toLowerCase()}-${String(index + 1)}-${"bound".repeat(18)}`,
    })),
    evidence: [`evidence/${label.toLowerCase()}-preview.json@${previewDigest}`],
    openedByEventId: `event-${label.toLowerCase()}-open`,
    armedByEventId: null,
    result: null,
    failure: null,
  };
}

function directReview(stage: ActionReviewStage): ActionReview {
  return {
    stage,
    binding: {
      view: "runs",
      itemId: "run:control",
      itemRevision: revisionFromProtocol(7),
      projectionRevision: revisionFromProtocol(11),
    },
    availableAction: "resume",
    preview: {
      previewId: "preview-focus-restoration",
      previewRevision: 3,
      previewDigest: digest,
      intent: {
        kind: "control",
        action: "resume",
        target: {
          kind: "task",
          projectSessionId: "session:control" as never,
          coordinationRunId: "run:control" as never,
          taskId: "task:control" as never,
          expectedRevision: 7,
        },
      },
      intentDigest: digest,
      beforeStateDigest: digest,
      consequenceClass: "consequential",
      evidenceRefs: [],
      gateIds: [],
      confirmationMode: "explicit",
      expiresAt: "2099-07-11T13:00:00.000Z" as Timestamp,
    },
    gates: [],
    openedByEventId: "direct-open",
    armedByEventId: stage === "review" ? null : "direct-arm",
    changes: [],
    status: null,
  };
}

function shortWorkflowReview(
  stage: ConsoleWorkflowReview["stage"],
): ConsoleWorkflowReview {
  return {
    ...longBoundReview("Git", digest, stage === "confirm" ? "confirm" : "review"),
    stage,
    details: [],
    result: stage === "committed" ? "committed exact workflow" : null,
  };
}

function expectEnabledVisibleFocus(
  runtime: FabricConsoleRuntime,
  expectedId?: string,
): void {
  const focusId = runtime.ui.focusId;
  if (expectedId !== undefined) expect(focusId).toBe(expectedId);
  expect(focusId).not.toBeNull();
  const region = runtime.frame.hitRegions.find(
    ({ enabled, id }) => enabled && id === focusId,
  );
  expect(region).toBeDefined();
  if (region === undefined) return;
  const focusedText = runtime.frame.rows
    .slice(region.rect.y1 - 1, region.rect.y2)
    .map((line) => line.slice(region.rect.x1 - 1, region.rect.x2))
    .join("\n");
  expect(focusedText).toContain(">");
}

function fixtureDataset(revision = 11): FabricConsoleDataset {
  const row: ConsoleRow<"attention"> = {
    view: "attention",
    stableId: "attention:1",
    revision: revisionFromProtocol(7),
    urgency: "critical-path",
    freshness: {
      state: "live",
      source: "fabric",
      revision: revisionFromProtocol(7),
      observedAt: timestamp,
      ageMs: 0,
    },
    summary: {
      kind: "attention",
      label: "Blocked",
      priority: "critical-path",
      title: "Resume blocked task",
      nativeNotification,
    },
    detailRef: {
      kind: "system",
      componentId: "attention:1",
      expectedRevision: 7,
    },
    actionAvailability: available,
  };
  const empty = createEmptyViewPages();
  return {
    connection: { state: "live", compatibility: { mode: "current" } },
    snapshot: {
      schemaVersion: 1,
      snapshotRevision: revision,
      readTransactionId: "snapshot-read",
      project: {
        freshness: "live",
        source: "fabric",
        revision,
        observedAt: timestamp,
        value: {
          projectId: "project-1" as ProjectId,
          canonicalRoot: "/repo",
        },
      },
      session: {
        freshness: "unavailable",
        source: "fabric",
        revision,
        observedAt: timestamp,
        reason: "no session",
      },
      runs: {
        freshness: "live",
        source: "fabric",
        revision,
        observedAt: timestamp,
        value: [],
      },
      attention: {
        freshness: "live",
        source: "fabric",
        revision,
        observedAt: timestamp,
        value: [],
      },
      capacity: {
        freshness: "unavailable",
        source: "fabric",
        revision,
        observedAt: timestamp,
        reason: "unknown",
      },
      cursor: revision,
      stateDigest: digest,
    },
    snapshotRevision: revisionFromProtocol(revision),
    cursor: revision,
    pages: {
      ...empty,
      attention: {
        view: "attention",
        rows: [row],
        nextCursor: 1,
        hasMore: false,
        snapshotRevision: revisionFromProtocol(revision),
        readTransactionId: "attention-read",
      },
    },
    loadedAtMs: Date.parse(timestamp),
    canMutate: true,
  };
}

function emptyViewRecord<Value>(value: Value): Record<FabricView, Value> {
  return Object.fromEntries(FABRIC_VIEWS.map((view) => [view, value])) as Record<
    FabricView,
    Value
  >;
}

class FakeController implements FabricRuntimeController {
  dataset = fixtureDataset();
  state: ConsoleControllerState = {
    activeView: "attention",
    selectionByView: {
      ...emptyViewRecord(null),
      attention: {
        stableId: "attention:1",
        revision: revisionFromProtocol(7),
      },
    },
    scrollAnchorByView: emptyViewRecord(null),
    review: null,
    pendingCommandIds: ["pending-1"],
    lastActionStatus: null,
    lastReceipt: null,
  } as ConsoleControllerState;

  activateView(view: FabricView): void {
    this.state = { ...this.state, activeView: view };
  }

  select(view: FabricView, stableId: string): void {
    const selected = this.dataset.pages[view].rows.find(
      (row) => row.stableId === stableId,
    );
    if (selected === undefined) return;
    this.state = {
      ...this.state,
      selectionByView: {
        ...this.state.selectionByView,
        [view]: { stableId, revision: selected.revision },
      },
    };
  }

  setScrollAnchor(view: FabricView, stableId: string | null): void {
    this.state = {
      ...this.state,
      scrollAnchorByView: {
        ...this.state.scrollAnchorByView,
        [view]: stableId,
      },
    };
  }

  updateDataset(dataset: FabricConsoleDataset): void {
    this.dataset = dataset;
  }
}

function stateBoundControlController(): FakeController {
  const controller = new FakeController();
  const run: ConsoleRow<"runs"> = {
    view: "runs",
    stableId: "run:control",
    revision: revisionFromProtocol(7),
    urgency: "critical-path",
    freshness: {
      state: "live",
      source: "fabric",
      revision: revisionFromProtocol(7),
      observedAt: timestamp,
      ageMs: 0,
    },
    summary: {
      kind: "run",
      projectSessionId: "session:control" as never,
      phase: "paused",
      health: "blocked",
      nextMilestone: "Resume exact run",
      declaredProgress: { plan: "open", counts: { blocked: 0, ready: 0, active: 1, complete: 0, cancelled: 0, degraded: 0 } },
      identity: {
        runKind: "coordination",
        chairAgentId: "agent:control-chair" as never,
        acceptedScopeRef: null,
        currentPlanRef: null,
        planRevision: null,
        workstreams: [],
        lastEventAt: timestamp,
      },
    },
    detailRef: {
      kind: "run",
      projectSessionId: "session:control" as never,
      coordinationRunId: "run:control" as never,
      expectedRevision: 7,
    },
    actionAvailability: available,
  };
  controller.dataset = {
    ...controller.dataset,
    pages: {
      ...controller.dataset.pages,
      runs: {
        view: "runs",
        rows: [run],
        nextCursor: 1,
        hasMore: false,
        snapshotRevision: revisionFromProtocol(11),
        readTransactionId: "runs-control-read",
      },
    },
  };
  controller.state = {
    ...controller.state,
    activeView: "runs",
    selectionByView: {
      ...controller.state.selectionByView,
      runs: { stableId: run.stableId, revision: run.revision },
    },
  };
  return controller;
}

describe("Fabric Console runtime routing", () => {
  it("uses s to return an exact session to the project selector", async () => {
    const controller = new FakeController();
    controller.dataset = {
      ...controller.dataset,
      projectSessions: {
        choices: [{
          projectSessionId: "session_switch_01" as never,
          mode: "independent",
          state: "active",
          revision: 1,
          generation: 1,
          lastEventAt: timestamp,
        }],
        selectedProjectSessionId: "session_switch_01" as never,
      },
    };
    const activate = vi.fn(async () => {});
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      detach: async () => {},
      activate,
      eventId: () => "switch-session-event",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    await runtime.handleInput({ kind: "key", key: "text", text: "s" });

    expect(activate).toHaveBeenCalledWith({
      regionId: "session:switch-project",
      binding: null,
      provenance: "keyboard",
      eventId: "switch-session-event",
    });
  });

  it("routes all eight views and paging without stealing editor digits", async () => {
    const controller = new FakeController();
    const setEditorActive = vi.fn();
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: (() => {
        let value = 0;
        return () => `event-${String(++value)}`;
      })(),
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
      setEditorActive,
    });

    for (const [index, view] of FABRIC_VIEWS.entries()) {
      await runtime.handleInput({ kind: "key", key: "text", text: String(index + 1) });
      expect(controller.state.activeView).toBe(view);
    }
    await runtime.handleInput({ kind: "key", key: "text", text: "[" });
    expect(controller.state.activeView).toBe("activity");
    await runtime.handleInput({ kind: "key", key: "text", text: "]" });
    expect(controller.state.activeView).toBe("system");
    await runtime.handleInput({ kind: "key", key: "alt-1" });
    expect(controller.state.activeView).toBe("attention");

    runtime.setInputMode("editor");
    await runtime.handleInput({ kind: "key", key: "text", text: "8" });
    await runtime.handleInput({ kind: "key", key: "page-down" });
    expect(controller.state.activeView).toBe("attention");
    expect(runtime.ui.draft).toBe("8");
    expect(setEditorActive).toHaveBeenCalledWith(true);
    await runtime.handleInput({ kind: "key", key: "escape" });
    expect(setEditorActive).toHaveBeenLastCalledWith(false);
  });

  it("pages a long Deck roster independently and clamps it after refresh", async () => {
    const controller = stateBoundControlController();
    controller.activateView("attention");
    const choices = Array.from({ length: 12 }, (_, index) => ({
      projectSessionId: `session-page-${String(index).padStart(2, "0")}` as never,
      mode: "independent" as const,
      state: "active" as const,
      revision: index + 1,
      generation: 1,
      lastEventAt: timestamp,
    }));
    const activate = vi.fn(async () => {});
    controller.dataset = {
      ...controller.dataset,
      projectSessions: { selectedProjectSessionId: null, choices },
      pages: {
        ...controller.dataset.pages,
        runs: { ...controller.dataset.pages.runs, rows: [] },
      },
    };
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 30, rows: 6 },
      draw: () => {},
      detach: async () => {},
      activate,
      eventId: () => "deck-page",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    const first = runtime.frame.hitRegions.find(({ id }) => id.startsWith("deck:"));
    expect(first).toBeDefined();
    if (first === undefined) return;
    runtime.setFocus(first.id);
    await runtime.handleInput({ kind: "key", key: "enter" });
    expect(activate).not.toHaveBeenCalled();

    const reached = new Set<string>();
    for (let page = 0; page < 20; page += 1) {
      for (const { id } of runtime.frame.hitRegions.filter(
        ({ id }) => id.startsWith("deck:session:"),
      )) reached.add(id);
      const before = runtime.ui.deckScrollOffset;
      await runtime.handleInput({ kind: "key", key: "page-down" });
      if (runtime.ui.deckScrollOffset === before) break;
    }

    expect(reached).toStrictEqual(new Set(choices.map(
      ({ projectSessionId }) => `deck:session:${projectSessionId}`,
    )));
    expect(runtime.ui.deckScrollOffset).toBeGreaterThan(0);
    expect(runtime.ui.scrollOffsetByView.attention ?? 0).toBe(0);

    const focusedId = runtime.ui.focusId;
    const focusedY = runtime.frame.hitRegions.find(({ id }) => id === focusedId)?.rect.y1;
    expect(focusedId).toMatch(/^deck:session:/u);
    expect(focusedY).toBeDefined();
    const inserted = {
      ...choices[0] as (typeof choices)[number],
      projectSessionId: "session-page-inserted" as never,
    };
    runtime.updateDataset({
      ...controller.dataset,
      projectSessions: { selectedProjectSessionId: null, choices: [inserted, ...choices] },
    });
    expect(runtime.ui.focusId).toBe(focusedId);
    expect(runtime.frame.hitRegions.find(({ id }) => id === focusedId)?.rect.y1).toBe(focusedY);

    const focusedStableId = focusedId?.slice("deck:session:".length);
    runtime.updateDataset({
      ...controller.dataset,
      projectSessions: {
        selectedProjectSessionId: null,
        choices: [inserted, ...choices].filter(
          ({ projectSessionId }) => projectSessionId !== focusedStableId,
        ),
      },
    });
    expect(runtime.ui.focusId).not.toBe(focusedId);
    expect(runtime.ui.focusId).toMatch(/^deck:session:/u);
    expect(runtime.ui.notice).toContain("focus moved to the nearest projected roster row");

    runtime.updateDataset({
      ...controller.dataset,
      projectSessions: { selectedProjectSessionId: null, choices: choices.slice(0, 2) },
    });
    expect(runtime.ui.deckScrollOffset).toBe(0);
  });

  it("pages the complete compact Deck roster with visible focus in both directions", async () => {
    const controller = stateBoundControlController();
    controller.activateView("attention");
    const sourceRun = controller.dataset.pages.runs.rows[0];
    if (sourceRun?.summary?.kind !== "run") throw new Error("run fixture unavailable");
    const sourceSummary = sourceRun.summary;
    const choices = Array.from({ length: 4 }, (_, index) => ({
      projectSessionId: `session:page-${String(index)}` as never,
      mode: "coordinated" as const,
      state: "active" as const,
      revision: index + 1,
      generation: 1,
      lastEventAt: timestamp,
    }));
    controller.dataset = {
      ...controller.dataset,
      projectSessions: { selectedProjectSessionId: null, choices },
      pages: {
        ...controller.dataset.pages,
        runs: {
          ...controller.dataset.pages.runs,
          rows: Array.from({ length: 4 }, (_, index) => ({
            ...sourceRun,
            stableId: `run:page-${String(index)}`,
            summary: {
              ...sourceSummary,
              projectSessionId: `session:page-${String(index)}` as never,
              identity: {
                ...sourceSummary.identity,
                workstreams: [{
                  workstreamId: `workstream:page-${String(index)}` as never,
                  deliveryRunId: `delivery:page-${String(index)}` as never,
                  leadAgentId: "agent:page-lead" as never,
                  state: "active" as const,
                  updatedAt: timestamp,
                }],
              },
            },
          })),
        },
      },
    };
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 30, rows: 6 },
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "deck-coordination-page",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    const expected = runtime.frame.presentation.deckRows.map(
      ({ stableId }) => `deck:${stableId}`,
    );
    const first = runtime.frame.hitRegions.find(({ id }) => id.startsWith("deck:"));
    expect(first).toBeDefined();
    if (first === undefined) return;
    const initialFocusId = first.id;
    runtime.setFocus(first.id);

    const reached = new Set<string>();
    for (let page = 0; page <= expected.length; page += 1) {
      const visibleDeckIds = runtime.frame.hitRegions
        .filter(({ id }) => id.startsWith("deck:"))
        .map(({ id }) => id);
      for (const id of visibleDeckIds) {
        reached.add(id);
      }
      expectEnabledVisibleFocus(runtime);
      const before = runtime.ui.deckScrollOffset;
      const beforeFocus = runtime.ui.focusId;
      await runtime.handleInput({ kind: "key", key: "page-down" });
      if (runtime.ui.deckScrollOffset === before && runtime.ui.focusId === beforeFocus) break;
    }
    expect(reached).toStrictEqual(new Set(expected));

    for (let page = 0; page <= expected.length; page += 1) {
      expectEnabledVisibleFocus(runtime);
      const before = runtime.ui.deckScrollOffset;
      const beforeFocus = runtime.ui.focusId;
      await runtime.handleInput({ kind: "key", key: "page-up" });
      if (runtime.ui.deckScrollOffset === before && runtime.ui.focusId === beforeFocus) break;
    }
    expect(runtime.ui.deckScrollOffset).toBe(0);
    expectEnabledVisibleFocus(runtime, initialFocusId);
  });

  it("keeps displayed action numbers stable across disabled and workflow entries", async () => {
    const controller = new FakeController();
    const activate = vi.fn(async () => {});
    const render: typeof renderFabricConsoleFrame = (
      dataset,
      state,
      ui,
      viewport,
    ) => {
      const frame = renderFabricConsoleFrame(dataset, state, ui, viewport);
      const binding = frame.hitRegions.find(({ kind }) => kind === "action")?.binding ?? null;
      const y = frame.rows.length - 2;
      const actions = [
        {
          id: "workflow:discuss",
          label: "Discuss",
          enabled: false,
          availableAction: null,
          reason: "daemon-chair-request-preparation-unavailable",
        },
        {
          id: "workflow:accept",
          label: "Accept",
          enabled: true,
          availableAction: null,
        },
      ] as const;
      const rows = [...frame.rows];
      const focusMarkerColumn = ui.focusId === actions[0].id
        ? 0
        : ui.focusId === actions[1].id
          ? 13
          : null;
      const actionRow = rows[y - 1];
      if (focusMarkerColumn !== null && actionRow !== undefined) {
        rows[y - 1] = `${actionRow.slice(0, focusMarkerColumn)}>${actionRow.slice(focusMarkerColumn + 1)}`;
      }
      return {
        ...frame,
        rows,
        presentation: { ...frame.presentation, actions, focusId: ui.focusId },
        hitRegions: [
          ...frame.hitRegions.filter(({ kind }) => kind !== "action"),
          {
            id: actions[0].id,
            kind: "action" as const,
            rect: { x1: 1, y1: y, x2: 12, y2: y },
            enabled: false,
            geometryKey: frame.geometryKey,
            binding,
          },
          {
            id: actions[1].id,
            kind: "action" as const,
            rect: { x1: 14, y1: y, x2: 24, y2: y },
            enabled: true,
            geometryKey: frame.geometryKey,
            binding,
          },
        ],
      };
    };
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({ focusId: "workflow:accept" }),
      draw: () => {},
      detach: async () => {},
      activate,
      eventId: () => "numbered-workflow-action",
      render,
      reducePointer: reduceFabricPointer,
    });

    await runtime.handleInput({ kind: "key", key: "text", text: "1" });
    expect(activate).not.toHaveBeenCalled();
    expect(runtime.ui.notice).toBe(
      "Action unavailable: daemon-chair-request-preparation-unavailable",
    );
    expect(controller.state.activeView).toBe("attention");
    await runtime.handleInput({ kind: "key", key: "text", text: "2" });
    expect(activate).toHaveBeenCalledOnce();
    expect(activate).toHaveBeenCalledWith(expect.objectContaining({
      regionId: "workflow:accept",
      provenance: "keyboard",
    }));
    expect(controller.state.activeView).toBe("attention");
  });

  it("opens the typed workflow palette and submits inert JSON only to Review", async () => {
    const activate = vi.fn(async () => {});
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      detach: async () => {},
      activate,
      eventId: () => "palette-event",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    await runtime.handleInput({ kind: "key", key: "text", text: ":" });
    expect(runtime.ui.inputMode).toBe("palette");
    await runtime.handleInput({
      kind: "paste",
      text: '{"kind":"intake-draft-create","request":{"summary":"Discuss first"}}',
    });
    expect(activate).not.toHaveBeenCalled();
    expect(runtime.frame.rows.join("\n")).toContain("WORKFLOW JSON:");
    expect(runtime.frame.rows.join("\n")).toContain("Enter opens Review");

    await runtime.handleInput({ kind: "key", key: "enter" });

    expect(activate).toHaveBeenCalledWith({
      regionId: "palette:submit",
      binding: null,
      provenance: "keyboard",
      eventId: "palette-event",
    });
    expect(runtime.ui.inputMode).toBe("browse");
  });

  it("commits filter text in a distinct mode without emitting an action", async () => {
    const activate = vi.fn(async () => {});
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      detach: async () => {},
      activate,
      eventId: () => "filter-event",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    await runtime.handleInput({ kind: "key", key: "text", text: "/" });
    expect(runtime.ui.inputMode).toBe("filter");
    await runtime.handleInput({
      kind: "paste",
      text: "status:degraded session:control approve",
    });
    expect(runtime.ui.filterDraft).toBe("status:degraded session:control approve");
    expect(runtime.ui.draft).toBe("");
    expect(activate).not.toHaveBeenCalled();

    await runtime.handleInput({ kind: "key", key: "enter" });

    expect(runtime.ui).toMatchObject({
      inputMode: "browse",
      filterQuery: "status:degraded session:control approve",
      filterDraft: "status:degraded session:control approve",
    });
    expect(activate).not.toHaveBeenCalled();

    await runtime.handleInput({ kind: "key", key: "text", text: "/" });
    await runtime.handleInput({ kind: "paste", text: " status:urgent" });
    await runtime.handleInput({ kind: "key", key: "escape" });
    expect(runtime.ui).toMatchObject({
      inputMode: "browse",
      filterQuery: "status:degraded session:control approve",
      filterDraft: "status:degraded session:control approve",
    });
    expect(activate).not.toHaveBeenCalled();
  });

  it("toggles the focused Deck row pin locally without emitting an action", async () => {
    const activate = vi.fn(async () => {});
    const focusId = "row:attention:attention:1";
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({ focusId }),
      draw: () => {},
      detach: async () => {},
      activate,
      eventId: () => "pin-event",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    await runtime.handleInput({ kind: "key", key: "text", text: "p" });
    expect(runtime.ui.pinnedRowIds).toStrictEqual([focusId]);
    expect(runtime.frame.rows.join("\n")).toContain("PINNED");
    expect(activate).not.toHaveBeenCalled();

    await runtime.handleInput({ kind: "key", key: "text", text: "p" });
    expect(runtime.ui.pinnedRowIds).toStrictEqual([]);
    expect(activate).not.toHaveBeenCalled();
    expect(createFabricUiState()).toMatchObject({
      filterDraft: "",
      filterQuery: "",
      pinnedRowIds: [],
    });
  });

  it("moves arrows only through visible filtered Attention rows", async () => {
    const controller = new FakeController();
    const template = controller.dataset.pages.attention.rows[0];
    if (template?.summary?.kind !== "attention") {
      throw new Error("Attention fixture unavailable");
    }
    const attentionRows = [
      { ...template, stableId: "attention:visible-1", summary: { ...template.summary, title: "visible match one" } },
      { ...template, stableId: "attention:hidden", summary: { ...template.summary, title: "hidden row" } },
      { ...template, stableId: "attention:visible-2", summary: { ...template.summary, title: "visible match two" } },
    ];
    controller.dataset = {
      ...controller.dataset,
      pages: {
        ...controller.dataset.pages,
        attention: { ...controller.dataset.pages.attention, rows: attentionRows },
      },
    };
    controller.state = {
      ...controller.state,
      selectionByView: {
        ...controller.state.selectionByView,
        attention: { stableId: "attention:visible-1", revision: template.revision },
      },
    };
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({
        filterQuery: "visible match",
        focusId: "row:attention:attention:visible-1",
      }),
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "filtered-arrow",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    await runtime.handleInput({ kind: "key", key: "down" });

    expect(controller.state.selectionByView.attention?.stableId)
      .toBe("attention:visible-2");
    expect(runtime.ui.focusId).toBe("row:attention:attention:visible-2");
  });

  it("hides detail and actions for a selected Watch row hidden by the filter", async () => {
    const controller = new FakeController();
    const template = controller.dataset.pages.attention.rows[0];
    if (template?.summary?.kind !== "attention") {
      throw new Error("Attention fixture unavailable");
    }
    const watchRow = {
      ...template,
      stableId: "attention:watch-hidden",
      urgency: "normal" as const,
      summary: { ...template.summary, title: "watch-secret detail" },
    };
    controller.dataset = {
      ...controller.dataset,
      pages: {
        ...controller.dataset.pages,
        attention: { ...controller.dataset.pages.attention, rows: [template, watchRow] },
      },
    };
    controller.state = {
      ...controller.state,
      selectionByView: {
        ...controller.state.selectionByView,
        attention: { stableId: "attention:watch-hidden", revision: watchRow.revision },
      },
    };
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({ filterQuery: "status:urgent" }),
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "hidden-watch-selection",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    await runtime.handleInput({ kind: "key", key: "escape" });
    expect(runtime.frame.rows.join("\n")).not.toContain("watch-secret");
  });

  it("keeps a same-burst palette opener and payload in modal input", async () => {
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "same-burst-palette-input",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    const open = runtime.handleInput({ kind: "key", key: "text", text: ":" });
    const payload = runtime.handleInput({ kind: "paste", text: "{\"kind\":\"draft\"}" });
    await Promise.all([open, payload]);

    expect(runtime.ui.inputMode).toBe("palette");
    expect(runtime.ui.draft).toBe("{\"kind\":\"draft\"}");
  });

  it("collects an echo confirmation as inert editor text before activation", async () => {
    const setEditorActive = vi.fn();
    const activate = vi.fn(async () => {});
    const opener = "row:attention:attention:1";
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({ focusId: opener }),
      draw: () => {},
      detach: async () => {},
      activate,
      eventId: () => "echo-event",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
      setEditorActive,
    });
    runtime.setWorkflowReview({
      workflowId: "workflow_echo",
      kind: "operator-action",
      source: "daemon-preview",
      stage: "confirm",
      previewDigest: digest,
      expectedRevision: revisionFromProtocol(7),
      consequenceClass: "consequential",
      confirmationMode: "echo",
      summary: "operator-action:project-session-stop",
      details: [],
      evidence: [],
      openedByEventId: "echo-open",
      armedByEventId: "echo-arm",
      result: null,
      failure: null,
    });

    expect(runtime.ui.inputMode).toBe("editor");
    expect(runtime.ui.draft).toBe("");
    expectEnabledVisibleFocus(runtime, "input:editor");
    await runtime.handleInput({ kind: "paste", text: digest });
    expect(runtime.ui.draft).toBe(digest);
    expect(activate).not.toHaveBeenCalled();
    await runtime.handleInput({ kind: "key", key: "escape" });
    expect(runtime.ui.inputMode).toBe("browse");
    expectEnabledVisibleFocus(runtime, "review:cancel");
    runtime.setWorkflowReview({
      ...shortWorkflowReview("committed"),
      confirmationMode: "echo",
    });
    expectEnabledVisibleFocus(runtime, "review:close");
    runtime.setWorkflowReview(null);
    expectEnabledVisibleFocus(runtime, opener);
    expect(setEditorActive).toHaveBeenLastCalledWith(false);
  });

  it.each([
    ["cancel", "review"],
    ["close", "committed"],
  ] as const)(
    "restores workflow Review opener focus after %s",
    (_exit, stage) => {
      const opener = "row:attention:attention:1";
      const runtime = new FabricConsoleRuntime({
        controller: new FakeController(),
        viewport: { columns: 80, rows: 24 },
        ui: createFabricUiState({ focusId: opener }),
        draw: () => {},
        detach: async () => {},
        activate: async () => {},
        eventId: () => "workflow-focus-restoration",
        render: renderFabricConsoleFrame,
        reducePointer: reduceFabricPointer,
      });

      runtime.setWorkflowReview(shortWorkflowReview(stage));
      expectEnabledVisibleFocus(
        runtime,
        stage === "review" ? "review:continue" : "review:close",
      );
      runtime.setWorkflowReview(null);

      expectEnabledVisibleFocus(runtime, opener);
    },
  );

  it("keeps workflow commit focus enabled, then restores its opener on close", () => {
    const opener = "row:attention:attention:1";
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({ focusId: opener }),
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "workflow-commit-focus-restoration",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    runtime.setWorkflowReview(shortWorkflowReview("confirm"));
    expectEnabledVisibleFocus(runtime, "review:cancel");
    runtime.setWorkflowReview(shortWorkflowReview("committed"));
    expectEnabledVisibleFocus(runtime, "review:close");
    runtime.setWorkflowReview(null);

    expectEnabledVisibleFocus(runtime, opener);
  });

  it("moves focus when a workflow Review opener is invalidated after close", () => {
    const controller = stateBoundControlController();
    const opener = "action:resume";
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({ focusId: opener }),
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "workflow-post-close-focus-reconciliation",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    runtime.setWorkflowReview(shortWorkflowReview("committed"));
    runtime.setWorkflowReview(null);
    expectEnabledVisibleFocus(runtime, opener);

    runtime.updateDataset({ ...controller.dataset, canMutate: false });

    expect(runtime.ui.focusId).not.toBe(opener);
    expectEnabledVisibleFocus(runtime);
  });

  it("carries a guided workflow opener through Review and restores it on cancel", () => {
    const opener = "row:attention:attention:1";
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({ focusId: opener }),
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "guided-review-focus-restoration",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    runtime.beginGuidedWorkflow({
      action: "discuss",
      binding: {
        view: "attention",
        itemId: "attention:1",
        itemRevision: revisionFromProtocol(7),
        projectionRevision: revisionFromProtocol(11),
      },
      prompt: "Discuss exact item",
    });
    runtime.setWorkflowReview(shortWorkflowReview("review"));
    runtime.setWorkflowReview(null);

    expectEnabledVisibleFocus(runtime, opener);
  });

  it.each([
    ["cancel", "review"],
    ["close", "committed"],
  ] as const)(
    "restores direct Review opener focus after %s",
    (_exit, stage) => {
      const controller = stateBoundControlController();
      const opener = "action:resume";
      const runtime = new FabricConsoleRuntime({
        controller,
        viewport: { columns: 80, rows: 24 },
        ui: createFabricUiState({ focusId: opener }),
        draw: () => {},
        detach: async () => {},
        activate: async () => {},
        eventId: () => "direct-focus-restoration",
        render: renderFabricConsoleFrame,
        reducePointer: reduceFabricPointer,
      });

      controller.state = { ...controller.state, review: directReview(stage) };
      runtime.repaint();
      expectEnabledVisibleFocus(
        runtime,
        stage === "review" ? "review:continue" : "review:close",
      );
      controller.state = { ...controller.state, review: null };
      runtime.repaint();

      expectEnabledVisibleFocus(runtime, opener);
    },
  );

  it("keeps direct commit focus enabled and falls back safely when its opener vanished", () => {
    const controller = stateBoundControlController();
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({ focusId: "action:resume" }),
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "direct-commit-focus-restoration",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    controller.state = { ...controller.state, review: directReview("confirm") };
    runtime.repaint();
    expectEnabledVisibleFocus(runtime, "review:cancel");
    controller.state = { ...controller.state, review: directReview("committed") };
    runtime.repaint();
    expectEnabledVisibleFocus(runtime, "review:close");

    controller.dataset = { ...controller.dataset, canMutate: false };
    controller.state = { ...controller.state, review: null };
    runtime.repaint();

    expect(runtime.ui.focusId).not.toBe("action:resume");
    expectEnabledVisibleFocus(runtime);
  });

  it("moves focus when a direct Review opener is invalidated after close", () => {
    const controller = stateBoundControlController();
    const opener = "action:resume";
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({ focusId: opener }),
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "direct-post-close-focus-reconciliation",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    controller.state = { ...controller.state, review: directReview("committed") };
    runtime.repaint();
    controller.state = { ...controller.state, review: null };
    runtime.repaint();
    expectEnabledVisibleFocus(runtime, opener);

    runtime.updateDataset({ ...controller.dataset, canMutate: false });

    expect(runtime.ui.focusId).not.toBe(opener);
    expectEnabledVisibleFocus(runtime);
  });

  it.each([
    ["Git", "review", "review:continue"],
    ["Git", "confirm", "review:confirm"],
    ["Launch", "review", "review:continue"],
    ["Launch", "confirm", "review:confirm"],
    ["Promotion", "review", "review:continue"],
    ["Promotion", "confirm", "review:confirm"],
  ] as const)(
    "unlocks a rich 80x24 %s %s only after contiguous overlapping pages were displayed",
    async (label, stage, actionId) => {
      const runtime = new FabricConsoleRuntime({
        controller: new FakeController(),
        viewport: { columns: 80, rows: 24 },
        draw: () => {},
        detach: async () => {},
        activate: async () => {},
        eventId: () => `event-${label.toLowerCase()}-coverage`,
        render: renderFabricConsoleFrame,
        reducePointer: reduceFabricPointer,
      });
      runtime.setWorkflowReview(longBoundReview(label, digest, stage));

      expect(runtime.frame.hitRegions.some(({ id }) => id === actionId))
        .toBe(false);
      await runtime.handleInput({ kind: "key", key: "end" });
      expect(runtime.frame.hitRegions.some(({ id }) => id === actionId))
        .toBe(false);
      await runtime.handleInput({ kind: "key", key: "home" });

      for (let page = 0; page < 80; page += 1) {
        if (runtime.frame.hitRegions.some(({ id, enabled }) =>
          id === actionId && enabled
        )) break;
        await runtime.handleInput({ kind: "key", key: "page-down" });
      }

      expect(runtime.frame.hitRegions.find(({ id }) => id === actionId))
        .toMatchObject({ enabled: true });
      const completed = runtime.ui.reviewCoverage;
      runtime.repaint();
      runtime.resize({ columns: 96, rows: 28 });
      expect(runtime.ui.reviewCoverage).toStrictEqual(completed);
      expect(runtime.frame.hitRegions.find(({ id }) => id === actionId))
        .toMatchObject({ enabled: true });
    },
  );

  it("does not roll a repeated Space from Review into consequential confirmation", async () => {
    const activations: string[] = [];
    let runtime: FabricConsoleRuntime;
    runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      detach: async () => {},
      activate: async ({ regionId }) => {
        activations.push(regionId);
        if (regionId === "review:continue") {
          runtime.setWorkflowReview(shortWorkflowReview("confirm"));
        }
      },
      eventId: () => "repeat-space-confirmation",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    runtime.setWorkflowReview(shortWorkflowReview("review"));

    const first = runtime.handleInput({ kind: "key", key: "space" });
    const repeated = runtime.handleInput({ kind: "key", key: "space" });
    await Promise.all([first, repeated]);

    expect(activations).toStrictEqual(["review:continue"]);
    expect(activations).not.toContain("review:confirm");
    expect(runtime.ui.notice).toContain("Stale Review input ignored");
    expectEnabledVisibleFocus(runtime, "review:cancel");
  });

  it("rejects a queued underlying action after its first input opens Review", async () => {
    const activations: string[] = [];
    let runtime: FabricConsoleRuntime;
    runtime = new FabricConsoleRuntime({
      controller: stateBoundControlController(),
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({ focusId: "action:resume" }),
      draw: () => {},
      detach: async () => {},
      activate: async ({ regionId }) => {
        activations.push(regionId);
        runtime.setWorkflowReview(shortWorkflowReview("review"));
      },
      eventId: () => "underlying-action-review-epoch",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    const first = runtime.handleInput({ kind: "key", key: "enter" });
    const queuedRepeat = runtime.handleInput({ kind: "key", key: "enter" });
    await Promise.all([first, queuedRepeat]);

    expect(activations).toStrictEqual(["action:resume"]);
    expect(runtime.ui.notice).toContain("Stale Review input ignored");
  });

  it("binds numbered Review actions to the stage in which they were received", async () => {
    const activations: string[] = [];
    let runtime: FabricConsoleRuntime;
    runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      detach: async () => {},
      activate: async ({ regionId }) => {
        activations.push(regionId);
        if (regionId === "review:continue") {
          runtime.setWorkflowReview(shortWorkflowReview("confirm"));
        }
      },
      eventId: () => "stable-review-shortcuts",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    runtime.setWorkflowReview(shortWorkflowReview("review"));
    expect(runtime.frame.rows.join("\n")).toContain("[1 Continue to confirmation]");
    expect(runtime.frame.rows.join("\n")).toContain("[2 Cancel Review]");
    expect(runtime.frame.hitRegions.find(({ id }) => id === "review:continue"))
      .toMatchObject({ shortcut: "1" });
    expect(runtime.frame.hitRegions.find(({ id }) => id === "review:cancel"))
      .toMatchObject({ shortcut: "2" });

    const continueInput = runtime.handleInput({ kind: "key", key: "text", text: "1" });
    const queuedCancel = runtime.handleInput({ kind: "key", key: "text", text: "2" });
    const queuedUnseenConfirm = runtime.handleInput({
      kind: "key",
      key: "text",
      text: "3",
    });
    await Promise.all([continueInput, queuedCancel, queuedUnseenConfirm]);

    expect(activations).toStrictEqual(["review:continue"]);
    expect(activations).not.toContain("review:confirm");
    expect(runtime.ui.notice).toContain("Stale Review input ignored");
    expect(runtime.frame.rows.join("\n")).toContain("[2 Cancel Review]");
    expect(runtime.frame.rows.join("\n")).toContain("[3 Confirm workflow]");

    await runtime.handleInput({ kind: "key", key: "text", text: "3" });
    expect(activations).toStrictEqual(["review:continue", "review:confirm"]);
  });

  it("rejects a Review action received against pre-resize hit geometry", async () => {
    const activate = vi.fn(async () => {});
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      detach: async () => {},
      activate,
      eventId: () => "pre-resize-review-input",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    runtime.setWorkflowReview(shortWorkflowReview("review"));

    const queued = runtime.handleInput({ kind: "key", key: "text", text: "1" });
    runtime.resize({ columns: 96, rows: 28 });
    await queued;

    expect(activate).not.toHaveBeenCalled();
    expect(runtime.ui.notice).toContain("Stale Review input ignored");
  });

  it("blocks ambient confirmation keys only on Confirm, not safe controls", async () => {
    const activate = vi.fn(async () => {});
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      detach: async () => {},
      activate,
      eventId: () => "confirm-safe-controls",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    runtime.setWorkflowReview(shortWorkflowReview("confirm"));
    expectEnabledVisibleFocus(runtime, "review:cancel");

    await runtime.handleInput({ kind: "key", key: "space" });
    expect(activate).toHaveBeenCalledWith(expect.objectContaining({
      regionId: "review:cancel",
    }));
    runtime.setFocus("review:confirm");
    await runtime.handleInput({ kind: "key", key: "space" });
    expect(activate).toHaveBeenCalledTimes(1);
    expect(runtime.ui.notice).toContain("explicit numbered confirmation");
  });

  it("does not remap a queued Review click after the stage changes", async () => {
    const activations: string[] = [];
    let runtime: FabricConsoleRuntime;
    runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({ mouseCapture: true }),
      draw: () => {},
      detach: async () => {},
      activate: async ({ regionId }) => {
        activations.push(regionId);
        if (regionId === "review:continue") {
          runtime.setWorkflowReview(shortWorkflowReview("confirm"));
        }
      },
      eventId: () => "stale-review-click",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    runtime.setWorkflowReview(shortWorkflowReview("review"));
    const cancel = runtime.frame.hitRegions.find(
      ({ id }) => id === "review:cancel",
    );
    expect(cancel).toBeDefined();
    if (cancel === undefined) return;
    const mouse = {
      kind: "mouse" as const,
      button: "left" as const,
      x: cancel.rect.x1,
      y: cancel.rect.y1,
      modifiers: { shift: false, alt: false, ctrl: false },
    };

    const continueInput = runtime.handleInput({ kind: "key", key: "text", text: "1" });
    const queuedPress = runtime.handleInput({ ...mouse, phase: "press" });
    const queuedRelease = runtime.handleInput({ ...mouse, phase: "release" });
    await Promise.all([continueInput, queuedPress, queuedRelease]);

    expect(activations).toStrictEqual(["review:continue"]);
    expect(runtime.ui.notice).toContain("Stale Review input ignored");
  });

  it("does not execute a queued view-tab click beneath a newly opened Review", async () => {
    const controller = stateBoundControlController();
    const activations: string[] = [];
    let runtime: FabricConsoleRuntime;
    runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({ focusId: "action:resume", mouseCapture: true }),
      draw: () => {},
      detach: async () => {},
      activate: async ({ regionId }) => {
        activations.push(regionId);
        if (regionId === "action:resume") {
          runtime.setWorkflowReview(shortWorkflowReview("review"));
        }
      },
      eventId: () => "stale-tab-beneath-review",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    const projectTab = runtime.frame.hitRegions.find(
      ({ id }) => id === "view:project",
    );
    expect(projectTab).toBeDefined();
    if (projectTab === undefined) return;
    const mouse = {
      kind: "mouse" as const,
      button: "left" as const,
      x: projectTab.rect.x1,
      y: projectTab.rect.y1,
      modifiers: { shift: false, alt: false, ctrl: false },
    };

    const openReview = runtime.handleInput({ kind: "key", key: "enter" });
    const queuedPress = runtime.handleInput({ ...mouse, phase: "press" });
    const queuedRelease = runtime.handleInput({ ...mouse, phase: "release" });
    await Promise.all([openReview, queuedPress, queuedRelease]);

    expect(controller.state.activeView).toBe("runs");
    expect(activations).toStrictEqual(["action:resume"]);
    expect(runtime.ui.notice).toContain("Stale Review input ignored");
  });

  it("does not execute a queued bound-row click beneath a newly opened Review", async () => {
    const controller = stateBoundControlController();
    const activations: string[] = [];
    let runtime: FabricConsoleRuntime;
    runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({ focusId: "action:resume", mouseCapture: true }),
      draw: () => {},
      detach: async () => {},
      activate: async ({ regionId }) => {
        activations.push(regionId);
        if (regionId === "action:resume") {
          runtime.setWorkflowReview(shortWorkflowReview("review"));
        }
      },
      eventId: () => "stale-row-beneath-review",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    const row = runtime.frame.hitRegions.find(
      ({ id }) => id === "row:runs:run:control",
    );
    expect(row).toBeDefined();
    if (row === undefined) return;
    const mouse = {
      kind: "mouse" as const,
      button: "left" as const,
      x: row.rect.x1,
      y: row.rect.y1,
      modifiers: { shift: false, alt: false, ctrl: false },
    };

    const openReview = runtime.handleInput({ kind: "key", key: "enter" });
    const queuedPress = runtime.handleInput({ ...mouse, phase: "press" });
    const queuedRelease = runtime.handleInput({ ...mouse, phase: "release" });
    await Promise.all([openReview, queuedPress, queuedRelease]);

    expect(controller.state.scrollAnchorByView.runs).toBeNull();
    expect(activations).toStrictEqual(["action:resume"]);
    expect(runtime.ui.notice).toContain("Stale Review input ignored");
  });

  it("anchors an incomplete Review by content across widening and continues without Home", async () => {
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 40, rows: 12 },
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "review-content-anchor-resize",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    runtime.setWorkflowReview(longBoundReview("Git"));
    for (let page = 0; page < 2; page += 1) {
      await runtime.handleInput({ kind: "key", key: "page-down" });
    }
    const before = runtime.frame.reviewCoverage;
    expect(before).toBeDefined();
    if (before === undefined || before === null) return;
    expect(before.coveredThrough).toBeLessThan(before.requiredEnd);

    runtime.resize({ columns: 120, rows: 12 });

    const widened = runtime.frame.reviewCoverage;
    expect(widened).toBeDefined();
    if (widened === undefined || widened === null) return;
    expect(widened.visibleStart).toBeLessThanOrEqual(before.visibleStart);
    expect(widened.visibleEnd).toBeGreaterThan(before.visibleStart);
    expect(runtime.ui.reviewCoverage?.coveredThrough).toBeGreaterThanOrEqual(
      before.coveredThrough,
    );
    for (let page = 0; page < 80; page += 1) {
      if (runtime.frame.hitRegions.some(
        ({ enabled, id }) => enabled && id === "review:continue",
      )) break;
      await runtime.handleInput({ kind: "key", key: "page-down" });
    }
    expect(runtime.frame.hitRegions.find(({ id }) => id === "review:continue"))
      .toMatchObject({ enabled: true });
  });

  it("explains contiguous Review coverage and the non-unlocking End preview", async () => {
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "review-progress-chrome",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    runtime.setWorkflowReview(longBoundReview("Git"));

    expect(runtime.frame.rows.join("\n")).toMatch(/Context read \d+\/\d+ chars/u);
    expect(runtime.frame.rows.join("\n")).toContain("Home + PgDn unlocks");
    await runtime.handleInput({ kind: "key", key: "end" });
    expect(runtime.frame.rows.join("\n")).toContain("End previews only");
    expect(runtime.frame.hitRegions.some(({ id }) => id === "review:continue"))
      .toBe(false);
  });

  it("keeps compact Review lock progress visible at 30x6", async () => {
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 30, rows: 6 },
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "strip-review-progress-chrome",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    runtime.setWorkflowReview(longBoundReview("Git"));

    expect(runtime.frame.rows.join("\n")).toMatch(/C\d+\/\d+ LOCK PgDn/u);
    await runtime.handleInput({ kind: "key", key: "end" });
    expect(runtime.frame.rows.join("\n")).toContain("End previews only");
    expect(runtime.frame.hitRegions.some(({ id }) => id === "review:continue"))
      .toBe(false);
  });

  it("keeps both explicit confirmation bindings reachable at 30x6", async () => {
    const activate = vi.fn(async () => {});
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 30, rows: 6 },
      draw: () => {},
      detach: async () => {},
      activate,
      eventId: () => "strip-confirm-actions",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    runtime.setWorkflowReview(shortWorkflowReview("confirm"));
    for (let page = 0; page < 80; page += 1) {
      if (runtime.ui.reviewCoverage !== null &&
          runtime.ui.reviewCoverage.coveredThrough >= runtime.ui.reviewCoverage.requiredEnd) break;
      await runtime.handleInput({ kind: "key", key: "page-down" });
    }

    expect(runtime.frame.hitRegions.find(({ id }) => id === "review:cancel"))
      .toMatchObject({ enabled: true });
    expect(runtime.frame.hitRegions.find(({ id }) => id === "review:confirm"))
      .toMatchObject({ enabled: true });
    await runtime.handleInput({ kind: "key", key: "text", text: "3" });
    expect(activate).toHaveBeenCalledOnce();
    expect(activate).toHaveBeenCalledWith(expect.objectContaining({
      regionId: "review:confirm",
      provenance: "keyboard",
    }));
  });

  it.each(["revision", "digest"] as const)(
    "resets cumulative review coverage when a stale %s is replaced",
    async (changedBinding) => {
      const runtime = new FabricConsoleRuntime({
        controller: new FakeController(),
        viewport: { columns: 80, rows: 24 },
        draw: () => {},
        detach: async () => {},
        activate: async () => {},
        eventId: () => "event-stale-review-coverage",
        render: renderFabricConsoleFrame,
        reducePointer: reduceFabricPointer,
      });
      const current = longBoundReview("Git");
      runtime.setWorkflowReview(current);
      for (let page = 0; page < 80; page += 1) {
        if (runtime.frame.hitRegions.some(({ id }) => id === "review:continue")) break;
        await runtime.handleInput({ kind: "key", key: "page-down" });
      }
      expect(runtime.frame.hitRegions.some(({ id }) => id === "review:continue"))
        .toBe(true);

      runtime.setWorkflowReview({
        ...current,
        ...(changedBinding === "digest"
          ? { previewDigest: `sha256:${"b".repeat(64)}` }
          : { expectedRevision: revisionFromProtocol(8) }),
      });

      expect(runtime.ui.reviewCoverage?.coveredThrough).toBeLessThan(
        runtime.ui.reviewCoverage?.requiredEnd ?? 0,
      );
      expect(runtime.frame.hitRegions.some(({ id }) => id === "review:continue"))
        .toBe(false);
    },
  );

  it("keeps bounded raw drafts, makes paste inert for actions and surfaces input drops", async () => {
    const activate = vi.fn(async () => {});
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({ inputMode: "editor" }),
      maxDraftBytes: 16,
      draw: () => {},
      detach: async () => {},
      activate,
      eventId: () => "event-1",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    await runtime.handleInput({
      kind: "paste",
      text: "q\nconfirm\u001b[31m",
    });
    expect(Buffer.byteLength(runtime.ui.draft)).toBeLessThanOrEqual(16);
    expect(runtime.ui.draft).toContain("\u001b");
    expect(activate).not.toHaveBeenCalled();

    await runtime.handleInput({ kind: "rejected", reason: "sequence-overflow" });
    await runtime.handleInput({ kind: "rejected", reason: "chunk-overflow" });
    expect(runtime.ui.rejectedInputCount).toBe(2);
    expect(runtime.ui.notice).toMatch(/^Input dropped \(2\): chunk-overflow/);
    expect(runtime.frame.rows.some((row) => row.includes("Input dropped"))).toBe(true);
  });

  it("uses the same bound activation for keyboard and mouse while preserving selection gesture", async () => {
    const activations: unknown[] = [];
    const controller = stateBoundControlController();
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({
        focusId: "action:resume",
        mouseCapture: true,
      }),
      draw: () => {},
      detach: async () => {},
      activate: async (activation) => {
        activations.push(activation);
      },
      eventId: (() => {
        let value = 0;
        return () => `event-${String(++value)}`;
      })(),
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    await runtime.handleInput({ kind: "key", key: "enter" });
    const action = runtime.frame.hitRegions.find(({ id }) => id === "action:resume");
    expect(action).toBeDefined();
    if (action === undefined) return;
    await runtime.handleInput({
      kind: "mouse",
      phase: "press",
      button: "left",
      x: action.rect.x1,
      y: action.rect.y1,
      modifiers: { shift: false, alt: false, ctrl: false },
    });
    await runtime.handleInput({
      kind: "mouse",
      phase: "release",
      button: "left",
      x: action.rect.x1,
      y: action.rect.y1,
      modifiers: { shift: false, alt: false, ctrl: false },
    });
    await runtime.handleInput({
      kind: "mouse",
      phase: "press",
      button: "left",
      x: action.rect.x1,
      y: action.rect.y1,
      modifiers: { shift: true, alt: false, ctrl: false },
    });

    expect(activations).toHaveLength(2);
    expect(activations).toEqual([
      expect.objectContaining({ regionId: "action:resume", provenance: "keyboard" }),
      expect.objectContaining({ regionId: "action:resume", provenance: "mouse" }),
    ]);
    expect(activations[0]).toMatchObject({ binding: action.binding });
    expect(activations[1]).toMatchObject({ binding: action.binding });
  });

  it("uses the same exact row binding for keyboard and mouse reads", async () => {
    const activations: unknown[] = [];
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({
        focusId: "row:attention:attention:1",
        mouseCapture: true,
      }),
      draw: () => {},
      detach: async () => {},
      activate: async (activation) => {
        activations.push(activation);
      },
      eventId: (() => {
        let value = 0;
        return () => `event-row-${String(++value)}`;
      })(),
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    const row = runtime.frame.hitRegions.find(
      ({ id }) => id === "row:attention:attention:1",
    );
    expect(row).toBeDefined();
    if (row === undefined) return;
    await runtime.handleInput({ kind: "key", key: "enter" });
    await runtime.handleInput({
      kind: "mouse",
      phase: "press",
      button: "left",
      x: row.rect.x1,
      y: row.rect.y1,
      modifiers: { shift: false, alt: false, ctrl: false },
    });
    await runtime.handleInput({
      kind: "mouse",
      phase: "release",
      button: "left",
      x: row.rect.x1,
      y: row.rect.y1,
      modifiers: { shift: false, alt: false, ctrl: false },
    });

    expect(activations).toEqual([
      expect.objectContaining({
        regionId: row.id,
        binding: row.binding,
        provenance: "keyboard",
      }),
      expect.objectContaining({
        regionId: row.id,
        binding: row.binding,
        provenance: "mouse",
      }),
    ]);
  });

  it("reflows on every resize without changing drafts, selection, scroll or pending commands", () => {
    const controller = stateBoundControlController();
    const activate = vi.fn(async () => {});
    const detach = vi.fn(async () => {});
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({
        draft: "keep this draft",
        scrollOffsetByView: { runs: 4 },
        detailScrollOffsetByView: { runs: 7 },
        focusId: "detail:runs:run:control",
      }),
      draw: () => {},
      detach,
      activate,
      eventId: () => "event-resize",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    const beforeUi = structuredClone(runtime.ui);
    const beforeController = structuredClone(controller.state);
    const modes = [
      runtime.resize({ columns: 140, rows: 36 }).mode,
      runtime.resize({ columns: 80, rows: 24 }).mode,
      runtime.resize({ columns: 60, rows: 18 }).mode,
      runtime.resize({ columns: 30, rows: 6 }).mode,
      runtime.resize({ columns: 5, rows: 2 }).mode,
      runtime.resize({ columns: 80, rows: 24 }).mode,
    ];

    expect(modes).toStrictEqual([
      "wide",
      "reference",
      "compact",
      "strip",
      "inert",
      "reference",
    ]);
    expect(runtime.ui).toStrictEqual(beforeUi);
    expectEnabledVisibleFocus(runtime);
    expect(controller.state).toStrictEqual(beforeController);
    expect(activate).not.toHaveBeenCalled();
    expect(detach).not.toHaveBeenCalled();
  });

  it("reclamps stored master and detail offsets after a projection shrinks", () => {
    const controller = new FakeController();
    const first = controller.dataset.pages.attention.rows[0];
    if (first === undefined) throw new Error("attention fixture unavailable");
    controller.dataset = {
      ...controller.dataset,
      pages: {
        ...controller.dataset.pages,
        attention: {
          ...controller.dataset.pages.attention,
          rows: [
            first,
            { ...first, stableId: "attention:2" },
            { ...first, stableId: "attention:3" },
          ],
        },
      },
    };
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({
        scrollOffsetByView: { attention: 2 },
        detailScrollOffsetByView: { attention: 50 },
      }),
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "projection-shrink",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    runtime.updateDataset(fixtureDataset(12));

    expect(runtime.ui.scrollOffsetByView.attention).toBe(0);
    const detail = runtime.frame.hitRegions.find(
      ({ id }) => id === "detail:attention:attention:1",
    );
    expect(runtime.ui.detailScrollOffsetByView.attention).toBe(
      detail?.scrollMaximum,
    );
    expect(runtime.ui.detailScrollOffsetByView.attention).toBeLessThan(50);
    expect(runtime.frame.rows.join("\n")).toContain("Resume blocked task");
  });

  it("restores a stable attention anchor at the same visual row after live inserts", () => {
    const controller = new FakeController();
    const template = controller.dataset.pages.attention.rows[0];
    if (template === undefined) throw new Error("attention fixture unavailable");
    const existing = Array.from({ length: 12 }, (_, index) => ({
      ...template,
      stableId: `attention:${String(index + 1)}`,
    }));
    controller.dataset = {
      ...controller.dataset,
      pages: {
        ...controller.dataset.pages,
        attention: { ...controller.dataset.pages.attention, rows: existing },
      },
    };
    controller.state = {
      ...controller.state,
      selectionByView: {
        ...controller.state.selectionByView,
        attention: { stableId: "attention:8", revision: template.revision },
      },
      scrollAnchorByView: {
        ...controller.state.scrollAnchorByView,
        attention: "attention:8",
      },
    };
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({
        focusId: "row:attention:attention:8",
        scrollOffsetByView: { attention: 5 },
      }),
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "stable-live-insert",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    runtime.repaint();
    const beforeY = runtime.frame.hitRegions.find(
      ({ id }) => id === "row:attention:attention:8",
    )?.rect.y1;
    expect(beforeY).toBeDefined();

    const inserted = [
      { ...template, stableId: "attention:new-1" },
      { ...template, stableId: "attention:new-2" },
      ...existing,
    ];
    runtime.updateDataset({
      ...controller.dataset,
      pages: {
        ...controller.dataset.pages,
        attention: { ...controller.dataset.pages.attention, rows: inserted },
      },
    });

    expect(runtime.ui.scrollOffsetByView.attention).toBe(7);
    expect(runtime.ui.focusId).toBe("row:attention:attention:8");
    expect(runtime.frame.hitRegions.find(
      ({ id }) => id === "row:attention:attention:8",
    )?.rect.y1).toBe(beforeY);
  });

  it.each(["editor", "guided", "palette", "filter"] as const)(
    "keeps q editable in normal %s mode but honors the advertised inert detach binding",
    async (inputMode) => {
      const detach = vi.fn(async () => {});
      const runtime = new FabricConsoleRuntime({
        controller: new FakeController(),
        viewport: { columns: 80, rows: 24 },
        ui: createFabricUiState({ inputMode, draft: "draft:", filterDraft: "filter:" }),
        draw: () => {},
        detach,
        activate: async () => {},
        eventId: () => `event-inert-q-${inputMode}`,
        render: renderFabricConsoleFrame,
        reducePointer: reduceFabricPointer,
      });

      await runtime.handleInput({ kind: "key", key: "text", text: "q" });

      expect(inputMode === "filter" ? runtime.ui.filterDraft : runtime.ui.draft)
        .toBe(inputMode === "filter" ? "filter:q" : "draft:q");
      expect(detach).not.toHaveBeenCalled();

      const inertFrame = runtime.resize({ columns: 8, rows: 1 });
      expect(inertFrame).toMatchObject({ mode: "inert" });
      expect(inertFrame.rows).toStrictEqual(["q detach"]);

      await runtime.handleInput({ kind: "key", key: "text", text: "q" });

      expect(detach).toHaveBeenCalledOnce();
      expect(detach).toHaveBeenCalledWith({ reason: "operator" });
      expect(inputMode === "filter" ? runtime.ui.filterDraft : runtime.ui.draft)
        .toBe(inputMode === "filter" ? "filter:q" : "draft:q");
    },
  );

  it.each(["editor", "guided", "palette", "filter"] as const)(
    "gives %s input visible ownership and restores its exact opener",
    (inputMode) => {
      const controller = stateBoundControlController();
      const opener = "action:resume";
      const runtime = new FabricConsoleRuntime({
        controller,
        viewport: { columns: 80, rows: 24 },
        ui: createFabricUiState({ focusId: opener }),
        draw: () => {},
        detach: async () => {},
        activate: async () => {},
        eventId: () => `input-focus-${inputMode}`,
        render: renderFabricConsoleFrame,
        reducePointer: reduceFabricPointer,
      });

      runtime.setInputMode(inputMode);
      expectEnabledVisibleFocus(runtime, `input:${inputMode}`);

      runtime.setInputMode("browse");
      expectEnabledVisibleFocus(runtime, opener);
    },
  );

  it.each(["editor", "guided", "palette", "filter"] as const)(
    "routes Ctrl-C through safety detach before %s mode dispatch after an inert resize",
    async (inputMode) => {
      const detach = vi.fn(async () => {});
      const runtime = new FabricConsoleRuntime({
        controller: new FakeController(),
        viewport: { columns: 80, rows: 24 },
        ui: createFabricUiState({ inputMode, draft: "preserved" }),
        draw: () => {},
        detach,
        activate: async () => {},
        eventId: () => `event-inert-ctrl-c-${inputMode}`,
        render: renderFabricConsoleFrame,
        reducePointer: reduceFabricPointer,
      });

      expect(runtime.resize({ columns: 1, rows: 1 }).mode).toBe("inert");
      await runtime.handleInput({ kind: "key", key: "ctrl-c" });

      expect(detach).toHaveBeenCalledOnce();
      expect(detach).toHaveBeenCalledWith({ reason: "safety" });
      expect(runtime.ui.draft).toBe("preserved");
    },
  );

  it("makes inert mouse detach geometry non-activating without Fabric or UI mutation", async () => {
    const controller = new FakeController();
    const beforeController = structuredClone(controller.state);
    const detach = vi.fn(async () => {});
    const activate = vi.fn(async () => {});
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 8, rows: 1 },
      ui: createFabricUiState({ mouseCapture: true }),
      draw: () => {},
      detach,
      activate,
      eventId: () => "event-inert-mouse-detach",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    const mouse = {
      kind: "mouse" as const,
      button: "left" as const,
      x: 1,
      y: 1,
      modifiers: { shift: false, alt: false, ctrl: false },
    };

    await runtime.handleInput({ ...mouse, phase: "press" });
    await runtime.handleInput({ ...mouse, phase: "release" });

    expect(detach).not.toHaveBeenCalled();
    expect(activate).not.toHaveBeenCalled();
    expect(controller.state).toStrictEqual(beforeController);
    expect(runtime.closed).toBe(false);
  });

  it("keeps every inert non-close input and public state setter mutation-free", async () => {
    const controller = new FakeController();
    const activate = vi.fn(async () => {});
    const detach = vi.fn(async () => {});
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({
        focusId: "input:editor",
        inputMode: "editor",
        draft: "preserve-draft",
        mouseCapture: true,
        scrollOffsetByView: { attention: 4 },
        detailScrollOffsetByView: { attention: 7 },
        reviewScrollOffset: 9,
      }),
      draw: () => {},
      detach,
      activate,
      eventId: () => "inert-no-mutation",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    const beforeUi = structuredClone(runtime.ui);
    const beforeController = structuredClone(controller.state);
    const beforeDataset = controller.dataset;

    expect(runtime.resize({ columns: 29, rows: 5 }).mode).toBe("inert");
    expect(runtime.ui).toStrictEqual(beforeUi);

    for (const event of [
      { kind: "key", key: "text", text: "x" },
      { kind: "key", key: "escape" },
      { kind: "key", key: "backspace" },
      { kind: "key", key: "enter" },
      { kind: "key", key: "alt-m" },
      { kind: "key", key: "alt-2" },
      { kind: "key", key: "page-down" },
      { kind: "key", key: "down" },
      { kind: "key", key: "tab" },
      { kind: "paste", text: "pasted" },
      { kind: "rejected", reason: "malformed-sequence" },
      {
        kind: "mouse",
        phase: "press",
        button: "left",
        x: 1,
        y: 1,
        modifiers: { shift: false, alt: false, ctrl: false },
      },
      {
        kind: "mouse",
        phase: "release",
        button: "left",
        x: 1,
        y: 1,
        modifiers: { shift: false, alt: false, ctrl: false },
      },
    ] as const) {
      await runtime.handleInput(event);
    }

    runtime.setInputMode("browse");
    runtime.setFocus("detach");
    runtime.setWorkflowReview(shortWorkflowReview("review"));
    runtime.updateDataset({ ...controller.dataset, loadedAtMs: controller.dataset.loadedAtMs + 1 });
    runtime.repaint();

    expect(runtime.ui).toStrictEqual(beforeUi);
    expect(controller.state).toStrictEqual(beforeController);
    expect(controller.dataset).toBe(beforeDataset);
    expect(activate).not.toHaveBeenCalled();
    expect(detach).not.toHaveBeenCalled();

    runtime.resize({ columns: 80, rows: 24 });
    expect(runtime.ui).toStrictEqual(beforeUi);
    expect(controller.state).toStrictEqual(beforeController);
  });

  it.each(["editor", "guided", "palette", "filter"] as const)(
    "allows only pointer Detach while the %s modal owns input",
    async (inputMode) => {
      const controller = new FakeController();
      const beforeController = structuredClone(controller.state);
      const detach = vi.fn(async () => {});
      const activate = vi.fn(async () => {});
      const runtime = new FabricConsoleRuntime({
        controller,
        viewport: { columns: 80, rows: 24 },
        ui: createFabricUiState({ inputMode, mouseCapture: true, draft: "q?" }),
        draw: () => {},
        detach,
        activate,
        eventId: () => `event-modal-mouse-${inputMode}`,
        render: renderFabricConsoleFrame,
        reducePointer: reduceFabricPointer,
      });
      const underlyingActionPoint = { x: 2, y: 22 };
      const modifiers = { shift: false, alt: false, ctrl: false };

      await runtime.handleInput({
        kind: "mouse",
        phase: "press",
        button: "left",
        ...underlyingActionPoint,
        modifiers,
      });
      await runtime.handleInput({
        kind: "mouse",
        phase: "release",
        button: "left",
        ...underlyingActionPoint,
        modifiers,
      });

      expect(activate).not.toHaveBeenCalled();
      expect(detach).not.toHaveBeenCalled();
      expect(controller.state).toStrictEqual(beforeController);
      expect(runtime.ui.draft).toBe("q?");

      const detachRegion = runtime.frame.hitRegions.find(({ id }) => id === "detach");
      expect(detachRegion).toBeDefined();
      if (detachRegion === undefined) return;
      const detachPoint = { x: detachRegion.rect.x1, y: detachRegion.rect.y1 };
      await runtime.handleInput({
        kind: "mouse",
        phase: "press",
        button: "left",
        ...detachPoint,
        modifiers,
      });
      await runtime.handleInput({
        kind: "mouse",
        phase: "release",
        button: "left",
        ...detachPoint,
        modifiers,
      });

      expect(detach).toHaveBeenCalledOnce();
      expect(detach).toHaveBeenCalledWith({ reason: "operator" });
      expect(activate).not.toHaveBeenCalled();
      expect(runtime.closed).toBe(true);
    },
  );

  it("uses local keyboard and mouse paths for split resizing without commands", async () => {
    const activate = vi.fn(async () => {});
    const runtime = new FabricConsoleRuntime({
      controller: stateBoundControlController(),
      viewport: { columns: 140, rows: 36 },
      ui: createFabricUiState({
        focusId: "splitter:master-detail",
        mouseCapture: true,
        splitterRatio: 0.45,
        draft: "split-safe",
      }),
      draw: () => {},
      detach: async () => {},
      activate,
      eventId: () => "event-split",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    await runtime.handleInput({ kind: "key", key: "right" });
    const keyboardRatio = runtime.ui.splitterRatio;
    const splitter = runtime.frame.hitRegions.find(
      ({ id }) => id === "splitter:master-detail",
    );
    expect(splitter).toBeDefined();
    if (splitter === undefined) return;
    await runtime.handleInput({
      kind: "mouse",
      phase: "press",
      button: "left",
      x: splitter.rect.x1,
      y: splitter.rect.y1,
      modifiers: { shift: false, alt: false, ctrl: false },
    });
    await runtime.handleInput({
      kind: "mouse",
      phase: "drag",
      button: "left",
      x: splitter.rect.x1 + 12,
      y: splitter.rect.y1,
      modifiers: { shift: false, alt: false, ctrl: false },
    });

    expect(keyboardRatio).toBeCloseTo(0.5);
    expect(runtime.ui.splitterRatio).toBeGreaterThan(keyboardRatio);
    expect(runtime.ui.draft).toBe("split-safe");
    expect(activate).not.toHaveBeenCalled();
  });

  it("migrates hidden splitter focus into compact mode and restores it only without interaction", async () => {
    const runtime = new FabricConsoleRuntime({
      controller: stateBoundControlController(),
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({
        focusId: "splitter:master-detail",
        splitterRatio: 0.45,
      }),
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "event-compact-splitter-focus",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    runtime.resize({ columns: 60, rows: 18 });
    expect(runtime.frame.mode).toBe("compact");
    expect(runtime.ui.focusId).not.toBe("splitter:master-detail");
    expect(runtime.frame.hitRegions.some(({ id }) => id === runtime.ui.focusId))
      .toBe(true);
    const ratio = runtime.ui.splitterRatio;
    await runtime.handleInput({ kind: "key", key: "up" });
    expect(runtime.ui.splitterRatio).toBe(ratio);
    runtime.resize({ columns: 80, rows: 24 });
    expect(runtime.ui.focusId).not.toBe("splitter:master-detail");

    runtime.setFocus("splitter:master-detail");
    runtime.resize({ columns: 60, rows: 18 });
    const migratedFocus = runtime.ui.focusId;
    runtime.resize({ columns: 80, rows: 24 });
    expect(migratedFocus).not.toBe("splitter:master-detail");
    expect(runtime.ui.focusId).toBe("splitter:master-detail");

    runtime.resize({ columns: 30, rows: 6 });
    expect(runtime.frame.mode).toBe("strip");
    expect(runtime.ui.focusId).not.toBe("splitter:master-detail");
    expect(runtime.frame.hitRegions.some(({ id }) => id === runtime.ui.focusId))
      .toBe(true);
    runtime.resize({ columns: 80, rows: 24 });
    expect(runtime.ui.focusId).toBe("splitter:master-detail");

    runtime.resize({ columns: 8, rows: 1 });
    expect(runtime.frame.mode).toBe("inert");
    expect(runtime.ui.focusId).toBe("splitter:master-detail");
    runtime.resize({ columns: 80, rows: 24 });
    expect(runtime.ui.focusId).toBe("splitter:master-detail");
  });

  it("restores an arbitrary semantic focus after a temporary compact resize", () => {
    const runtime = new FabricConsoleRuntime({
      controller: stateBoundControlController(),
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "semantic-resize-focus",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    const detailFocus = runtime.frame.hitRegions.find(
      ({ id, kind }) => kind === "pager" && id.startsWith("detail:"),
    )?.id;
    expect(detailFocus).toBeDefined();
    if (detailFocus === undefined) return;
    runtime.setFocus(detailFocus);

    runtime.resize({ columns: 60, rows: 18 });
    expect(runtime.ui.focusId).not.toBe(detailFocus);
    expectEnabledVisibleFocus(runtime);
    runtime.resize({ columns: 80, rows: 24 });

    expectEnabledVisibleFocus(runtime, detailFocus);
  });

  it("retains splitter restoration when projection refresh invalidates its surrogate", () => {
    const controller = stateBoundControlController();
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({ focusId: "splitter:master-detail" }),
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "event-splitter-projection-refresh",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    runtime.resize({ columns: 60, rows: 18 });
    const firstSurrogate = runtime.ui.focusId;
    expect(firstSurrogate).not.toBe("splitter:master-detail");

    runtime.updateDataset({
      ...controller.dataset,
      pages: {
        ...controller.dataset.pages,
        runs: {
          ...controller.dataset.pages.runs,
          rows: [],
        },
      },
    });
    expect(runtime.ui.focusId).not.toBe(firstSurrogate);
    expectEnabledVisibleFocus(runtime);

    runtime.resize({ columns: 80, rows: 24 });
    expectEnabledVisibleFocus(runtime, "splitter:master-detail");
  });

  it("keeps a splitter surrogate visible through chained compact, strip and inert resizes", () => {
    const runtime = new FabricConsoleRuntime({
      controller: stateBoundControlController(),
      viewport: { columns: 140, rows: 36 },
      ui: createFabricUiState({
        compactPane: "detail",
        focusId: "splitter:master-detail",
      }),
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "event-chained-resize-focus",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    for (const viewport of [
      { columns: 60, rows: 18, mode: "compact" },
      { columns: 30, rows: 6, mode: "strip" },
      { columns: 8, rows: 1, mode: "inert" },
    ] as const) {
      const focusBeforeResize = runtime.ui.focusId;
      runtime.resize(viewport);
      expect(runtime.frame.mode).toBe(viewport.mode);
      if (viewport.mode === "inert") {
        expect(runtime.ui.focusId).toBe(focusBeforeResize);
      } else {
        expect(runtime.ui.focusId).not.toBe("splitter:master-detail");
        expect(runtime.frame.hitRegions.some(
          ({ enabled, id }) => enabled && id === runtime.ui.focusId,
        )).toBe(true);
      }
    }

    runtime.resize({ columns: 140, rows: 36 });
    expect(runtime.ui.focusId).toBe("splitter:master-detail");
  });

  it("advances a long 30x6 review with overlapping strip pages", async () => {
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 30, rows: 6 },
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "event-strip-review-coverage",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    runtime.setWorkflowReview(longBoundReview("Git"));
    expect(runtime.frame.hitRegions.some(({ id }) => id === "review:continue"))
      .toBe(false);

    for (let page = 0; page < 300; page += 1) {
      if (runtime.frame.hitRegions.some(
        ({ enabled, id }) => enabled && id === "review:continue",
      )) break;
      await runtime.handleInput({ kind: "key", key: "page-down" });
    }

    expect(runtime.frame.hitRegions.find(({ id }) => id === "review:continue"))
      .toMatchObject({ enabled: true });
  });

  it("falls back from an enabled browse target with no visible focus marker", () => {
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "event-unmarked-focus-fallback",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    runtime.setWorkflowReview(longBoundReview("Git"));
    expect(runtime.frame.hitRegions).toContainEqual(
      expect.objectContaining({ enabled: true, id: "review:scroll" }),
    );

    runtime.setFocus("review:scroll");

    expect(runtime.ui.focusId).not.toBe("review:scroll");
    expectEnabledVisibleFocus(runtime);
  });

  it("makes every enabled 80x24 target keyboard reachable with visible focus", async () => {
    const activate = vi.fn(async () => {});
    const runtime = new FabricConsoleRuntime({
      controller: stateBoundControlController(),
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      detach: async () => {},
      activate,
      eventId: () => "event-focus-traversal",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    const expected = runtime.frame.hitRegions
      .filter(({ enabled }) => enabled)
      .map(({ id }) => id);
    expect(expected).toContain("splitter:master-detail");

    const visited = new Set<string>();
    for (let index = 0; index < expected.length; index += 1) {
      await runtime.handleInput({ kind: "key", key: "tab" });
      const focusId = runtime.ui.focusId;
      expect(focusId).not.toBeNull();
      if (focusId === null) continue;
      visited.add(focusId);

      const region = runtime.frame.hitRegions.find(
        ({ enabled, id }) => enabled && id === focusId,
      );
      expect(region).toBeDefined();
      if (region === undefined) continue;
      const focusedText = runtime.frame.rows
        .slice(region.rect.y1 - 1, region.rect.y2)
        .map((line) => line.slice(region.rect.x1 - 1, region.rect.x2))
        .join("\n");
      expect(focusedText, focusId).toContain(">");

      if (focusId === "splitter:master-detail") {
        const ratio = runtime.ui.splitterRatio;
        await runtime.handleInput({ kind: "key", key: "enter" });
        await runtime.handleInput({ kind: "key", key: "space" });
        expect(runtime.ui.splitterRatio).toBe(ratio);
        expect(activate).not.toHaveBeenCalled();
      }
    }

    expect(visited).toStrictEqual(new Set(expected));
  });

  it("keeps compact master and detail reachable without hiding state", async () => {
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 60, rows: 18 },
      ui: createFabricUiState({ compactPane: "master", draft: "compact-safe" }),
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "event-compact",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    await runtime.handleInput({ kind: "key", key: "right" });
    expect(runtime.ui.compactPane).toBe("detail");
    expect(runtime.frame.rows.join("\n")).toContain("ID: attention:1");
    await runtime.handleInput({ kind: "key", key: "left" });
    expect(runtime.ui.compactPane).toBe("master");
    expect(runtime.frame.rows.join("\n")).toContain("Resume blocked task");
    expect(runtime.ui.draft).toBe("compact-safe");
  });

  it("keeps a bounded independently scrollable message detail window", async () => {
    const controller = new FakeController();
    const messageRow: ConsoleRow<"activity"> = {
      view: "activity",
      stableId: "activity-group-message",
      revision: revisionFromProtocol(4),
      urgency: "normal",
      freshness: {
        state: "live",
        source: "fabric",
        revision: revisionFromProtocol(4),
        observedAt: timestamp,
        ageMs: 0,
      },
      summary: {
        kind: "activity",
        summary: "Long message",
        occurredAt: timestamp,
        group: {
          groupId: "activity-group-message",
          ordinal: 1,
          kind: "message",
          actorIds: ["agent-message"],
          target: { kind: "message", id: "message-1" },
          eventKinds: ["message-persisted"],
          occurredAtRange: { first: timestamp, last: timestamp },
          sourceRange: { first: 4, last: 4 },
          count: 1,
          evidenceLinkCount: 0,
          evidenceLinksDigest:
            "sha256:4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945" as never,
          evidenceLinksTruncated: false,
          evidenceLinks: [],
          members: [{
            ordinal: 1,
            eventId: "event-message",
            eventKind: "message-persisted",
            actorId: "agent-message",
            target: { kind: "message", id: "message-1" },
            occurredAt: timestamp,
            sourceRevision: 4,
            messageBodyRef: {
              projectSessionId: "session-1" as never,
              messageId: "message-1" as never,
              expectedRevision: 4,
            },
            detailAvailability: "available",
            evidenceLinkCount: 0,
            evidenceLinksDigest:
              "sha256:4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945" as never,
          }],
        },
      },
      detailRef: {
        kind: "activity",
        groupId: "activity-group-message",
        expectedRevision: 4,
      },
      actionAvailability: { state: "read-only", reason: "state-ineligible" },
    };
    controller.dataset = {
      ...controller.dataset,
      pages: {
        ...controller.dataset.pages,
        activity: {
          view: "activity",
          rows: [messageRow],
          nextCursor: 1,
          hasMore: false,
          snapshotRevision: revisionFromProtocol(11),
          readTransactionId: "activity-read",
        },
      },
      inspection: {
        kind: "message",
        state: "current",
        binding: {
          view: "activity",
          itemId: "activity-group-message",
          itemRevision: revisionFromProtocol(4),
          projectionRevision: revisionFromProtocol(11),
        },
        result: {
          available: true,
          messageId: "message-1" as never,
          revision: 4,
          body: Array.from({ length: 30 }, (_, index) => `line-${String(index).padStart(2, "0")}`).join("\n"),
          terminalNeutralised: true,
          capabilityValuesRedacted: true,
          artifactRefs: [],
        },
      },
    };
    controller.state = {
      ...controller.state,
      activeView: "activity",
      selectionByView: {
        ...controller.state.selectionByView,
        activity: { stableId: "activity-group-message", revision: revisionFromProtocol(4) },
      },
    };
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 60, rows: 18 },
      ui: createFabricUiState({ compactPane: "detail", mouseCapture: true }),
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "event-scroll",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    expect(runtime.frame.rows.join("\n")).toContain("line-00");
    expect(runtime.frame.rows.join("\n")).not.toContain("line-13");
    await runtime.handleInput({ kind: "key", key: "page-down" });

    expect(runtime.ui.detailScrollOffsetByView.activity).toBe(5);
    expect(runtime.frame.rows.join("\n")).toContain("line-13");
    expect(runtime.ui.scrollOffsetByView.activity).toBeUndefined();

    const detail = runtime.frame.hitRegions.find(
      ({ id }) => id === "detail:activity:activity-group-message",
    );
    expect(detail).toBeDefined();
    if (detail === undefined) return;
    await runtime.handleInput({
      kind: "mouse",
      phase: "wheel",
      button: "wheel-down",
      x: detail.rect.x1,
      y: detail.rect.y1,
      modifiers: { shift: false, alt: false, ctrl: false },
    });

    expect(runtime.ui.detailScrollOffsetByView.activity).toBe(10);
    expect(runtime.frame.rows.join("\n")).toContain("line-18");
    expect(runtime.ui.scrollOffsetByView.activity).toBeUndefined();

    for (let index = 0; index < 10; index += 1) {
      await runtime.handleInput({ kind: "key", key: "page-down" });
    }
    expect(runtime.ui.detailScrollOffsetByView.activity).toBe(29);
    expect(runtime.frame.rows.join("\n")).toContain("line-29");
  });

  it("renders bounded artifact content as inert text in the Evidence detail pane", async () => {
    const rendered = "# Actual reviewed spec\n\u001b[31mred must stay inert\nafop_hidden-token";
    const renderedDigest = `sha256:${createHash("sha256").update(rendered).digest("hex")}` as Sha256Digest;
    const controller = new FakeController();
    const row: ConsoleRow<"evidence"> = {
      view: "evidence",
      stableId: "artifact-spec",
      revision: revisionFromProtocol(7),
      urgency: "normal",
      freshness: {
        state: "live",
        source: "fabric",
        revision: revisionFromProtocol(7),
        observedAt: timestamp,
        ageMs: 0,
      },
      summary: {
        kind: "evidence",
        evidenceKind: "artifact",
        status: "informational",
        provenance: "fabric:chair",
      },
      detailRef: {
        kind: "evidence",
        evidenceId: "artifact-spec" as never,
        expectedRevision: 7,
      },
      actionAvailability: {
        state: "available",
        actions: ["promotion"],
        requiresPreview: true,
      },
    };
    controller.dataset = {
      ...controller.dataset,
      pages: {
        ...controller.dataset.pages,
        evidence: {
          view: "evidence",
          rows: [row],
          nextCursor: 1,
          hasMore: false,
          snapshotRevision: revisionFromProtocol(11),
          readTransactionId: "evidence-read",
        },
      },
      inspection: {
        kind: "artifact",
        state: "current",
        binding: {
          view: "evidence",
          itemId: "artifact-spec",
          itemRevision: revisionFromProtocol(7),
          projectionRevision: revisionFromProtocol(11),
        },
        readTransactionId: "artifact-read",
        result: {
          artifactRef: { path: "docs/spec.md" as never, digest },
          evidenceRevision: 7,
          evidenceKind: "artifact",
          sourceKind: "project-file",
          publisherKind: "agent",
          publisherRef: "chair-1",
          projectSessionId: null,
          coordinationRunId: null,
          taskId: null,
          createdAt: timestamp,
          mediaType: "text/markdown",
          content: rendered,
          totalBytes: 100,
          totalLines: 3,
          renderedTotalBytes: Buffer.byteLength(rendered),
          renderedTotalLines: 3,
          renderedArtifactDigest: renderedDigest,
          transformation: "combined",
          terminalNeutralised: true,
          capabilityValuesRedacted: true,
          credentialValuesRedacted: true,
          pages: [{
            pageIndex: 0,
            lineFragment: "whole",
            pageContentDigest: renderedDigest,
            bytes: Buffer.byteLength(rendered),
          }],
          coverage: {
            complete: true,
            verified: true,
            pageCount: 1,
          },
          reviewDisposition: "blocked-redacted",
        },
      },
    };
    controller.state = {
      ...controller.state,
      activeView: "evidence",
      selectionByView: {
        ...controller.state.selectionByView,
        evidence: { stableId: "artifact-spec", revision: revisionFromProtocol(7) },
      },
    };

    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({
        compactPane: "detail",
        focusId: "detail:evidence:artifact-spec",
      }),
      draw: () => {},
      detach: async () => {},
      activate: async () => {},
      eventId: () => "event-artifact",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });
    const frame = runtime.frame.rows.join("\n");

    expect(frame).toContain("Evidence: artifact r7");
    expect(frame).toContain("Publisher: agent:chair-1");
    expect(frame).toContain("Coverage: 1/1 VERIFIED");
    expect(runtime.frame.hitRegions.find(({ id }) => id === "action:promotion")?.enabled).toBe(false);
    await runtime.handleInput({ kind: "key", key: "page-down" });
    expect(runtime.frame.rows.join("\n")).toContain("Review: BLOCKED | hidden source bytes");
    await runtime.handleInput({ kind: "key", key: "page-down" });
    const contentFrame = runtime.frame.rows.join("\n");
    expect(contentFrame).toContain("# Actual reviewed spec");
    expect(contentFrame).toContain("<ESC>[31mred must stay inert");
    expect(contentFrame).toContain("[REDACTED capability]");
    expect(contentFrame).not.toContain("afop_hidden-token");
    expect(contentFrame).not.toContain("\u001b");

    runtime.resize({ columns: 44, rows: 15 });
    expect(runtime.frame.mode).toBe("compact");
    expect(controller.dataset.inspection).toBeDefined();
    runtime.resize({ columns: 120, rows: 32 });
    expect(runtime.frame.mode).toBe("wide");
    await runtime.handleInput({ kind: "key", key: "page-up" });
    await runtime.handleInput({ kind: "key", key: "page-up" });
    expect(runtime.frame.rows.join("\n")).toContain("Coverage: 1/1 VERIFIED");
    expect(controller.state.selectionByView.evidence?.stableId).toBe("artifact-spec");
  });

  it("detaches exactly once, never stops work, and ignores late input", async () => {
    const detach = vi.fn(async () => {});
    const activate = vi.fn(async () => {});
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      detach,
      activate,
      eventId: () => "event-late",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    await Promise.all([runtime.close("operator"), runtime.close("operator")]);
    await runtime.handleInput({ kind: "key", key: "text", text: "q" });
    await runtime.handleInput({ kind: "key", key: "enter" });

    expect(detach).toHaveBeenCalledTimes(1);
    expect(detach).toHaveBeenCalledWith({ reason: "operator" });
    expect(activate).not.toHaveBeenCalled();
    expect(runtime.closed).toBe(true);
  });

  it("shows only a bounded failure code when an action callback rejects", async () => {
    const controller = stateBoundControlController();
    const runtime = new FabricConsoleRuntime({
      controller,
      viewport: { columns: 80, rows: 24 },
      ui: createFabricUiState({ focusId: "action:resume" }),
      draw: () => {},
      detach: async () => {},
      activate: async () =>
        Promise.reject(
          Object.assign(new Error("token-never-render"), {
            code: "WRONG_PROJECT",
          }),
        ),
      eventId: () => "event-failure",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    await runtime.handleInput({ kind: "key", key: "enter" });

    expect(runtime.ui.notice).toBe("Action failed: WRONG_PROJECT");
    expect(runtime.frame.rows.join("\n")).toContain(
      "Action failed: WRONG_PROJECT",
    );
    expect(runtime.frame.rows.join("\n")).not.toContain("token-never-render");
  });

  it("fatal decoder quarantine takes the same idempotent safety detach path", async () => {
    const detach = vi.fn(async () => {});
    const runtime = new FabricConsoleRuntime({
      controller: new FakeController(),
      viewport: { columns: 80, rows: 24 },
      draw: () => {},
      detach,
      activate: async () => {},
      eventId: () => "event-fatal",
      render: renderFabricConsoleFrame,
      reducePointer: reduceFabricPointer,
    });

    await runtime.handleInput({ kind: "fatal", reason: "input-quarantine-lost" });
    await runtime.handleInput({ kind: "fatal", reason: "input-quarantine-lost" });

    expect(detach).toHaveBeenCalledTimes(1);
    expect(detach).toHaveBeenCalledWith({ reason: "safety" });
  });
});
