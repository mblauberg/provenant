import { describe, expect, it } from "vitest";

import { renderFabricDetail } from "../src/renderer-detail.js";
import { renderFabricTabs } from "../src/renderer-main.js";
import { setFabricRow, type FabricHitRegion } from "../src/renderer-primitives.js";
import { wrapFabricReviewContent } from "../src/renderer-review.js";
import {
  createFabricUiState,
  type FabricConsolePresentation,
} from "../src/presenter.js";
import type { FabricConsoleDataset } from "../src/protocol-adapter.js";

describe("renderer component boundaries", () => {
  it("writes fixed-width rows through the renderer primitive seam", () => {
    const rows = ["unchanged", "unchanged"];

    setFabricRow(rows, 1, 6, "status");
    setFabricRow(rows, 3, 6, "outside");

    expect(rows).toStrictEqual(["status", "unchanged"]);
  });

  it("renders tab labels and hit geometry through the main surface seam", () => {
    const rows = [" ".repeat(24)];
    const hitRegions: FabricHitRegion[] = [];
    const presentation = {
      views: [
        { view: "attention", label: "Attention", active: true, key: "1" },
        { view: "project", label: "Project", active: false, key: "2" },
      ],
      focusId: "view:attention",
    } as unknown as FabricConsolePresentation;

    renderFabricTabs(rows, 24, presentation, "geometry", hitRegions, 1);

    expect(rows[0]).toBe(">1:Attn* 2:Proj         ");
    expect(hitRegions).toStrictEqual([
      {
        id: "view:attention",
        kind: "tab",
        rect: { x1: 1, y1: 1, x2: 8, y2: 1 },
        enabled: true,
        geometryKey: "geometry",
        binding: null,
      },
      {
        id: "view:project",
        kind: "tab",
        rect: { x1: 10, y1: 1, x2: 15, y2: 1 },
        enabled: true,
        geometryKey: "geometry",
        binding: null,
      },
    ]);
  });

  it("renders fallback detail lines through the detail surface seam", () => {
    const rows = [" ".repeat(20), " ".repeat(20)];
    const presentation = {
      activeView: "project",
      masterRows: [],
      detail: {
        stableId: "project-1",
        revision: 1,
        lines: [{ label: "Owner", value: "renderer" }],
      },
      focusId: null,
    } as unknown as FabricConsolePresentation;
    const dataset = { inspection: null } as unknown as FabricConsoleDataset;
    const hitRegions: FabricHitRegion[] = [];

    renderFabricDetail(
      rows,
      20,
      presentation,
      dataset,
      createFabricUiState(),
      "geometry",
      hitRegions,
      { x1: 1, y1: 1, x2: 20, y2: 2 },
    );

    expect(rows[0]).toBe(">Owner: renderer    ");
    expect(hitRegions).toStrictEqual([]);
  });

  it("renders the exact labelled run composition in compact and wide detail panes", () => {
    const presentation = {
      activeView: "runs",
      masterRows: [],
      detail: {
        stableId: "run-1",
        revision: "1",
        lines: [],
      },
      focusId: null,
    } as unknown as FabricConsolePresentation;
    const target = {
      kind: "coordination-run",
      coordinationRunId: "run-1",
    };
    const dataset = {
      inspection: {
        kind: "run",
        state: "current",
        binding: {
          view: "runs",
          itemId: "run-1",
          itemRevision: "1",
          projectionRevision: "1",
          projectSessionId: "session-1",
          runTarget: target,
        },
        result: {
          projectSessionId: "session-1",
          target,
          readTransactionId: "run-read-1",
          composition: {
            projectSessionId: "session-1",
            target,
            identity: {
              freshness: "live",
              source: "fabric",
              revision: 1,
              observedAt: "2026-07-26T00:00:00.000Z",
              value: {
                acceptedScope: { observation: "Unobserved" },
                currentPlan: { observation: "Unobserved" },
                lead: { observation: "Observed", value: "chair-1" },
                phase: { observation: "Observed", value: "active" },
                health: { observation: "Observed", value: "healthy" },
                currentMilestone: { observation: "Unobserved" },
                nextMilestone: { observation: "Observed", value: "quiescing" },
                lastEventAt: { observation: "Unobserved" },
              },
            },
            declaredProgress: {
              freshness: "live",
              source: "fabric",
              revision: 1,
              observedAt: "2026-07-26T00:00:00.000Z",
              value: {
                observation: "Observed",
                value: {
                  plan: "open",
                  counts: {
                    blocked: 1,
                    ready: 2,
                    active: 3,
                    complete: 4,
                    cancelled: 0,
                    degraded: 0,
                  },
                },
              },
            },
          },
          work: [],
          agents: [],
          evidence: [],
          activity: [{
            itemId: "activity-group-1",
            itemRevision: 5,
            fact: {
              freshness: "live",
              source: "fabric",
              revision: 5,
              observedAt: "2026-07-26T00:00:00.000Z",
              value: {
                summary: {
                  kind: "activity",
                  summary: "task activity",
                  occurredAt: "2026-07-26T00:00:00.000Z",
                  group: {
                    groupId: "activity-group-1",
                    ordinal: 1,
                    kind: "task",
                    actorIds: ["chair-1"],
                    target: { kind: "task", id: "task-1" },
                    eventKinds: ["task-updated"],
                    occurredAtRange: {
                      first: "2026-07-26T00:00:00.000Z",
                      last: "2026-07-26T00:00:00.000Z",
                    },
                    sourceRange: { first: 5, last: 5 },
                    count: 1,
                    evidenceLinkCount: 0,
                    evidenceLinksDigest: `sha256:${"b".repeat(64)}`,
                    evidenceLinksTruncated: false,
                    evidenceLinks: [],
                    members: [{
                      ordinal: 1,
                      eventId: "event-1",
                      eventKind: "task-updated",
                      actorId: "chair-1",
                      target: { kind: "task", id: "task-1" },
                      occurredAt: "2026-07-26T00:00:00.000Z",
                      sourceRevision: 5,
                      detailAvailability: "available",
                      evidenceLinkCount: 0,
                      evidenceLinksDigest: `sha256:${"b".repeat(64)}`,
                    }],
                  },
                },
                detailRef: {
                  kind: "activity",
                  groupId: "activity-group-1",
                  expectedRevision: 5,
                },
                actionAvailability: {
                  state: "read-only",
                  reason: "state-ineligible",
                },
              },
            },
          }],
          issues: [{
            kind: "task",
            scope: target,
            taskId: "task-blocked",
            taskRevision: 2,
            state: "blocked",
            detailRef: { kind: "task", taskId: "task-blocked", expectedRevision: 2 },
          }],
          evidencePaths: [{
            path: "reports/result.md",
            digest: `sha256:${"a".repeat(64)}`,
          }],
          evidencePathObservation: "Observed",
        },
      },
    } as unknown as FabricConsoleDataset;

    const render = (columns: number, height: number): readonly string[] => {
      const rows = Array.from({ length: height }, () => " ".repeat(columns));
      renderFabricDetail(
        rows,
        columns,
        presentation,
        dataset,
        createFabricUiState(),
        "geometry",
        [],
        { x1: 1, y1: 1, x2: columns, y2: height },
      );
      return rows;
    };

    expect(render(44, 8).join("\n")).toContain("Target: SESSION session-1 | coordination");
    const wide = render(120, 24).join("\n");
    expect(wide).toContain("Lead: chair-1");
    expect(wide).toContain("Current milestone: Unobserved");
    expect(wide).toContain("Work states (server): blocked 1 | ready 2 | active 3 | complete 4");
    expect(wide).toContain("Activity group: activity-group-1 | task | count 1");
    expect(wide).toContain("Blocking issue: task task-blocked r2 | blocked");
    expect(wide).toContain("Evidence path: reports/result.md");

    const result = (dataset.inspection as {
      result: {
        composition: Record<string, unknown>;
        evidencePathObservation: string;
      };
    }).result;
    result.composition.identity = {
      freshness: "unavailable",
      source: "fabric",
      revision: 2,
      observedAt: "2026-07-26T00:00:00.000Z",
      reason: "not observed",
    };
    result.composition.declaredProgress = {
      freshness: "conflict",
      source: "fabric",
      revision: 2,
      observedAt: "2026-07-26T00:00:00.000Z",
      candidates: [{ observation: "Unobserved" }, { observation: "Unobserved" }],
    };
    result.evidencePathObservation = "Unknown";
    (result as unknown as { issues: unknown[] }).issues.push({
      kind: "task-fact-conflict",
      scope: target,
      taskId: "task-conflict",
      taskRevision: 3,
      detailRef: { kind: "task", taskId: "task-conflict", expectedRevision: 3 },
    });
    const unavailable = render(120, 24).join("\n");
    expect(unavailable).toContain("Lead: Unobserved");
    expect(unavailable).toContain("Work states: Unknown");
    expect(unavailable).toContain("Evidence path: Unknown");
    expect(unavailable).toContain("Blocking issue: task task-conflict | Unknown");

    result.composition.identity = {
      freshness: "conflict",
      source: "fabric",
      revision: 3,
      observedAt: "2026-07-26T00:00:00.000Z",
      candidates: [{}, {}],
    };
    result.composition.declaredProgress = {
      freshness: "unavailable",
      source: "fabric",
      revision: 3,
      observedAt: "2026-07-26T00:00:00.000Z",
      reason: "not observed",
    };
    const contradictory = render(120, 24).join("\n");
    expect(contradictory).toContain("Lead: Unknown");
    expect(contradictory).toContain("Work states: Unobserved");
  });

  it("preserves logical review anchors through the review surface seam", () => {
    const wrapped = wrapFabricReviewContent(
      { lines: ["abcdef", "xy"], requiredContextLineCount: 1 },
      3,
    );

    expect(wrapped).toStrictEqual({
      lines: [
        { value: "abc", start: 0, end: 3 },
        { value: "def", start: 3, end: 7 },
        { value: "xy", start: 7, end: 10 },
      ],
      requiredEnd: 7,
    });
  });
});
