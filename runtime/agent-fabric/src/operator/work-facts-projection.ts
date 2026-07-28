import {
  parseIdentifier,
  type WorkTaskState,
  type WorkWorkflowFacts,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError } from "../project-session/contracts.js";
import { currentRunPlanBinding } from "../project-session/run-plan-store.js";
import { integer, nullableText, row, text, type Row } from "../persistence/row-codec.js";
import { projectServerRunTaskStates } from "./declared-run-progress-projection.js";

const MAX_DEPENDENCIES = 1024;
const MAX_CHECKS = 256;
const MAX_BARRIERS = 256;
const MAX_WRITE_LEASES = 128;
const MAX_WRITE_PATHS = 256;

export type WorkFactsProjection = "include" | "omit";

export function workFactsProjectionField(
  database: Database.Database,
  task: Row,
  workflowRevision: number,
  projection: WorkFactsProjection,
): { workflow?: WorkWorkflowFacts } {
  return projection === "include"
    ? { workflow: projectWorkFacts(database, task, workflowRevision) }
    : {};
}

/** Project one task's workflow facts inside the caller's SQLite read transaction. */
export function projectWorkFacts(
  database: Database.Database,
  task: Row,
  workflowRevision: number,
): WorkWorkflowFacts {
  const runId = text(task, "run_id");
  const taskId = text(task, "task_id");
  const state = taskState(text(task, "state"));
  const ownerAgentId = nullableText(task, "owner_agent_id");
  const ownerLeaseGeneration = integer(task, "owner_lease_generation");
  const projectSessionId = text(task, "project_session_id");
  const dependencyRevision = positive(integer(task, "dependency_revision"), "workFacts.dependencyRevision");
  const taskStates = projectServerRunTaskStates(database, runId);
  if (taskStates.status === "unknown") {
    throw new ProjectFabricCoreError("RECOVERY_REQUIRED", taskStates.reason);
  }
  return {
    workflowRevision: positive(workflowRevision, "workFacts.workflowRevision"),
    objective: { observation: "Observed", value: text(task, "objective") },
    dependencies: {
      observation: "Observed",
      dependencyRevision,
      taskIds: dependencies(database, runId, taskId, projectSessionId, dependencyRevision),
    },
    coordinationRun: {
      observation: "Observed",
      projectSessionId: parseIdentifier<"ProjectSessionId">(
        projectSessionId,
        "workFacts.projectSessionId",
      ),
      coordinationRunId: parseIdentifier<"CoordinationRunId">(runId, "workFacts.coordinationRunId"),
    },
    workstream: workstream(database, runId, taskId),
    parentTask: { observation: "Unobserved" },
    plan: plan(database, runId),
    task: {
      observation: "Observed",
      state,
      owner: ownerAgentId === null
        ? { observation: "Unobserved" }
        : {
            observation: "Observed",
            agentId: parseIdentifier<"AgentId">(ownerAgentId, "workFacts.owner.agentId"),
            ownerLeaseGeneration: positive(ownerLeaseGeneration, "workFacts.owner.ownerLeaseGeneration"),
          },
    },
    checks: { observation: "Observed", items: checks(database, runId, taskId) },
    barriers: { observation: "Observed", items: barriers(database, runId, taskId) },
    declaredWriteScopes: { observation: "Observed", leases: writeLeases(database, runId, taskId) },
    runTaskStates: { observation: "Observed", counts: taskStates.counts },
  };
}

export function projectTaskCheckState(
  database: Database.Database,
  runId: string,
  taskId: string,
): "pending" | "passing" | "failing" | "unknown" {
  const values = database.prepare(`
    SELECT status FROM task_objective_checks WHERE run_id=? AND task_id=? ORDER BY check_id
  `).all(runId, taskId).map((value) => text(row(value, "task objective check"), "status"));
  if (values.length === 0) return "unknown";
  if (values.includes("fail")) return "failing";
  if (values.includes("pending")) return "pending";
  return values.every((value) => value === "pass") ? "passing" : "unknown";
}

function dependencies(
  database: Database.Database,
  runId: string,
  taskId: string,
  projectSessionId: string,
  dependencyRevision: number,
) {
  const values = database.prepare(`
    SELECT dependency_task_id, project_session_id, dependency_revision FROM task_dependencies
     WHERE run_id=? AND task_id=? ORDER BY dependency_task_id
     LIMIT ${String(MAX_DEPENDENCIES + 1)}
  `).all(runId, taskId);
  bounded(values, MAX_DEPENDENCIES, "dependencies");
  return values.map((value) => {
    const stored = row(value, "work dependency");
    if (stored.project_session_id !== projectSessionId || stored.dependency_revision !== dependencyRevision) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "stored work dependency binding is contradictory");
    }
    return parseIdentifier<"TaskId">(
      text(stored, "dependency_task_id"),
      "workFacts.dependencies.taskId",
    );
  });
}

function workstream(
  database: Database.Database,
  runId: string,
  taskId: string,
): WorkWorkflowFacts["workstream"] {
  const values = database.prepare(`
    SELECT workstream_id, delivery_run_id, revision, state FROM workstreams
     WHERE coordination_run_id=? AND fabric_task_id=?
    UNION
    SELECT stream.workstream_id, stream.delivery_run_id, stream.revision, stream.state
      FROM team_owned_tasks owned
      JOIN workstream_custody custody ON custody.team_id=owned.team_id
      JOIN workstreams stream ON stream.workstream_id=custody.workstream_id
       AND stream.coordination_run_id=owned.run_id
     WHERE owned.run_id=? AND owned.task_id=?
    ORDER BY workstream_id LIMIT 2
  `).all(runId, taskId, runId, taskId).map((value) => projectWorkstream(row(value, "work binding")));
  if (values.length === 0) return { observation: "Unobserved" };
  if (values.length > 1) return { observation: "Unknown", reason: "MultipleWorkstreamBindings" };
  return { observation: "Observed", ...values[0]! };
}

function projectWorkstream(value: Row): Omit<Extract<WorkWorkflowFacts["workstream"], {
  observation: "Observed";
}>, "observation"> {
  const state = text(value, "state");
  if (!["active", "complete", "cancelled", "degraded", "abandoned"].includes(state)) {
    throw new ProjectFabricCoreError("RECOVERY_REQUIRED", `stored workstream state is invalid: ${state}`);
  }
  return {
    workstreamId: parseIdentifier<"WorkstreamId">(text(value, "workstream_id"), "workFacts.workstreamId"),
    deliveryRunId: parseIdentifier<"DeliveryRunId">(text(value, "delivery_run_id"), "workFacts.deliveryRunId"),
    workstreamRevision: positive(integer(value, "revision"), "workFacts.workstreamRevision"),
    state: state as "active" | "complete" | "cancelled" | "degraded" | "abandoned",
  };
}

function plan(database: Database.Database, runId: string): WorkWorkflowFacts["plan"] {
  const value = currentRunPlanBinding(database, runId);
  return value === null
    ? { observation: "Unobserved" }
    : { observation: "Observed", planRevision: positive(value.planRevision, "workFacts.planRevision") };
}

function checks(database: Database.Database, runId: string, taskId: string) {
  const values = database.prepare(`
    SELECT check_id, status FROM task_objective_checks
     WHERE run_id=? AND task_id=? ORDER BY check_id LIMIT ${String(MAX_CHECKS + 1)}
  `).all(runId, taskId);
  bounded(values, MAX_CHECKS, "objective checks");
  return values.map((value) => {
    const stored = row(value, "work objective check");
    const state = text(stored, "status");
    if (state !== "pending" && state !== "pass" && state !== "fail") {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", `stored objective-check state is invalid: ${state}`);
    }
    return {
      checkId: parseIdentifier<"CheckId">(text(stored, "check_id"), "workFacts.checkId"),
      state: state as "pending" | "pass" | "fail",
    };
  });
}

function barriers(database: Database.Database, runId: string, taskId: string): WorkWorkflowFacts["barriers"]["items"] {
  const values = database.prepare(`
    SELECT CASE WHEN scope='run' THEN 'run' ELSE 'stage' END AS kind,
           run_id||':'||scope||':'||stage_id AS barrier_id, stage_id, state, NULL AS request_id
      FROM barriers WHERE run_id=?
    UNION ALL
    SELECT 'task-request', barrier.barrier_id, '', barrier.state, request.request_id
      FROM task_request_barriers barrier JOIN task_requests request USING(request_id)
     WHERE request.run_id=? AND request.task_id=?
    ORDER BY kind, barrier_id LIMIT ${String(MAX_BARRIERS + 1)}
  `).all(runId, runId, taskId);
  bounded(values, MAX_BARRIERS, "barriers");
  return values.map<WorkWorkflowFacts["barriers"]["items"][number]>((value) => {
    const stored = row(value, "work barrier");
    const kind = text(stored, "kind");
    const barrierId = text(stored, "barrier_id");
    const state = text(stored, "state");
    if (kind === "run" && state === "closed") return { kind, barrierId, state };
    if (kind === "stage" && state === "closed") {
      return { kind, barrierId, stageId: text(stored, "stage_id"), state };
    }
    if (kind === "task-request" && (state === "blocked" || state === "released" || state === "abandoned")) {
      return {
        kind,
        barrierId,
        requestId: parseIdentifier<"RequestId">(text(stored, "request_id"), "workFacts.requestId"),
        state,
      };
    }
    throw new ProjectFabricCoreError("RECOVERY_REQUIRED", `stored barrier state is invalid: ${kind}/${state}`);
  }).sort((left, right) => compareText(
    `${left.kind}:${left.barrierId}`,
    `${right.kind}:${right.barrierId}`,
  ));
}

function writeLeases(
  database: Database.Database,
  runId: string,
  taskId: string,
): WorkWorkflowFacts["declaredWriteScopes"]["leases"] {
  const values = database.prepare(`
    SELECT binding.obligation_id AS lease_id, binding.state AS binding_state,
           lease.run_id, lease.kind, lease.generation, lease.status
      FROM task_obligation_bindings binding
      LEFT JOIN leases lease ON lease.lease_id=binding.obligation_id
     WHERE binding.coordination_run_id=? AND binding.task_id=?
       AND binding.obligation_kind='write-lease'
     ORDER BY binding.obligation_id LIMIT ${String(MAX_WRITE_LEASES + 1)}
  `).all(runId, taskId);
  bounded(values, MAX_WRITE_LEASES, "task-bound write leases");
  return values.map((value) => {
    const stored = row(value, "task-bound write lease");
    const leaseId = parseIdentifier<"LeaseId">(text(stored, "lease_id"), "workFacts.leaseId");
    const bindingState = text(stored, "binding_state");
    if (stored.run_id === null || stored.kind === null || stored.generation === null || stored.status === null) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", `task-bound write lease is dangling: ${leaseId}`);
    }
    const state = text(stored, "status");
    if (!["active", "reconciled", "abandoned"].includes(bindingState) ||
        text(stored, "run_id") !== runId || text(stored, "kind") !== "write" ||
        !["active", "quarantined", "released"].includes(state)) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", `task-bound write lease is contradictory: ${leaseId}`);
    }
    const paths = database.prepare(`
      SELECT canonical_path FROM write_scope_entries
       WHERE lease_id=? ORDER BY canonical_path LIMIT ${String(MAX_WRITE_PATHS + 1)}
    `).all(leaseId);
    bounded(paths, MAX_WRITE_PATHS, `write lease ${leaseId} paths`);
    return {
      leaseId,
      generation: positive(integer(stored, "generation"), "workFacts.writeLease.generation"),
      state: state as "active" | "quarantined" | "released",
      paths: paths
        .map((path) => text(row(path, "write scope path"), "canonical_path"))
        .sort(compareText),
    };
  });
}

function taskState(value: string): WorkTaskState {
  if (!["blocked", "ready", "active", "complete", "cancelled", "degraded"].includes(value)) {
    throw new ProjectFabricCoreError("RECOVERY_REQUIRED", `stored task state is invalid: ${value}`);
  }
  return value as WorkTaskState;
}

function bounded(values: readonly unknown[], maximum: number, label: string): void {
  if (values.length > maximum) {
    throw new ProjectFabricCoreError("RESOURCE_EXHAUSTED", `work facts exceed ${String(maximum)} ${label}`);
  }
}

function positive(value: number, path: string): number {
  if (value < 1) throw new ProjectFabricCoreError("RECOVERY_REQUIRED", `${path} must be positive`);
  return value;
}

function compareText(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}
