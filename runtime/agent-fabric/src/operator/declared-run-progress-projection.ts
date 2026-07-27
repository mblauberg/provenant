import type { DeclaredRunProgress, DeclaredRunTaskStateCounts } from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { currentRunPlanBinding } from "../project-session/run-plan-store.js";
import { integer, row, text } from "../project-session/store-support.js";

export type ServerRunTaskStates =
  | { status: "observed"; counts: DeclaredRunTaskStateCounts }
  | { status: "unknown"; reason: string };

/** Count every server-side task state in one run, independent of page bounds. */
export function projectServerRunTaskStates(
  database: Database.Database,
  runId: string,
): ServerRunTaskStates {
  const counts: DeclaredRunTaskStateCounts = {
    blocked: 0, ready: 0, active: 0, complete: 0, cancelled: 0, degraded: 0,
  };
  const values = database.prepare(`
    SELECT state, COUNT(*) AS tasks FROM tasks WHERE run_id=? GROUP BY state
  `).all(runId);
  for (const value of values) {
    const stored = row(value, "run task-state count");
    const state = text(stored, "state");
    if (!Object.hasOwn(counts, state)) return { status: "unknown", reason: `unrecognised task state: ${state}` };
    counts[state as keyof DeclaredRunTaskStateCounts] = integer(stored, "tasks");
  }
  return { status: "observed", counts };
}

/** Project server-scoped task counts without deriving any undeclared total. */
export function projectDeclaredRunProgress(
  database: Database.Database,
  runId: string,
): DeclaredRunProgress {
  const states = projectServerRunTaskStates(database, runId);
  if (states.status === "unknown") return { plan: "unknown", reason: states.reason };
  const counts = states.counts;
  const binding = currentRunPlanBinding(database, runId);
  if (binding?.declaredTaskDenominator === null || binding === null) {
    return { plan: "open", counts };
  }
  const classifiedTasks = Object.values(counts).reduce((sum, count) => sum + count, 0);
  if (classifiedTasks > binding.declaredTaskDenominator) {
    return {
      plan: "unknown",
      reason: `task count ${String(classifiedTasks)} exceeds plan r${String(binding.planRevision)} denominator ${String(binding.declaredTaskDenominator)}`,
    };
  }
  return {
    plan: "finite",
    planRevision: binding.planRevision,
    counts,
    declaredTaskDenominator: binding.declaredTaskDenominator,
  };
}

/** Count tasks bound to one exact delivery workstream, independent of page bounds. */
export function projectDeliveryWorkstreamProgress(
  database: Database.Database,
  input: Readonly<{
    coordinationRunId: string;
    workstreamId: string;
    deliveryRunId: string;
  }>,
): DeclaredRunProgress {
  const counts: DeclaredRunTaskStateCounts = {
    blocked: 0, ready: 0, active: 0, complete: 0, cancelled: 0, degraded: 0,
  };
  const values = database.prepare(`
    WITH RECURSIVE scoped_teams(team_id) AS (
      SELECT custody.team_id
        FROM workstream_custody custody
        JOIN workstreams stream ON stream.workstream_id=custody.workstream_id
       WHERE stream.coordination_run_id=? AND stream.workstream_id=?
         AND stream.delivery_run_id=?
      UNION
      SELECT child.team_id
        FROM teams child
        JOIN scoped_teams parent ON child.parent_team_id=parent.team_id
       WHERE child.run_id=?
    ),
    scoped_tasks(task_id) AS (
      SELECT fabric_task_id FROM workstreams
       WHERE coordination_run_id=? AND workstream_id=? AND delivery_run_id=?
         AND fabric_task_id IS NOT NULL
      UNION
      SELECT owned.task_id
        FROM team_owned_tasks owned
        JOIN scoped_teams scoped ON scoped.team_id=owned.team_id
       WHERE owned.run_id=?
    )
    SELECT task.state, COUNT(*) AS tasks
      FROM tasks task
      JOIN scoped_tasks scoped ON scoped.task_id=task.task_id
     WHERE task.run_id=?
     GROUP BY task.state
  `).all(
    input.coordinationRunId,
    input.workstreamId,
    input.deliveryRunId,
    input.coordinationRunId,
    input.coordinationRunId,
    input.workstreamId,
    input.deliveryRunId,
    input.coordinationRunId,
    input.coordinationRunId,
  );
  for (const value of values) {
    const stored = row(value, "delivery workstream task-state count");
    const state = text(stored, "state");
    if (!Object.hasOwn(counts, state)) {
      return { plan: "unknown", reason: `unrecognised task state: ${state}` };
    }
    counts[state as keyof DeclaredRunTaskStateCounts] = integer(stored, "tasks");
  }
  return { plan: "open", counts };
}

/** Latest recorded event for a task bound to one exact delivery workstream. */
export function deliveryWorkstreamProgressObservedAt(
  database: Database.Database,
  input: Readonly<{
    coordinationRunId: string;
    workstreamId: string;
    deliveryRunId: string;
  }>,
): number | null {
  const value = database.prepare(`
    WITH RECURSIVE scoped_teams(team_id) AS (
      SELECT custody.team_id
        FROM workstream_custody custody
        JOIN workstreams stream ON stream.workstream_id=custody.workstream_id
       WHERE stream.coordination_run_id=? AND stream.workstream_id=?
         AND stream.delivery_run_id=?
      UNION
      SELECT child.team_id
        FROM teams child
        JOIN scoped_teams parent ON child.parent_team_id=parent.team_id
       WHERE child.run_id=?
    ),
    scoped_tasks(task_id) AS (
      SELECT fabric_task_id FROM workstreams
       WHERE coordination_run_id=? AND workstream_id=? AND delivery_run_id=?
         AND fabric_task_id IS NOT NULL
      UNION
      SELECT owned.task_id
        FROM team_owned_tasks owned
        JOIN scoped_teams scoped ON scoped.team_id=owned.team_id
       WHERE owned.run_id=?
    )
    SELECT MAX(observation.observed_at) AS observed_at
      FROM (
        SELECT event.created_at AS observed_at
          FROM events event
          JOIN scoped_tasks scoped
            ON json_extract(event.payload_json, '$.taskId')=scoped.task_id
         WHERE event.run_id=?
        UNION ALL
        SELECT result.created_at AS observed_at
          FROM task_results result
          JOIN scoped_tasks scoped ON scoped.task_id=result.task_id
         WHERE result.run_id=?
        UNION ALL
        SELECT membership.updated_at AS observed_at
          FROM project_session_memberships membership
          JOIN scoped_tasks scoped ON scoped.task_id=membership.member_id
         WHERE membership.coordination_run_id=? AND membership.member_kind='task'
      ) observation
  `).get(
    input.coordinationRunId,
    input.workstreamId,
    input.deliveryRunId,
    input.coordinationRunId,
    input.coordinationRunId,
    input.workstreamId,
    input.deliveryRunId,
    input.coordinationRunId,
    input.coordinationRunId,
    input.coordinationRunId,
    input.coordinationRunId,
  );
  if (value === undefined) return null;
  const stored = row(value, "delivery workstream progress observation");
  return stored.observed_at === null ? null : integer(stored, "observed_at");
}
