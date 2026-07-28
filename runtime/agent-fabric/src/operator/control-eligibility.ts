import type Database from "better-sqlite3";

import { integer, isRow, text } from "../persistence/row-codec.js";

export type ResolvedControlTarget = {
  scopeKind: "task" | "subtree" | "run" | "session";
  revision: number;
  projectSessionId: string;
  sessionGeneration: number;
  runs: readonly string[];
  tasks: readonly {
    runId: string;
    taskId: string;
    revision: number;
    state: string;
    ownerAgentId: string | null;
    ownerLeaseGeneration: number;
  }[];
  agents: readonly { runId: string; agentId: string; lifecycle: string }[];
};

export type ActiveTurn = {
  runId: string;
  agentId: string;
  actionId: string;
  adapterId: string;
  providerSessionGeneration: number;
  turnLeaseGeneration: number;
  sourcePayloadHash: string;
  turnId: string | null;
};

export type ControlEligibility = {
  lifecycleState: "active" | "paused" | "terminal";
  eligibleActions: readonly ("pause" | "resume" | "cancel" | "steer")[];
};

type Row = Record<string, unknown>;

function record(value: unknown): value is Row {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function readControlEligibility(
  database: Database.Database,
  target: ResolvedControlTarget,
  activeTurns: readonly ActiveTurn[] = readControlActiveTurns(database, target),
): ControlEligibility {
  const tasksPaused = target.tasks.length > 0 && target.tasks.every((task) => isRow(database.prepare(`
    SELECT 1 FROM operator_control_fences
     WHERE coordination_run_id=? AND task_id=? AND state='paused'
  `).get(task.runId, task.taskId)));
  const agentsPaused = target.tasks.length === 0 && target.agents.length > 0 && target.agents.every((agent) => {
    const freeze = database.prepare(`
      SELECT reason FROM delivery_freezes WHERE run_id=? AND agent_id=?
    `).get(agent.runId, agent.agentId);
    return isRow(freeze) && typeof freeze.reason === "string" && freeze.reason.startsWith("operator-pause:");
  });
  const paused = tasksPaused || agentsPaused;
  const terminal = target.tasks.length > 0 && target.tasks.every((task) =>
    ["complete", "cancelled", "degraded"].includes(task.state));
  if (terminal) return { lifecycleState: "terminal", eligibleActions: [] };
  if (paused) return { lifecycleState: "paused", eligibleActions: ["resume", "cancel"] };
  return activeTurns.length > 0
    ? { lifecycleState: "active", eligibleActions: ["pause", "cancel", "steer"] }
    : { lifecycleState: "active", eligibleActions: ["pause", "cancel"] };
}

export function readControlActiveTurns(
  database: Database.Database,
  target: ResolvedControlTarget,
): readonly ActiveTurn[] {
  const turns: ActiveTurn[] = [];
  for (const agent of target.agents) {
    const value = database.prepare(`
      SELECT lease.action_id, lease.provider_session_generation, lease.turn_lease_generation,
             action.adapter_id, action.payload_hash, action.payload_json, action.result_json
        FROM provider_session_turn_leases lease
        JOIN provider_actions action
          ON action.run_id=lease.run_id
         AND action.adapter_id=lease.adapter_id
         AND action.action_id=lease.action_id
       WHERE lease.run_id=? AND lease.agent_id=? AND lease.status='active'
    `).get(agent.runId, agent.agentId);
    if (!isRow(value)) continue;
    const payload: unknown = JSON.parse(text(value, "payload_json"));
    const sourceResult: unknown = value.result_json === null
      ? null
      : JSON.parse(text(value, "result_json"));
    const attributedTaskId = record(payload) && typeof payload.taskId === "string" ? payload.taskId : null;
    if (
      (target.scopeKind === "task" || target.scopeKind === "subtree") &&
      (attributedTaskId === null || !target.tasks.some((task) =>
        task.runId === agent.runId && task.taskId === attributedTaskId))
    ) continue;
    turns.push({
      runId: agent.runId,
      agentId: agent.agentId,
      actionId: text(value, "action_id"),
      adapterId: text(value, "adapter_id"),
      providerSessionGeneration: integer(value, "provider_session_generation"),
      turnLeaseGeneration: integer(value, "turn_lease_generation"),
      sourcePayloadHash: text(value, "payload_hash"),
      turnId: record(sourceResult) && typeof sourceResult.turnId === "string" ? sourceResult.turnId : null,
    });
  }
  return turns;
}
