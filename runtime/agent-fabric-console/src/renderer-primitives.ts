import type {
  ProjectSessionId,
  RunProjectionTarget,
} from "@local/agent-fabric-protocol";

import type { ConsoleControllerState } from "./controller.js";
import {
  chromeText,
  fitCells,
  type FabricResponsiveMode,
  type Rect,
} from "./layout.js";
import type { FabricView, Revision } from "./model.js";
import type {
  FabricConsolePresentation,
  PresentedRow,
} from "./presenter.js";
import type { FabricConsoleDataset } from "./protocol-adapter.js";

export type FabricHitBinding = Readonly<{
  view: FabricView;
  itemId: string;
  itemRevision: Revision;
  projectionRevision: Revision;
  projectSessionId?: ProjectSessionId;
  runTarget?: RunProjectionTarget;
}>;

export type FabricHitRegion = Readonly<{
  id: string;
  kind: "tab" | "row" | "session" | "action" | "detach" | "pager" | "splitter" | "input";
  rect: Rect;
  enabled: boolean;
  geometryKey: string;
  binding: FabricHitBinding | null;
  shortcut?: string;
  scrollMaximum?: number;
}>;

export type FabricConsoleFrame = Readonly<{
  columns: number;
  rows: readonly string[];
  mode: FabricResponsiveMode;
  geometryKey: string;
  hitRegions: readonly FabricHitRegion[];
  presentation: FabricConsolePresentation;
  reviewCoverage?: FabricReviewCoverageObservation | null;
}>;

export type FabricReviewCoverageObservation = Readonly<{
  reviewKey: string;
  coveredThrough: number;
  requiredEnd: number;
  visibleStart: number;
  visibleEnd: number;
  visibleLineCount: number;
  previousAnchor: number;
  nextAnchor: number;
  endAnchor: number;
}>;

export function fabricGeometryKey(
  columns: number,
  rows: number,
  dataset: FabricConsoleDataset,
  controller: ConsoleControllerState,
): string {
  const revisions = dataset.pages[controller.activeView].rows
    .map((row) => `${row.stableId}@${row.revision}`)
    .join(",");
  return `${String(columns)}x${String(rows)}:r${dataset.snapshotRevision ?? "none"}:${controller.activeView}:${revisions}`;
}

export function setFabricRow(
  rows: string[],
  row: number,
  columns: number,
  value: string,
): void {
  if (row < 1 || row > rows.length) return;
  rows[row - 1] = fitCells(chromeText(value), columns);
}

export function presentedBinding(
  dataset: FabricConsoleDataset,
  row: PresentedRow,
): FabricHitBinding | null {
  return dataset.snapshotRevision === null
    ? null
    : {
        view: row.view,
        itemId: row.stableId,
        itemRevision: row.revision,
        projectionRevision: dataset.snapshotRevision,
        ...runBindingFields(dataset, row),
      };
}

function runBindingFields(
  dataset: FabricConsoleDataset,
  row: PresentedRow,
): Readonly<{
  projectSessionId?: ProjectSessionId;
  runTarget?: RunProjectionTarget;
}> {
  if (row.view !== "runs") return {};
  const source = dataset.pages.runs.rows.find(
    (candidate) => candidate.stableId === row.stableId && candidate.revision === row.revision,
  );
  if (
    source?.summary?.kind !== "run" ||
    source.summary.projectSessionId === undefined ||
    source.detailRef?.kind !== "run"
  ) {
    return {};
  }
  return {
    projectSessionId: source.summary.projectSessionId,
    runTarget: {
      kind: "coordination-run",
      coordinationRunId: source.detailRef.coordinationRunId,
    },
  };
}

export function reviewBinding(
  presentation: FabricConsolePresentation,
): FabricHitBinding | null {
  const review = presentation.review;
  return review === null || review.workflowId !== null
    ? null
    : {
        view: presentation.activeView,
        itemId: review.itemId,
        itemRevision: review.itemRevision,
        projectionRevision: review.projectionRevision,
      };
}

export function rowText(row: PresentedRow, focused: boolean, pinned = false): string {
  return `${focused ? ">" : row.selected ? "*" : " "}${row.urgencyMarker.padEnd(2, " ")} ${pinned ? "^ PINNED " : ""}${row.primary} | ${row.secondary} | ${row.freshness} | r${row.revision}`;
}
