import {
  parseIdentifier,
  parseTimestamp,
  type RunIdentity,
  type RunWorkstreamIdentity,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError } from "../project-session/contracts.js";
import { currentAcceptedRunScope, currentRunPlanBinding } from "../project-session/run-plan-store.js";
import { integer, isRow, row, text, type Row } from "../persistence/row-codec.js";

export const MAX_RUN_IDENTITY_WORKSTREAMS = 1024;

/**
 * Project one coordination run's closed identity fact from Fabric-owned rows.
 * The caller owns the surrounding read transaction; this owner bounds its
 * nested group before materialising a result that the protocol cannot encode.
 */
export function projectRunIdentity(database: Database.Database, run: Row): RunIdentity {
  const runId = text(run, "run_id");
  const storedWorkstreams = database.prepare(`
    SELECT workstream_id, delivery_run_id, lead_agent_id, state, updated_at
      FROM workstreams WHERE coordination_run_id=?
     ORDER BY workstream_id
     LIMIT ${String(MAX_RUN_IDENTITY_WORKSTREAMS + 1)}
  `).all(runId);
  if (storedWorkstreams.length > MAX_RUN_IDENTITY_WORKSTREAMS) {
    throw new ProjectFabricCoreError(
      "RESOURCE_EXHAUSTED",
      `run identity exceeds ${String(MAX_RUN_IDENTITY_WORKSTREAMS)} workstreams`,
    );
  }
  const workstreams = storedWorkstreams.map((value): RunWorkstreamIdentity => {
    const workstream = row(value, "run workstream identity");
    const state = text(workstream, "state");
    if (
      state !== "active" && state !== "complete" && state !== "cancelled" &&
      state !== "degraded" && state !== "abandoned"
    ) {
      throw new ProjectFabricCoreError(
        "RECOVERY_REQUIRED",
        `stored workstream state is outside the closed contract: ${state}`,
      );
    }
    return {
      workstreamId: parseIdentifier<"WorkstreamId">(
        text(workstream, "workstream_id"),
        "runIdentity.workstreamId",
      ),
      deliveryRunId: parseIdentifier<"DeliveryRunId">(
        text(workstream, "delivery_run_id"),
        "runIdentity.deliveryRunId",
      ),
      leadAgentId: parseIdentifier<"AgentId">(
        text(workstream, "lead_agent_id"),
        "runIdentity.leadAgentId",
      ),
      state,
      updatedAt: parseTimestamp(
        new Date(integer(workstream, "updated_at")).toISOString(),
        "runIdentity.workstreamUpdatedAt",
      ),
    };
  });
  const lastEvent = database.prepare(`
    SELECT MAX(created_at) AS last_event_at FROM events WHERE run_id=?
  `).get(runId);
  const lastEventAt = isRow(lastEvent) &&
      typeof lastEvent.last_event_at === "number" &&
      Number.isSafeInteger(lastEvent.last_event_at)
    ? parseTimestamp(new Date(lastEvent.last_event_at).toISOString(), "runIdentity.lastEventAt")
    : null;
  const plan = currentRunPlanBinding(database, runId);
  const acceptedScopeRef = plan?.acceptedScopeRef ??
    currentAcceptedRunScope(database, runId)?.artifactRef ?? null;
  return {
    runKind: "coordination",
    chairAgentId: parseIdentifier<"AgentId">(text(run, "chair_agent_id"), "runIdentity.chairAgentId"),
    acceptedScopeRef,
    currentPlanRef: plan?.currentPlanRef ?? null,
    planRevision: plan?.planRevision ?? null,
    workstreams,
    lastEventAt,
  };
}
