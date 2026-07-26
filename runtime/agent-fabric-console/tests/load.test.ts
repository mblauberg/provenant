import { performance } from "node:perf_hooks";
import { hostname, platform, release } from "node:os";

import { describe, expect, it, vi } from "vitest";

import type {
  OperatorCapabilityCredential,
  OperatorProjectionSnapshot,
  OperatorViewPageResult,
  ProjectId,
  Sha256Digest,
  Timestamp,
} from "@local/agent-fabric-protocol";
import type { ConsoleControllerState } from "../src/controller.js";
import {
  MAX_FRAME_CELLS,
  cellWidth,
  renderFabricConsoleFrame,
} from "../src/index.js";
import {
  ConsoleProtocolAdapter,
  type ConsoleProtocolPort,
} from "../src/protocol-adapter.js";
import {
  FABRIC_VIEWS,
  createEmptyViewPages,
  revisionFromProtocol,
  type ConsoleRow,
  type FabricView,
} from "../src/model.js";
import { createFabricUiState } from "../src/presenter.js";
import { TerminalInputDecoder } from "../src/input.js";
import { startConsoleRefreshLoop } from "../src/cli.js";

const timestamp = "2026-07-11T12:00:00.000Z" as Timestamp;
const digest = (`sha256:${"e".repeat(64)}`) as Sha256Digest;
const nativeNotification = {
  kind: "daemon-journal",
  targetIntegration: "native-desktop",
  status: "available",
  journalState: "sent",
  deliveryItemRevision: 1,
  claimGeneration: null,
  integrationState: "available",
  observedAt: timestamp,
} as const;

function largeFixture(count: number) {
  const rows: ConsoleRow<"attention">[] = Array.from(
    { length: count },
    (_, index) => ({
      view: "attention",
      stableId: `attention:${String(index).padStart(6, "0")}`,
      revision: revisionFromProtocol(index + 1),
      urgency: index % 17 === 0 ? "critical-path" : "advisory",
      freshness: {
        state: "live",
        source: "fabric",
        revision: revisionFromProtocol(index + 1),
        observedAt: timestamp,
        ageMs: index * 10,
      },
      summary: {
        kind: "attention",
        label: index % 17 === 0 ? "Blocked" : "FYI",
        priority: index % 17 === 0 ? "critical-path" : "advisory",
        title: `Bounded load item ${String(index)}`,
        nativeNotification: {
          ...nativeNotification,
          deliveryItemRevision: index + 1,
        },
      },
      detailRef: {
        kind: "system",
        componentId: `component-${String(index)}`,
        expectedRevision: index + 1,
      },
      actionAvailability: {
        state: "read-only",
        reason: "state-ineligible",
      },
    }),
  );
  const empty = createEmptyViewPages();
  const dataset = {
    connection: {
      state: "live" as const,
      compatibility: { mode: "current" as const },
    },
    snapshot: {
      schemaVersion: 1 as const,
      snapshotRevision: 11,
      readTransactionId: "load",
      project: {
        freshness: "live" as const,
        source: "fabric" as const,
        revision: 11,
        observedAt: timestamp,
        value: {
          projectId: "project-load" as ProjectId,
          canonicalRoot: "/load",
        },
      },
      session: {
        freshness: "unavailable" as const,
        source: "fabric" as const,
        revision: 11,
        observedAt: timestamp,
        reason: "not selected",
      },
      runs: {
        freshness: "live" as const,
        source: "fabric" as const,
        revision: 11,
        observedAt: timestamp,
        value: [],
      },
      attention: {
        freshness: "live" as const,
        source: "fabric" as const,
        revision: 11,
        observedAt: timestamp,
        value: [],
      },
      capacity: {
        freshness: "unavailable" as const,
        source: "fabric" as const,
        revision: 11,
        observedAt: timestamp,
        reason: "unknown",
      },
      cursor: 11,
      stateDigest: digest,
    },
    snapshotRevision: revisionFromProtocol(11),
    cursor: 11,
    pages: {
      ...empty,
      attention: {
        view: "attention" as const,
        rows,
        nextCursor: count,
        hasMore: false,
        snapshotRevision: revisionFromProtocol(11),
        readTransactionId: "load-attention",
      },
    },
    loadedAtMs: 0,
    canMutate: false,
  };
  const selections = Object.fromEntries(
    FABRIC_VIEWS.map((view) => [view, null]),
  ) as Record<FabricView, null | { stableId: string; revision: ReturnType<typeof revisionFromProtocol> }>;
  if (rows[0] !== undefined) {
    selections.attention = {
      stableId: rows[0].stableId,
      revision: rows[0].revision,
    };
  }
  const controller: ConsoleControllerState = {
    activeView: "attention",
    selectionByView: selections,
    scrollAnchorByView: Object.fromEntries(
      FABRIC_VIEWS.map((view) => [view, null]),
    ) as never,
    review: null,
    pendingCommandIds: [],
    lastActionStatus: null,
    lastReceipt: null,
  };
  return { dataset, controller, rows };
}

function countRowProjections(row: ConsoleRow<"attention">): () => number {
  const summary = row.summary;
  let count = 0;
  Object.defineProperty(row, "summary", {
    get: () => {
      count += 1;
      return summary;
    },
  });
  return () => count;
}

describe("Console bounded load gates", () => {
  it("renders 10,000 projected rows through repeated dynamic resize within the cell bound", () => {
    const { dataset, controller, rows } = largeFixture(10_000);
    const projectionSentinel = rows[5_000];
    expect(projectionSentinel).toBeDefined();
    if (projectionSentinel === undefined) return;
    const sentinelProjectionCount = countRowProjections(projectionSentinel);
    const ui = createFabricUiState({
      focusId: "row:attention:attention:000000",
      scrollOffsetByView: { attention: 9_950 },
      draft: "load-preserved",
    });
    const before = structuredClone(ui);
    const started = performance.now();
    const sizes = [
      [80, 24],
      [160, 50],
      [60, 18],
      [30, 6],
      [5, 2],
    ] as const;
    let renderedCells = 0;
    for (let index = 0; index < 500; index += 1) {
      const [columns, rows] = sizes[index % sizes.length] ?? [80, 24];
      const frame = renderFabricConsoleFrame(dataset, controller, ui, {
        columns,
        rows,
      });
      renderedCells += frame.columns * frame.rows.length;
      expect(frame.columns * frame.rows.length).toBeLessThanOrEqual(
        MAX_FRAME_CELLS,
      );
      expect(frame.rows.every((row) => cellWidth(row) === frame.columns)).toBe(
        true,
      );
    }
    const durationMs = performance.now() - started;

    expect(renderedCells).toBeGreaterThan(0);
    expect(sentinelProjectionCount()).toBe(1);
    expect(durationMs).toBeLessThan(5_000);
    expect(ui).toStrictEqual(before);
  }, 10_000);

  it("projects each independent Console runtime once across alternating resizes", () => {
    const first = largeFixture(100);
    const second = largeFixture(100);
    const firstSentinel = first.rows[50];
    const secondSentinel = second.rows[50];
    expect(firstSentinel).toBeDefined();
    expect(secondSentinel).toBeDefined();
    if (firstSentinel === undefined || secondSentinel === undefined) return;
    const firstProjectionCount = countRowProjections(firstSentinel);
    const secondProjectionCount = countRowProjections(secondSentinel);
    const firstUi = createFabricUiState();
    const secondUi = createFabricUiState();

    for (let index = 0; index < 20; index += 1) {
      renderFabricConsoleFrame(
        first.dataset,
        first.controller,
        firstUi,
        { columns: 80, rows: 24 },
      );
      renderFabricConsoleFrame(
        second.dataset,
        second.controller,
        secondUi,
        { columns: 160, rows: 50 },
      );
    }

    expect(firstProjectionCount()).toBe(1);
    expect(secondProjectionCount()).toBe(1);
  });

  it("projects invariant rows once across repeated selection changes", () => {
    const { dataset, controller: initialController, rows } = largeFixture(10_000);
    const projectionSentinel = rows[5_000];
    expect(projectionSentinel).toBeDefined();
    if (projectionSentinel === undefined) return;
    const sentinelProjectionCount = countRowProjections(projectionSentinel);
    const ui = createFabricUiState({
      focusId: "row:attention:attention:000000",
      scrollOffsetByView: { attention: 0 },
      draft: "selection-preserved",
    });
    const before = structuredClone(ui);
    let controller = initialController;
    let frame = renderFabricConsoleFrame(
      dataset,
      controller,
      ui,
      { columns: 80, rows: 24 },
    );
    expect(frame.presentation.masterRows[0]?.selected).toBe(true);
    expect(frame.presentation.topAttention?.selected).toBe(true);

    for (let index = 1; index <= 50; index += 1) {
      const selected = rows[index];
      expect(selected).toBeDefined();
      if (selected === undefined) return;
      controller = {
        ...controller,
        selectionByView: {
          ...controller.selectionByView,
          attention: {
            stableId: selected.stableId,
            revision: selected.revision,
          },
        },
      };
      frame = renderFabricConsoleFrame(
        dataset,
        controller,
        ui,
        { columns: 80, rows: 24 },
      );

      expect(frame.presentation.masterRows[index]).toMatchObject({
        stableId: selected.stableId,
        selected: true,
      });
      expect(frame.presentation.masterRows[index - 1]?.selected).toBe(false);
      expect(frame.presentation.detail?.stableId).toBe(selected.stableId);
      expect(frame.presentation.topAttention?.selected).toBe(false);
    }

    expect(frame.presentation.masterRows.filter(({ selected }) => selected))
      .toHaveLength(1);
    expect(sentinelProjectionCount()).toBe(1);
    expect(frame.columns * frame.rows.length).toBeLessThanOrEqual(
      MAX_FRAME_CELLS,
    );
    expect(frame.rows.every((row) => cellWidth(row) === frame.columns)).toBe(
      true,
    );
    expect(ui).toStrictEqual(before);
  });

  it("rejects frames beyond the shared cell budget without allocation", () => {
    const { dataset, controller } = largeFixture(1);
    const frame = renderFabricConsoleFrame(
      dataset,
      controller,
      createFabricUiState(),
      { columns: MAX_FRAME_CELLS, rows: MAX_FRAME_CELLS },
    );
    expect(frame).toMatchObject({ columns: 0, rows: [], mode: "inert" });
  });

  it("bounds hostile decoder traffic and never grows pending state", () => {
    const decoder = new TerminalInputDecoder({
      maxPendingBytes: 32,
      maxPasteBytes: 64,
      maxChunkBytes: 128,
    });
    let rejected = 0;
    for (let index = 0; index < 10_000; index += 1) {
      const events = decoder.push(Buffer.from("\u001b[<999;99999;99999M"));
      rejected += events.filter((event) => event.kind === "rejected").length;
    }
    expect(rejected).toBe(10_000);
    expect(decoder.end()).toStrictEqual([]);
  });

  it("fails closed when a protocol page cursor is endless", async () => {
    const credential = {
      capabilityId: "capability-load",
      token: "token",
    } as OperatorCapabilityCredential;
    const projectId = "project-load" as ProjectId;
    const snapshot: OperatorProjectionSnapshot = largeFixture(0).dataset
      .snapshot as OperatorProjectionSnapshot;
    const viewPage = vi.fn(async (request) => ({
      status: "page" as const,
      view: request.view,
      rows: [],
      nextCursor: request.cursor + 1,
      hasMore: true,
      snapshotRevision: request.snapshotRevision,
      readTransactionId: "endless",
    }) as OperatorViewPageResult);
    const port: ConsoleProtocolPort = {
      snapshot: async () => snapshot,
      events: async () => ({
        status: "continuation",
        events: [],
        nextCursor: 11,
        hasMore: false,
        snapshotRevision: 11,
        readTransactionId: "events",
      }),
      viewPage,
      runPage: async () => { throw new Error("unused run page"); },
      readDetail: async () => ({
        status: "resnapshot-required",
        reason: "snapshot-mismatch",
        currentSnapshotRevision: 11,
      }),
      readGate: async () => {
        throw new Error("unused");
      },
      readMessageBody: null,
      readRepository: null,
      readArtifactContent: null,
    };
    const adapter = new ConsoleProtocolAdapter({
      binding: {
        ok: true,
        port,
        readOnly: true,
        actions: null,
        nativeNotificationProjection: "daemon-journal",
        runSessionProjection: "exact",
        compatibility: { mode: "current" },
      },
      credential,
      projectId,
      maxPagesPerView: 3,
      maxResnapshotAttempts: 1,
    });

    const result = await adapter.open();

    expect(result).toMatchObject({
      connection: { state: "unavailable", reason: "projection-invalid" },
      canMutate: false,
    });
    expect(viewPage).toHaveBeenCalledTimes(3);
  });

  it("bounds the exact eventless notification churn workload", async () => {
    const rowCount = 1_000;
    const transactionCount = 200;
    const transitionsPerTransaction = 10;
    const pollTicks = 20;
    const pollIntervalMs = 500;
    const credential = {
      capabilityId: "capability-churn",
      token: "token-churn-never-render",
    } as OperatorCapabilityCredential;
    const projectId = "project-churn" as ProjectId;
    const journalStates = Array.from({ length: rowCount }, () => "pending" as "pending" | "sent");
    let revision = 1;
    let transactionIndex = 0;
    let transitionIndex = 0;
    let snapshotCalls = 0;
    let activeRefreshes = 0;
    let maximumConcurrentRefreshes = 0;
    let completedRefreshes = 0;
    const refreshLatencies: number[] = [];
    const baseSnapshot = largeFixture(0).dataset.snapshot as OperatorProjectionSnapshot;
    const port: ConsoleProtocolPort = {
      snapshot: async () => {
        snapshotCalls += 1;
        return { ...baseSnapshot, snapshotRevision: revision, cursor: 0 };
      },
      events: async () => ({
        status: "continuation",
        events: [],
        nextCursor: 0,
        hasMore: false,
        snapshotRevision: revision,
        readTransactionId: `churn-events-${String(revision)}`,
      }),
      viewPage: async (request) => {
        if (request.view !== "attention") {
          return {
            status: "page",
            view: request.view,
            rows: [],
            nextCursor: 0,
            hasMore: false,
            snapshotRevision: request.snapshotRevision,
            readTransactionId: `churn-${request.view}-${String(revision)}`,
          } as OperatorViewPageResult;
        }
        const end = Math.min(request.cursor + request.limit, rowCount);
        const rows = Array.from({ length: end - request.cursor }, (_, offset) => {
          const index = request.cursor + offset;
          const itemRevision = index + 1;
          return {
            itemId: `attention-churn-${String(index).padStart(4, "0")}`,
            itemRevision,
            fact: {
              freshness: "live" as const,
              source: "fabric" as const,
              revision: itemRevision,
              observedAt: timestamp,
              value: {
                summary: {
                  kind: "attention" as const,
                  label: "FYI" as const,
                  priority: "advisory" as const,
                  title: `Churn row ${String(index)}`,
                  nativeNotification: {
                    targetIntegration: "native-desktop" as const,
                    status: "available" as const,
                    journalState: journalStates[index] ?? "pending",
                    deliveryItemRevision: itemRevision,
                    claimGeneration: 1,
                    integrationState: "available" as const,
                    observedAt: timestamp,
                  },
                },
                detailRef: {
                  kind: "system" as const,
                  componentId: `native-${String(index)}`,
                  expectedRevision: itemRevision,
                },
                actionAvailability: {
                  state: "read-only" as const,
                  reason: "state-ineligible" as const,
                },
              },
            },
          };
        });
        return {
          status: "page",
          view: "attention",
          rows,
          nextCursor: end,
          hasMore: end < rowCount,
          snapshotRevision: request.snapshotRevision,
          readTransactionId: `churn-attention-${String(revision)}`,
        };
      },
      runPage: async () => { throw new Error("unused run page"); },
      readDetail: async () => ({
        status: "resnapshot-required",
        reason: "snapshot-mismatch",
        currentSnapshotRevision: revision,
      }),
      readGate: async () => { throw new Error("unused"); },
      readMessageBody: null,
      readRepository: null,
      readArtifactContent: null,
    };
    const adapter = new ConsoleProtocolAdapter({
      binding: {
        ok: true,
        port,
        readOnly: true,
        actions: null,
        nativeNotificationProjection: "daemon-journal",
        runSessionProjection: "exact",
        compatibility: { mode: "current" },
      },
      credential,
      projectId,
      pageLimit: 100,
    });
    await adapter.open();
    const warmSnapshotCalls = snapshotCalls;
    let scheduledTick: (() => void) | undefined;
    const loop = startConsoleRefreshLoop({
      intervalMs: pollIntervalMs,
      isClosed: () => false,
      onClosed: () => {},
      schedule: (callback) => {
        scheduledTick = callback;
        return "deterministic-churn-loop";
      },
      clear: () => {},
      refresh: async () => {
        activeRefreshes += 1;
        maximumConcurrentRefreshes = Math.max(maximumConcurrentRefreshes, activeRefreshes);
        const started = performance.now();
        try {
          await adapter.poll();
        } finally {
          refreshLatencies.push(performance.now() - started);
          activeRefreshes -= 1;
          completedRefreshes += 1;
        }
      },
    });
    if (scheduledTick === undefined) throw new Error("deterministic refresh callback was not installed");
    const heapBefore = process.memoryUsage().heapUsed;
    const cpuBefore = process.cpuUsage();
    const wallStarted = performance.now();

    for (let tick = 0; tick < pollTicks; tick += 1) {
      for (let batch = 0; batch < transactionCount / pollTicks; batch += 1) {
        for (let transition = 0; transition < transitionsPerTransaction; transition += 1) {
          const index = transitionIndex % rowCount;
          journalStates[index] = journalStates[index] === "pending" ? "sent" : "pending";
          transitionIndex += 1;
          revision += 1;
        }
        transactionIndex += 1;
      }
      const expectedRefreshes = completedRefreshes + 1;
      scheduledTick();
      while (completedRefreshes < expectedRefreshes) {
        await new Promise<void>((resolve) => setImmediate(resolve));
      }
    }
    await loop.stop();

    const wallMs = performance.now() - wallStarted;
    const cpu = process.cpuUsage(cpuBefore);
    const cpuMs = (cpu.user + cpu.system) / 1_000;
    const heapDelta = Math.max(0, process.memoryUsage().heapUsed - heapBefore);
    const sortedLatencies = [...refreshLatencies].sort((left, right) => left - right);
    const p95Index = Math.max(0, Math.ceil(sortedLatencies.length * 0.95) - 1);
    const p95Ms = sortedLatencies[p95Index] ?? Number.POSITIVE_INFINITY;
    const resnapshots = snapshotCalls - warmSnapshotCalls;
    const record = {
      gate: "spec01-32.15-notification-churn",
      host: { hostname: hostname(), platform: platform(), release: release() },
      node: process.version,
      workload: {
        consoles: 1,
        openAttentionRows: rowCount,
        transitions: transitionIndex,
        transactions: transactionIndex,
        simulatedMs: pollTicks * pollIntervalMs,
        pollTicks,
      },
      result: { resnapshots, maximumConcurrentRefreshes, p95Ms, wallMs, cpuMs, heapDelta },
    };
    console.info(JSON.stringify(record));

    expect(transactionIndex).toBe(transactionCount);
    expect(transitionIndex).toBe(transactionCount * transitionsPerTransaction);
    expect(completedRefreshes).toBe(pollTicks);
    expect(resnapshots).toBeLessThanOrEqual(pollTicks);
    expect(maximumConcurrentRefreshes).toBe(1);
    expect(p95Ms).toBeLessThanOrEqual(250);
    expect(wallMs).toBeLessThanOrEqual(5_000);
    expect(cpuMs).toBeLessThanOrEqual(5_000);
    expect(heapDelta).toBeLessThanOrEqual(32 * 1024 * 1024);
    expect(record.host.hostname.length).toBeGreaterThan(0);
    expect(record.node).toMatch(/^v/u);
  }, 10_000);
});
