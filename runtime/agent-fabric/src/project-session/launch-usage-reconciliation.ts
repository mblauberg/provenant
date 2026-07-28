import { parseLaunchAdapterOutcomeV1 } from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError } from "./contracts.js";
import { canonicalJson, integer, isRow, row, sha256, text } from "../persistence/row-codec.js";

const DIGEST = /^sha256:[0-9a-f]{64}$/u;
const UNIT_KEY = /^[a-z][a-z0-9_.:-]{0,63}$/u;

export type LaunchUsageReconciliationInput = Readonly<{
  projectId: string;
  projectSessionId: string;
  coordinationRunId: string;
  providerAdapterId: string;
  providerActionId: string;
  reservationId: string;
  expectedReservationRevision: number;
  observedUsage: Readonly<Record<string, number>>;
  evidenceDigest: string;
}>;

export type LaunchUsageReconciliationResult = Readonly<{
  reservationId: string;
  revision: number;
  reconciledUsage: Readonly<Record<string, number>>;
}>;

function protocol(message: string): never {
  throw new ProjectFabricCoreError("PROTOCOL_INVALID", message);
}

function stale(message: string): never {
  throw new ProjectFabricCoreError("STALE_REVISION", message);
}

function exactDigest(value: string, label: string): string {
  if (!DIGEST.test(value)) protocol(`${label} must be an exact sha256 digest`);
  return value;
}

function resourceAmounts(value: Readonly<Record<string, number>>, label: string): Readonly<Record<string, number>> {
  if (Object.keys(value).length === 0) protocol(`${label} must not be empty`);
  const result: Record<string, number> = {};
  for (const [unit, amount] of Object.entries(value).sort(([left], [right]) => left.localeCompare(right))) {
    if (!UNIT_KEY.test(unit)) protocol(`${label}.${unit} has an invalid unit key`);
    if (!Number.isSafeInteger(amount) || amount < 0) {
      protocol(`${label}.${unit} must be a non-negative safe integer`);
    }
    result[unit] = amount;
  }
  return result;
}

export function reconcileUnknownLaunchUsage(
  database: Database.Database,
  clock: () => number,
  input: LaunchUsageReconciliationInput,
): LaunchUsageReconciliationResult {
  const evidenceDigest = exactDigest(input.evidenceDigest, "launchUsageReconciliation.evidenceDigest");
  if (!Number.isSafeInteger(input.expectedReservationRevision) || input.expectedReservationRevision < 1) {
    protocol("launchUsageReconciliation.expectedReservationRevision must be positive");
  }
  const observedUsage = resourceAmounts(input.observedUsage, "launchUsageReconciliation.observedUsage");
  const eventId = `launch-usage-reconciliation:${sha256(canonicalJson({
    ...input,
    evidenceDigest,
    observedUsage,
  }))}`;
  return database.transaction(() => {
    const replay = database.prepare(`
      SELECT payload_json FROM events WHERE event_id=?
    `).get(eventId);
    if (isRow(replay)) {
      const payload: unknown = JSON.parse(text(replay, "payload_json"));
      if (!isRow(payload) || payload.reservationId !== input.reservationId ||
          payload.evidenceDigest !== evidenceDigest || !isRow(payload.reconciledUsage) ||
          !Number.isSafeInteger(payload.revision)) {
        throw new Error("stored launch usage reconciliation is invalid");
      }
      return {
        reservationId: input.reservationId,
        revision: Number(payload.revision),
        reconciledUsage: payload.reconciledUsage as Record<string, number>,
      };
    }
    const binding = row(database.prepare(`
      SELECT reservation.revision, reservation.state, reservation.amounts_json,
             action.status, action.effect_count, action.result_json
        FROM project_session_launch_custody custody
        JOIN project_sessions session
          ON session.project_session_id=custody.project_session_id
        JOIN resource_reservations reservation
          ON reservation.reservation_id=custody.reservation_id
        JOIN provider_actions action
          ON action.adapter_id=custody.provider_adapter_id
         AND action.action_id=custody.provider_action_id
       WHERE session.project_id=?
         AND custody.project_session_id=?
         AND custody.coordination_run_id=?
         AND custody.provider_adapter_id=?
         AND custody.provider_action_id=?
         AND custody.reservation_id=?
    `).get(
      input.projectId,
      input.projectSessionId,
      input.coordinationRunId,
      input.providerAdapterId,
      input.providerActionId,
      input.reservationId,
    ), "launch usage reconciliation binding");
    if (integer(binding, "revision") !== input.expectedReservationRevision) {
      stale("launch usage reservation revision changed");
    }
    if (
      text(binding, "state") !== "reconciled" ||
      text(binding, "status") !== "terminal" ||
      integer(binding, "effect_count") !== 1
    ) {
      throw new ProjectFabricCoreError(
        "LIFECYCLE_PRECONDITION_FAILED",
        "launch usage requires an exact terminal successful action",
      );
    }
    const result = parseLaunchAdapterOutcomeV1(JSON.parse(text(binding, "result_json")));
    if (result.outcome.kind !== "terminal-success") {
      throw new ProjectFabricCoreError(
        "LIFECYCLE_PRECONDITION_FAILED",
        "launch usage requires terminal-success evidence",
      );
    }
    const amounts: unknown = JSON.parse(text(binding, "amounts_json"));
    if (!isRow(amounts)) throw new Error("stored launch reservation amounts are invalid");
    const unknownUnits = Object.entries(result.outcome.resourceUsage)
      .filter(([, value]) => value === "unknown")
      .map(([unit]) => unit)
      .sort();
    if (Object.keys(observedUsage).some((unit) => !unknownUnits.includes(unit))) {
      protocol("launch usage reconciliation may cover only unknown dimensions");
    }
    for (const [unit, value] of Object.entries(observedUsage)) {
      const maximum = amounts[unit];
      if (typeof maximum !== "number" || value > maximum) {
        protocol(`launch usage reconciliation ${unit} exceeds its reservation`);
      }
    }
    const dimensions = database.prepare(`
      SELECT scope_id, unit_key, amount, usage_unknown
        FROM resource_reservation_dimensions
       WHERE reservation_id=? ORDER BY scope_id, unit_key
    `).all(input.reservationId).map((value) => row(value, "launch usage reservation dimension"));
    for (const dimension of dimensions) {
      const unit = text(dimension, "unit_key");
      if (integer(dimension, "usage_unknown") !== 1) continue;
      const consumed = observedUsage[unit];
      if (consumed === undefined) continue;
      const amount = integer(dimension, "amount");
      database.prepare(`
        UPDATE resource_reservation_dimensions
           SET consumed=?, released=?, usage_unknown=0
         WHERE reservation_id=? AND scope_id=? AND unit_key=? AND usage_unknown=1
      `).run(consumed, amount - consumed, input.reservationId, text(dimension, "scope_id"), unit);
      const changed = database.prepare(`
        UPDATE resource_dimensions
           SET used=used+?,
               usage_unknown=CASE WHEN EXISTS (
                 SELECT 1 FROM resource_reservation_dimensions other
                  WHERE other.scope_id=resource_dimensions.scope_id
                    AND other.unit_key=resource_dimensions.unit_key
                    AND other.usage_unknown=1
               ) THEN 1 ELSE 0 END
         WHERE scope_id=? AND unit_key=? AND used+?<=limit_value
      `).run(consumed, text(dimension, "scope_id"), unit, consumed);
      if (changed.changes !== 1) {
        throw new ProjectFabricCoreError("RESOURCE_EXHAUSTED", `${unit} reconciliation exceeds its ancestor`);
      }
      database.prepare(`
        UPDATE resource_scopes
           SET state=CASE WHEN EXISTS (
             SELECT 1 FROM resource_dimensions current
              WHERE current.scope_id=resource_scopes.scope_id AND current.usage_unknown=1
           ) THEN 'usage-unknown' ELSE 'active' END,
               revision=revision+1
         WHERE scope_id=?
      `).run(text(dimension, "scope_id"));
    }
    const changed = database.prepare(`
      UPDATE resource_reservations SET revision=revision+1, updated_at=?
       WHERE reservation_id=? AND revision=? AND state='reconciled'
    `).run(clock(), input.reservationId, input.expectedReservationRevision);
    if (changed.changes !== 1) stale("launch usage reconciliation raced another writer");
    const resultValue = {
      reservationId: input.reservationId,
      revision: input.expectedReservationRevision + 1,
      reconciledUsage: observedUsage,
    };
    database.prepare(`
      INSERT INTO events(event_id, run_id, type, actor_agent_id, payload_json, created_at)
      VALUES (?, ?, 'launch-usage-reconciled', NULL, ?, ?)
    `).run(
      eventId,
      input.coordinationRunId,
      canonicalJson({ ...resultValue, evidenceDigest }),
      clock(),
    );
    return resultValue;
  }).immediate();
}
