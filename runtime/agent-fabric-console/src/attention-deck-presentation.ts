import type { ConsoleRow } from "./model.js";
import type { FabricConsoleDataset } from "./protocol-adapter.js";
import type { PresentedDeckRow } from "./presenter-model.js";
import { presentRow } from "./row-presentation.js";
import {
  compareProjectedIds,
  groupProjectedRosterFacts,
} from "./attention-deck-grouping.js";

type DeckStatus = Readonly<{ urgencyMarker: string; statusLabel: string }>;

function absentStatus(): DeckStatus {
  return { urgencyMarker: "?", statusLabel: "UNAVAILABLE" };
}

function sessionStatus(state: string | undefined): DeckStatus {
  if (state === "active") {
    return { urgencyMarker: " ", statusLabel: "ACTIVE" };
  }
  if (state === "visibility_degraded") {
    return { urgencyMarker: "~", statusLabel: "DEGRADED" };
  }
  return absentStatus();
}

function runStatus(row: ConsoleRow<"runs">): DeckStatus {
  const freshness = row.freshness.state;
  if (freshness === "stale" || freshness === "unavailable" || freshness === "conflict") {
    return { urgencyMarker: "?", statusLabel: freshness.toUpperCase() };
  }
  if (row.summary?.kind !== "run") return absentStatus();
  const health = row.summary.health;
  if (health === "degraded") {
    return { urgencyMarker: "~", statusLabel: "DEGRADED" };
  }
  return { urgencyMarker: " ", statusLabel: health?.toUpperCase() ?? "UNKNOWN" };
}

function stableId(kind: PresentedDeckRow["kind"], ...ids: readonly string[]): string {
  return `${kind}:${ids.map((id) => encodeURIComponent(id)).join(":")}`;
}

function secondary(
  values: readonly Readonly<{ label: string; value: string | null }>[],
): string {
  return values
    .map(({ label, value }) => `${label}:${value ?? "not projected"}`)
    .join(" | ");
}

export function presentDeckRows(
  dataset: FabricConsoleDataset,
): Readonly<{ rows: readonly PresentedDeckRow[]; totalCount: number; runCount: number }> {
  const runRows = dataset.pages.runs.rows.filter(
    (row): row is ConsoleRow<"runs"> =>
      row.view === "runs" && row.summary?.kind === "run",
  );
  const sessionChoices = dataset.projectSessions?.choices ?? [];
  const allRows: PresentedDeckRow[] = [];

  const groups = groupProjectedRosterFacts(
    sessionChoices,
    runRows,
    (session) => String(session.projectSessionId),
    (row) => {
      const summary = row.summary;
      if (summary?.kind !== "run") {
        throw new TypeError("Deck run group requires a projected run summary");
      }
      if (summary.projectSessionId === undefined) {
        throw new TypeError("exact run projection has no project-session identity");
      }
      return String(summary.projectSessionId);
    },
    (row) => row.stableId,
  );
  for (const { projectSessionId, session: choice, runs } of groups) {
    if (choice !== null) {
      const status = sessionStatus(choice.state);
      const sessionStableId = stableId("session", projectSessionId);
      allRows.push({
        kind: "session",
        stableId: sessionStableId,
        entityId: projectSessionId,
        projectSessionId,
        coordinationRunId: null,
        deliveryRunId: null,
        owner: null,
        phase: null,
        state: choice.state,
        health: null,
        freshness: null,
        lastEvent: choice.lastEventAt,
        updatedAt: null,
        nextMilestone: null,
        ...status,
        primary: `SESSION ${projectSessionId}`,
        secondary: secondary([
          { label: "mode", value: choice.mode },
          { label: "state", value: choice.state },
          { label: "last event", value: choice.lastEventAt },
        ]),
        sourceRow: null,
      });
    }

    for (const runRow of runs) {
      const summary = runRow.summary;
      if (summary?.kind !== "run") continue;
      const presented = presentRow(runRow, false, false, dataset);
      const status = runStatus(runRow);
      const coordinationStableId = stableId(
        "coordination",
        projectSessionId,
        runRow.stableId,
      );
      allRows.push({
        kind: "coordination",
        stableId: coordinationStableId,
        entityId: runRow.stableId,
        projectSessionId,
        coordinationRunId: runRow.stableId,
        deliveryRunId: null,
        owner: String(summary.identity.chairAgentId),
        phase: summary.phase,
        state: null,
        health: summary.health,
        freshness: presented.freshness,
        lastEvent: summary.identity.lastEventAt,
        updatedAt: null,
        nextMilestone: summary.nextMilestone,
        ...status,
        primary: choice === null
          ? `COORDINATION ${runRow.stableId} | SESSION ${projectSessionId}`
          : `COORDINATION ${runRow.stableId}`,
        secondary: secondary([
          { label: "owner", value: String(summary.identity.chairAgentId) },
          { label: "phase", value: summary.phase },
          { label: "health", value: summary.health },
          { label: "fresh", value: presented.freshness },
          { label: "last event", value: summary.identity.lastEventAt },
          { label: "next", value: summary.nextMilestone },
        ]),
        sourceRow: presented,
      });

      for (const workstream of [...summary.identity.workstreams].sort(
        (left, right) =>
          compareProjectedIds(String(left.workstreamId), String(right.workstreamId)) ||
          compareProjectedIds(String(left.deliveryRunId), String(right.deliveryRunId)),
      )) {
        const workstreamStableId = stableId(
          "workstream",
          projectSessionId,
          runRow.stableId,
          String(workstream.workstreamId),
        );
        allRows.push({
          kind: "workstream",
          stableId: workstreamStableId,
          entityId: String(workstream.workstreamId),
          projectSessionId,
          coordinationRunId: runRow.stableId,
          deliveryRunId: String(workstream.deliveryRunId),
          owner: String(workstream.leadAgentId),
          phase: null,
          state: workstream.state,
          health: null,
          freshness: null,
          lastEvent: null,
          updatedAt: workstream.updatedAt,
          nextMilestone: null,
          ...absentStatus(),
          primary: `WORKSTREAM ${String(workstream.workstreamId)} | DELIVERY ${String(workstream.deliveryRunId)}`,
          secondary: secondary([
            { label: "owner", value: String(workstream.leadAgentId) },
            { label: "state", value: workstream.state },
            { label: "phase", value: null },
            { label: "health", value: null },
            { label: "fresh", value: "UNAVAILABLE" },
            { label: "last event", value: null },
            { label: "updated", value: workstream.updatedAt },
          ]),
          sourceRow: presented,
        });
      }
    }
  }

  for (const runRow of dataset.pages.runs.rows.filter(
    (row) => row.summary?.kind !== "run",
  )) {
    const presented = presentRow(runRow, false, false, dataset);
    const status = runStatus(runRow);
    const coordinationStableId = stableId("coordination", "unscoped", runRow.stableId);
    allRows.push({
      kind: "coordination",
      stableId: coordinationStableId,
      entityId: runRow.stableId,
      projectSessionId: null,
      coordinationRunId: runRow.stableId,
      deliveryRunId: null,
      owner: null,
      phase: null,
      state: null,
      health: null,
      freshness: presented.freshness,
      lastEvent: null,
      updatedAt: null,
      nextMilestone: null,
      ...status,
      primary: `COORDINATION ${runRow.stableId} | SESSION not projected`,
      secondary: secondary([
        { label: "owner", value: null },
        { label: "phase", value: null },
        { label: "health", value: null },
        { label: "fresh", value: presented.freshness },
        { label: "last event", value: null },
        { label: "next", value: null },
      ]),
      sourceRow: presented,
    });
  }

  const runCount = allRows.filter(({ kind }) => kind !== "session").length;
  return { rows: allRows, totalCount: allRows.length, runCount };
}
