import type {
  OperatorActionAvailability,
  OperatorDetailRef,
  OperatorViewRow,
  ProjectionViewItemMap,
  ProjectId,
  ProjectSessionId,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { isRow, row, type Row } from "../persistence/row-codec.js";
import {
  activityItems,
  activityNarrativeGroups,
  activityRows,
  loadActivityDetail,
  type ActivityNarrativeGroupingProjection,
  type LoadedActivityDetail,
} from "./activity-projection.js";
import type { ProjectedActivityNarrativeGroup } from "./activity-grouping.js";

type CachedActivityNarrativeProjection = Readonly<{
  scopeKey: string;
  snapshotRevision: number;
  groups: readonly ProjectedActivityNarrativeGroup[];
}>;

export class ActivityProjectionCache {
  readonly #database: Database.Database;
  #cachedNarrativeProjection: CachedActivityNarrativeProjection | null = null;

  constructor(database: Database.Database) {
    this.#database = database;
  }

  invalidateUnlessRevision(snapshotRevision: number): void {
    if (
      this.#cachedNarrativeProjection !== null &&
      this.#cachedNarrativeProjection.snapshotRevision !== snapshotRevision
    ) {
      this.#cachedNarrativeProjection = null;
    }
  }

  items(
    projectId: ProjectId,
    projectSessionId: ProjectSessionId | undefined,
    grouping: ActivityNarrativeGroupingProjection,
    snapshotRevision: number,
  ): ProjectionViewItemMap["activity"][] {
    if (grouping === "include") {
      return activityItems(
        this.#database,
        [],
        "include",
        this.#projectActivityNarrativeGroups(
          projectId,
          projectSessionId,
          snapshotRevision,
        ),
      );
    }
    return activityItems(
      this.#database,
      this.#activityEventRows(projectId, projectSessionId),
      "omit",
    );
  }

  rows(
    projectId: ProjectId,
    projectSessionId: ProjectSessionId | undefined,
    availability: OperatorActionAvailability,
    grouping: ActivityNarrativeGroupingProjection,
    snapshotRevision: number,
  ): OperatorViewRow<"activity">[] {
    if (grouping === "include") {
      return activityRows(
        this.#database,
        [],
        availability,
        "include",
        this.#projectActivityNarrativeGroups(
          projectId,
          projectSessionId,
          snapshotRevision,
        ),
      );
    }
    return activityRows(
      this.#database,
      this.#activityEventRows(projectId, projectSessionId),
      availability,
      "omit",
    );
  }

  detail(
    detailRef: Extract<OperatorDetailRef, { kind: "activity" }>,
    projectId: ProjectId,
    projectSessionId: ProjectSessionId | undefined,
    grouping: ActivityNarrativeGroupingProjection,
    snapshotRevision: number,
  ): LoadedActivityDetail {
    if (grouping === "include") {
      return loadActivityDetail(
        this.#database,
        [],
        detailRef,
        "include",
        this.#projectActivityNarrativeGroups(
          projectId,
          projectSessionId,
          snapshotRevision,
        ),
      );
    }
    if ("groupId" in detailRef) {
      return loadActivityDetail(this.#database, [], detailRef, "omit");
    }
    const value = projectSessionId === undefined
      ? this.#database.prepare(`
          SELECT e.*, seq.sequence, r.project_session_id FROM events e
          JOIN observer_event_sequence seq ON seq.event_id=e.event_id
          JOIN runs r ON r.run_id=e.run_id
          JOIN project_sessions s ON s.project_session_id=r.project_session_id
          WHERE e.event_id=? AND s.project_id=?
        `).get(detailRef.eventId, projectId)
      : this.#database.prepare(`
          SELECT e.*, seq.sequence, r.project_session_id FROM events e
          JOIN observer_event_sequence seq ON seq.event_id=e.event_id
          JOIN runs r ON r.run_id=e.run_id
          JOIN project_sessions s ON s.project_session_id=r.project_session_id
          WHERE e.event_id=? AND s.project_id=? AND r.project_session_id=?
        `).get(detailRef.eventId, projectId, projectSessionId);
    if (!isRow(value)) throw new Error("activity detail not found");
    return loadActivityDetail(this.#database, [value], detailRef, "omit");
  }

  #activityEventRows(
    projectId: ProjectId,
    projectSessionId: ProjectSessionId | undefined,
  ): Row[] {
    const values = projectSessionId === undefined
      ? this.#database.prepare(`
          SELECT e.*, seq.sequence, r.project_session_id FROM events e
          JOIN observer_event_sequence seq ON seq.event_id=e.event_id
          JOIN runs r ON r.run_id=e.run_id
          JOIN project_sessions s ON s.project_session_id=r.project_session_id
          WHERE s.project_id=?
          ORDER BY seq.sequence DESC
        `).all(projectId)
      : this.#database.prepare(`
          SELECT e.*, seq.sequence, r.project_session_id FROM events e
          JOIN observer_event_sequence seq ON seq.event_id=e.event_id
          JOIN runs r ON r.run_id=e.run_id
          WHERE r.project_session_id=?
          ORDER BY seq.sequence DESC
        `).all(projectSessionId);
    return values.map((value) => row(value, "operator projection row"));
  }

  #projectActivityNarrativeGroups(
    projectId: ProjectId,
    projectSessionId: ProjectSessionId | undefined,
    snapshotRevision: number,
  ): readonly ProjectedActivityNarrativeGroup[] {
    const scopeKey = projectSessionId === undefined
      ? `project\u0000${projectId}`
      : `session\u0000${projectId}\u0000${projectSessionId}`;
    if (
      this.#cachedNarrativeProjection?.scopeKey === scopeKey &&
      this.#cachedNarrativeProjection.snapshotRevision === snapshotRevision
    ) {
      return this.#cachedNarrativeProjection.groups;
    }
    const groups = activityNarrativeGroups(
      this.#database,
      this.#activityEventRows(projectId, projectSessionId),
    );
    this.#cachedNarrativeProjection = {
      scopeKey,
      snapshotRevision,
      groups,
    };
    return groups;
  }
}
