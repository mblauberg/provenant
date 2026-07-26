import { attentionDeckLayoutFor, chromeText, fitCells, writeFixedCells, type Rect } from "./layout.js";
import type { PresentedDeckRow, PresentedHeader, PresentedRow, FabricConsolePresentation, FabricConsoleUiState } from "./presenter.js";
import type { FabricConsoleDataset } from "./protocol-adapter.js";
import type { FabricHitBinding, FabricHitRegion } from "./index.js";
import { attentionPinId, deckPinId } from "./view-filter.js";
import { compactDeckRosterRows, visibleDeckOffset } from "./attention-deck-navigation.js";

type AttentionDeckRenderInput = Readonly<{
  rows: string[];
  columns: number;
  presentation: FabricConsolePresentation;
  dataset: FabricConsoleDataset;
  ui: FabricConsoleUiState;
  geometryKey: string;
  hitRegions: FabricHitRegion[];
  bounds: Rect;
}>;

function setRow(rows: string[], row: number, columns: number, value: string): void {
  if (row < 1 || row > rows.length) return;
  rows[row - 1] = fitCells(chromeText(value), columns);
}

function writeBoundsRow(
  rows: string[],
  columns: number,
  bounds: Rect,
  row: number,
  value: string,
): void {
  if (bounds.x1 === 1 && bounds.x2 === columns) {
    setRow(rows, row, columns, value);
    return;
  }
  rows[row - 1] = writeFixedCells(
    rows[row - 1] ?? " ".repeat(columns),
    bounds.x1,
    bounds.x2 - bounds.x1 + 1,
    chromeText(value),
  );
}

function bindingFor(
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
        ...(row.view === "runs"
          ? (() => {
              const source = dataset.pages.runs.rows.find(
                (candidate) =>
                  candidate.stableId === row.stableId &&
                  candidate.revision === row.revision,
              );
              return source?.summary?.kind === "run" &&
                source.summary.projectSessionId !== undefined &&
                source.detailRef?.kind === "run"
                ? {
                    projectSessionId: source.summary.projectSessionId,
                    runTarget: {
                      kind: "coordination-run" as const,
                      coordinationRunId: source.detailRef.coordinationRunId,
                    },
                  }
                : {};
            })()
          : {}),
      };
}

function deckBinding(
  dataset: FabricConsoleDataset,
  row: PresentedDeckRow,
): FabricHitBinding | null {
  if (
    row.sourceRow === null ||
    row.projectSessionId === null ||
    row.coordinationRunId === null
  ) {
    return null;
  }
  const base = bindingFor(dataset, row.sourceRow);
  if (base === null) return null;
  return {
    ...base,
    projectSessionId: row.projectSessionId as never,
    runTarget: row.kind === "workstream" &&
      row.deliveryRunId !== null
      ? {
          kind: "delivery-workstream",
          coordinationRunId: row.coordinationRunId as never,
          deliveryRunId: row.deliveryRunId as never,
          workstreamId: row.entityId as never,
        }
      : {
          kind: "coordination-run",
          coordinationRunId: row.coordinationRunId as never,
        },
  };
}

function rowText(row: PresentedRow, focused: boolean, pinned: boolean): string {
  const pin = pinned ? "^ PINNED " : "";
  return `${focused ? ">" : row.selected ? "*" : " "}${row.urgencyMarker.padEnd(2, " ")} ${pin}${row.primary} | ${row.secondary} | ${row.freshness} | r${row.revision}`;
}

function deckRowText(row: PresentedDeckRow, focused: boolean, pinned: boolean): string {
  const pin = pinned ? "^ PINNED " : "";
  return `${focused ? ">" : " "}${row.urgencyMarker.padEnd(2, " ")} ${pin}${row.primary} | ${row.statusLabel} | ${row.secondary}`;
}

export function stripHeaderLines(header: PresentedHeader): readonly string[] {
  return [
    `Project:${header.project}`,
    `Session:${header.session}`,
    `Run:${header.run}`,
    `Revision:r${header.revision ?? "?"}`,
    `Fresh:${header.freshness.toUpperCase()}`,
    `Phase:${header.phase}`,
    `Owner:${header.owner}`,
    `Next:${header.nextMilestone}`,
    `Health:${header.health}`,
    `Need you:${String(header.needsYouCount)} | Watch:${String(header.watchCount)} collapsed`,
  ];
}

export function filteredDeckLabel(
  presentation: FabricConsolePresentation,
  compact = false,
): string {
  return compact
    ? `FILTERED ${String(presentation.deckShownCount)}/${String(presentation.deckUnfilteredCount)}`
    : `FILTERED VIEW, ${String(presentation.deckShownCount)} of ${String(presentation.deckUnfilteredCount)} shown`;
}

export function compactAttentionRowText(
  row: PresentedRow,
  focused: boolean,
  pinnedRowIds: readonly string[] = [],
): string {
  const marker = focused ? ">" : row.selected ? "*" : " ";
  const [state = "UNAVAILABLE", age = ""] = row.freshness.split(" ");
  const freshness = state === "LIVE" ? age : `${state} ${age}`.trim();
  const pin = pinnedRowIds.includes(attentionPinId(row)) ? "^ PINNED " : "";
  return `${marker}${row.urgencyMarker.padEnd(2, " ")} ${pin}${row.primary} ${freshness}`;
}

function compactDeckText(row: PresentedDeckRow, focused: boolean, pinned: boolean): string {
  const phase = row.phase ?? row.state ?? "unknown";
  const age = row.freshness?.split(" ").at(-1) ?? "";
  const gutter = row.urgencyMarker.trim() === ""
    ? focused ? ">  " : ""
    : `${focused ? ">" : " "}${row.urgencyMarker.padEnd(2, " ")} `;
  return `${gutter}${pinned ? "^ PINNED " : ""}${row.entityId} ${phase} ${row.statusLabel}${age === "" ? "" : ` ${age}`}`;
}

function renderCompactRemainder(input: AttentionDeckRenderInput, bounds: Rect): void {
  const { rows, columns, presentation, dataset, ui, geometryKey, hitRegions } = input;
  const available = Math.max(0, bounds.y2 - bounds.y1 + 1);
  const renderedAttention = presentation.needsYouRows.find(({ selected }) => selected) ??
    presentation.needsYouRows[0];
  const remainingAttention = presentation.needsYouRows
    .filter(({ stableId }) => stableId !== renderedAttention?.stableId)
    .slice(0, Math.max(0, available - 1));
  let y = bounds.y1;
  for (const item of remainingAttention) {
    const id = `row:${item.view}:${item.stableId}`;
    writeBoundsRow(rows, columns, bounds, y, compactAttentionRowText(
      item,
      presentation.focusId === id,
      ui.pinnedRowIds,
    ));
    hitRegions.push({
      id,
      kind: "row",
      rect: { x1: bounds.x1, y1: y, x2: bounds.x2, y2: y },
      enabled: true,
      geometryKey,
      binding: bindingFor(dataset, item),
    });
    y += 1;
  }
  const rosterCapacity = Math.max(0, bounds.y2 - y + 1);
  const roster = compactDeckRosterRows(presentation.deckRows, ui.pinnedRowIds);
  const maximumOffset = Math.max(0, roster.length - rosterCapacity);
  const offset = visibleDeckOffset(
    roster,
    presentation.focusId,
    ui.deckScrollOffset,
    rosterCapacity,
  );
  for (const item of roster.slice(offset, offset + rosterCapacity)) {
    const id = `deck:${item.stableId}`;
    writeBoundsRow(rows, columns, bounds, y, compactDeckText(
      item,
      presentation.focusId === id,
      ui.pinnedRowIds.includes(deckPinId(item)),
    ));
    hitRegions.push({
      id,
      kind: "row",
      rect: { x1: bounds.x1, y1: y, x2: bounds.x2, y2: y },
      enabled: true,
      geometryKey,
      binding: deckBinding(dataset, item),
      scrollMaximum: maximumOffset,
    });
    y += 1;
  }
}

function renderAttention(input: AttentionDeckRenderInput, bounds: Rect): void {
  const { rows, columns, presentation, dataset, ui, geometryKey, hitRegions } = input;
  writeBoundsRow(
    rows,
    columns,
    bounds,
    bounds.y1,
    presentation.deckFilterActive
      ? filteredDeckLabel(presentation)
      : `NEEDS YOU:${String(presentation.header.needsYouCount)} | WATCH:${String(presentation.header.watchCount)} collapsed`,
  );
  const capacity = Math.max(0, bounds.y2 - bounds.y1);
  const latestWatch = presentation.watchRows[0] ?? null;
  const showWatchSummary = latestWatch !== null &&
    capacity > Math.max(1, presentation.needsYouRows.length);
  const needsCapacity = Math.max(0, capacity - (showWatchSummary ? 1 : 0));
  const maximumOffset = Math.max(0, presentation.needsYouRows.length - needsCapacity);
  const offset = Math.min(
    maximumOffset,
    Math.max(0, Math.trunc(ui.scrollOffsetByView.attention ?? 0)),
  );
  for (const [index, item] of presentation.needsYouRows
    .slice(offset, offset + needsCapacity)
    .entries()) {
    const y = bounds.y1 + index + 1;
    const id = `row:${item.view}:${item.stableId}`;
    writeBoundsRow(rows, columns, bounds, y, rowText(
      item,
      presentation.focusId === id,
      ui.pinnedRowIds.includes(attentionPinId(item)),
    ));
    hitRegions.push({
      id,
      kind: "row",
      rect: { x1: bounds.x1, y1: y, x2: bounds.x2, y2: y },
      enabled: true,
      geometryKey,
      binding: bindingFor(dataset, item),
    });
  }
  let nextRow = bounds.y1 + Math.min(needsCapacity, presentation.needsYouRows.length) + 1;
  if (presentation.needsYouRows.length === 0 && needsCapacity > 0) {
    writeBoundsRow(
      rows,
      columns,
      bounds,
      bounds.y1 + 1,
      presentation.deckFilterActive
        ? "No rows match the active filter."
        : "No projected user judgement required.",
    );
    nextRow += 1;
  }
  if (showWatchSummary && latestWatch !== null && nextRow <= bounds.y2) {
    writeBoundsRow(
      rows,
      columns,
      bounds,
      nextRow,
      `WATCH latest: ${ui.pinnedRowIds.includes(attentionPinId(latestWatch)) ? "^ PINNED " : ""}${latestWatch.primary} | ${latestWatch.freshness}`,
    );
  }
}

function renderRoster(input: AttentionDeckRenderInput, bounds: Rect): void {
  const { rows, columns, presentation, dataset, ui, geometryKey, hitRegions } = input;
  const width = bounds.x2 - bounds.x1 + 1;
  const header = presentation.deckFilterActive
    ? filteredDeckLabel(presentation)
    : width < 40
    ? `N:${String(presentation.header.needsYouCount)} W:${String(presentation.header.watchCount)} R:${String(presentation.deckTotalCount)} RUN:${String(presentation.deckRunCount)}`
    : `PROJECTED ROSTER:${String(presentation.deckTotalCount)} | RUN IDENTITIES:${String(presentation.deckRunCount)}`;
  writeBoundsRow(
    rows,
    columns,
    bounds,
    bounds.y1,
    header,
  );
  const capacity = Math.max(0, bounds.y2 - bounds.y1);
  const maximumOffset = Math.max(0, presentation.deckRows.length - capacity);
  const offset = Math.min(maximumOffset, Math.max(0, Math.trunc(ui.deckScrollOffset)));
  const visibleItems = presentation.deckRows.slice(offset, offset + capacity);
  for (const [index, item] of visibleItems.entries()) {
    const y = bounds.y1 + index + 1;
    const id = `deck:${item.stableId}`;
    writeBoundsRow(rows, columns, bounds, y, deckRowText(
      item,
      presentation.focusId === id,
      ui.pinnedRowIds.includes(deckPinId(item)),
    ));
    hitRegions.push({
      id,
      kind: "row",
      rect: { x1: bounds.x1, y1: y, x2: bounds.x2, y2: y },
      enabled: true,
      geometryKey,
      binding: deckBinding(dataset, item),
      scrollMaximum: maximumOffset,
    });
  }
  if (presentation.deckRows.length === 0 && capacity > 0) {
    writeBoundsRow(
      rows,
      columns,
      bounds,
      bounds.y1 + 1,
      presentation.deckFilterActive
        ? "No rows match the active filter."
        : "No projected runs.",
    );
  }

  let nextRow = bounds.y1 + visibleItems.length + 1;
  const detailPriority = new Map([
    ["Native notification", 0],
    ["Notification basis", 1],
  ]);
  const detailLines = [...(presentation.detail?.lines ?? [])].sort(
    (left, right) =>
      (detailPriority.get(left.label) ?? 2) -
      (detailPriority.get(right.label) ?? 2),
  );
  for (const detail of detailLines) {
    if (nextRow > bounds.y2) break;
    writeBoundsRow(rows, columns, bounds, nextRow, `${detail.label}: ${detail.value}`);
    nextRow += 1;
  }
  const selected = presentation.masterRows.find(
    (row) => row.stableId === presentation.detail?.stableId,
  );
  const detailStart = bounds.y1 + visibleItems.length + 1;
  if (selected !== undefined && detailStart < nextRow) {
    hitRegions.push({
      id: `detail:${presentation.activeView}:${selected.stableId}`,
      kind: "pager",
      rect: { x1: bounds.x1, y1: detailStart, x2: bounds.x2, y2: nextRow - 1 },
      enabled: true,
      geometryKey,
      binding: bindingFor(dataset, selected),
      scrollMaximum: 0,
    });
  }
}

export function renderFabricDeckRoster(input: AttentionDeckRenderInput): void {
  if (input.columns === 30 && input.rows.length === 6) {
    renderCompactRemainder(input, input.bounds);
    return;
  }
  renderRoster(input, input.bounds);
}

export function renderFabricAttentionDeck(input: AttentionDeckRenderInput): void {
  const { rows, columns, presentation, bounds } = input;
  if (attentionDeckLayoutFor({ columns, rows: rows.length }) === "simultaneous") {
    const attentionWidth = Math.max(30, Math.min(54, Math.floor(columns * 0.42)));
    const attention = { ...bounds, x2: attentionWidth };
    const roster = { ...bounds, x1: attentionWidth + 2 };
    renderAttention(input, attention);
    for (let y = bounds.y1; y <= bounds.y2; y += 1) {
      rows[y - 1] = writeFixedCells(
        rows[y - 1] ?? " ".repeat(columns),
        attentionWidth + 1,
        1,
        "|",
      );
    }
    renderRoster(input, roster);
    return;
  }

  const height = bounds.y2 - bounds.y1 + 1;
  const watchLineCount = presentation.watchRows.length === 0 ? 0 : 1;
  const attentionHeight = Math.min(
    Math.max(2, presentation.needsYouRows.length + 1 + watchLineCount),
    Math.max(2, Math.floor(height * 0.45)),
  );
  const attention = { ...bounds, y2: bounds.y1 + attentionHeight - 1 };
  const roster = { ...bounds, y1: Math.min(bounds.y2, attention.y2 + 1) };
  renderAttention(input, attention);
  renderRoster(input, roster);
}
