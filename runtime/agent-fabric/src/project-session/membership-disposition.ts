import type Database from "better-sqlite3";

import { ProjectFabricCoreError } from "./contracts.js";
import { integer, nullableText, row, text } from "../persistence/row-codec.js";

export type MembershipSourceKind =
  | "coordination-run"
  | "workstream"
  | "task"
  | "lease"
  | "provider-action"
  | "required-message"
  | "artifact-obligation"
  | "gate"
  | "scoped-barrier";

export type MembershipSourceDisposition =
  | { state: "active" }
  | { state: "reconciled" }
  | { state: "abandoned"; reason: string };

function active(): MembershipSourceDisposition {
  return { state: "active" };
}

function reconciled(): MembershipSourceDisposition {
  return { state: "reconciled" };
}

function abandoned(reason: string): MembershipSourceDisposition {
  return { state: "abandoned", reason };
}

function exactlyOne(values: readonly unknown[], label: string): Record<string, unknown> {
  if (values.length === 0) throw new ProjectFabricCoreError("NOT_FOUND", `${label} was not found`);
  if (values.length !== 1) throw new ProjectFabricCoreError("CONFLICT", `${label} is ambiguous across source owners`);
  return row(values[0], label);
}

export function membershipSourceDisposition(
  database: Database.Database,
  projectSessionId: string,
  runId: string,
  kind: MembershipSourceKind,
  memberId: string,
  memberAdapterId = "",
): MembershipSourceDisposition {
  switch (kind) {
    case "coordination-run": {
      const source = row(database.prepare(`
        SELECT lifecycle_state FROM runs WHERE project_session_id=? AND run_id=?
      `).get(projectSessionId, memberId), "coordination-run membership source");
      const state = text(source, "lifecycle_state");
      if (state === "awaiting_acceptance" || state === "closed") return reconciled();
      if (state === "cancelled" || state === "launch_failed") {
        return abandoned(`coordination-run source state ${state}`);
      }
      return active();
    }
    case "workstream": {
      const source = row(database.prepare(`
        SELECT state FROM workstreams
         WHERE project_session_id=? AND coordination_run_id=? AND workstream_id=?
      `).get(projectSessionId, runId, memberId), "workstream membership source");
      const state = text(source, "state");
      if (state === "complete") return reconciled();
      if (state === "cancelled" || state === "degraded" || state === "abandoned") {
        return abandoned(`workstream source state ${state}`);
      }
      return active();
    }
    case "task": {
      const source = row(database.prepare("SELECT state FROM tasks WHERE run_id=? AND task_id=?")
        .get(runId, memberId), "task membership source");
      const state = text(source, "state");
      if (state === "complete") return reconciled();
      if (state === "cancelled" || state === "degraded") return abandoned(`task source state ${state}`);
      return active();
    }
    case "lease": {
      const sources = database.prepare(`
        SELECT status, 'write' AS source_kind, NULL AS run_state, NULL AS current_lease_id
          FROM leases WHERE run_id=? AND lease_id=?
        UNION ALL
        SELECT lease.status, 'chair' AS source_kind, run.lifecycle_state AS run_state,
               run.chair_lease_id AS current_lease_id
          FROM run_chair_leases lease JOIN runs run ON run.run_id=lease.run_id
         WHERE lease.project_session_id=? AND lease.run_id=? AND lease.lease_id=?
        UNION ALL
        SELECT status, 'task-owner' AS source_kind, NULL AS run_state, NULL AS current_lease_id
          FROM task_owner_leases
         WHERE project_session_id=? AND run_id=? AND lease_id=?
      `).all(
        runId,
        memberId,
        projectSessionId,
        runId,
        memberId,
        projectSessionId,
        runId,
        memberId,
      );
      const source = exactlyOne(sources, "lease membership source");
      const status = text(source, "status");
      const owner = text(source, "source_kind");
      if (status === "released") return reconciled();
      if (
        status === "revoked" && owner === "chair" &&
        nullableText(source, "run_state") === "closed" &&
        nullableText(source, "current_lease_id") === memberId
      ) return reconciled();
      if (status === "revoked") return abandoned(`${owner} lease source status revoked`);
      return active();
    }
    case "provider-action": {
      if (memberAdapterId.length === 0) {
        throw new ProjectFabricCoreError("NOT_FOUND", "provider-action membership adapter was not found");
      }
      const source = row(database.prepare(
        "SELECT status FROM provider_actions WHERE run_id=? AND adapter_id=? AND action_id=?",
      ).get(runId, memberAdapterId, memberId), "provider-action membership source");
      return text(source, "status") === "terminal" ? reconciled() : active();
    }
    case "required-message": {
      const message = row(database.prepare(`
        SELECT requires_ack FROM messages WHERE run_id=? AND message_id=?
      `).get(runId, memberId), "required-message membership source");
      if (integer(message, "requires_ack") !== 1) {
        throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "message is not a required-message source");
      }
      const source = row(database.prepare(`
        SELECT COUNT(*) AS total,
               SUM(CASE WHEN state NOT IN ('acknowledged','abandoned','expired') THEN 1 ELSE 0 END) AS unresolved,
               SUM(CASE WHEN state IN ('abandoned','expired') THEN 1 ELSE 0 END) AS abandoned
          FROM deliveries WHERE run_id=? AND message_id=?
      `).get(runId, memberId), "required-message delivery source");
      const total = integer(source, "total");
      if (total === 0) return active();
      const unresolved = source.unresolved === null ? total : integer(source, "unresolved");
      const abandonedCount = source.abandoned === null ? 0 : integer(source, "abandoned");
      if (unresolved !== 0) return active();
      if (abandonedCount > 0) return abandoned("required-message source delivery expired or abandoned");
      return reconciled();
    }
    case "artifact-obligation": {
      const source = row(database.prepare(`
        SELECT registry_state FROM artifacts WHERE run_id=? AND artifact_id=?
      `).get(runId, memberId), "artifact membership source");
      return text(source, "registry_state") === "active" ? reconciled() : active();
    }
    case "gate": {
      const source = row(database.prepare(`
        SELECT status, resolution_json FROM scoped_gates
         WHERE project_session_id=? AND coordination_run_id=? AND gate_id=?
      `).get(projectSessionId, runId, memberId), "gate membership source");
      const status = text(source, "status");
      if (status === "approved" || status === "rejected") return reconciled();
      if (status === "cancelled") return abandoned("gate source status cancelled");
      if (status === "superseded") {
        const resolution = nullableText(source, "resolution_json");
        if (resolution !== null) {
          const parsed: unknown = JSON.parse(resolution);
          if (
            typeof parsed === "object" && parsed !== null && !Array.isArray(parsed) &&
            Reflect.get(parsed, "kind") !== "system-supersession"
          ) return reconciled();
        }
        return abandoned("gate source status superseded by acceptance-cycle exit");
      }
      return active();
    }
    case "scoped-barrier": {
      const sources = database.prepare(`
        SELECT barrier.state AS state, NULL AS gate_status, NULL AS resolution_json
          FROM task_request_barriers barrier
          JOIN task_requests request ON request.request_id=barrier.request_id
         WHERE request.project_session_id=? AND request.run_id=? AND barrier.barrier_id=?
        UNION ALL
        SELECT NULL AS state, gate.status AS gate_status, gate.resolution_json AS resolution_json
          FROM scoped_gate_barriers barrier
          JOIN scoped_gates gate ON gate.gate_id=barrier.gate_id
         WHERE gate.project_session_id=? AND gate.coordination_run_id=? AND barrier.barrier_id=?
      `).all(projectSessionId, runId, memberId, projectSessionId, runId, memberId);
      if (sources.length === 0) {
        throw new ProjectFabricCoreError("NOT_FOUND", "scoped-barrier membership source was not found");
      }
      const states = sources.map((source) => {
        const value = row(source, "scoped-barrier membership source");
        return {
          state: nullableText(value, "state"),
          gateStatus: nullableText(value, "gate_status"),
          gateResolution: nullableText(value, "resolution_json"),
        };
      });
      if (states.some(({ state, gateStatus }) =>
        (state !== null && state !== "released" && state !== "abandoned") ||
        gateStatus === "pending" || gateStatus === "deferred")) return active();
      const abandonedState = states.find(({ state, gateStatus, gateResolution }) => {
        if (state === "abandoned" || gateStatus === "cancelled") return true;
        if (gateStatus !== "superseded") return false;
        if (gateResolution === null) return true;
        const parsed: unknown = JSON.parse(gateResolution);
        return typeof parsed === "object" && parsed !== null && !Array.isArray(parsed) &&
          Reflect.get(parsed, "kind") === "system-supersession";
      });
      if (abandonedState !== undefined) {
        return abandoned(
          abandonedState.state === "abandoned"
            ? "scoped barrier source state abandoned"
            : `scoped barrier gate source status ${String(abandonedState.gateStatus)}`,
        );
      }
      return reconciled();
    }
  }
}
