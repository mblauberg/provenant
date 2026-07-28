import type Database from "better-sqlite3";

import { ProjectFabricCoreError } from "../project-session/contracts.js";
import { canonicalJson, sha256 } from "../persistence/row-codec.js";

export const PROVIDER_ACTION_OWNERS = [
  "generic",
  "launch",
  "provider_agent",
  "lifecycle",
  "herdr",
  "chair_recovery",
  "chair_live_handoff",
  "operator_control",
  "certifying_review",
  "integrity_failed",
] as const;

export type ProviderActionOwner = typeof PROVIDER_ACTION_OWNERS[number];

export type ProviderActionOwnerRef = Readonly<{
  runId: string;
  adapterId: string;
  actionId: string;
}>;

export type ProviderActionOwnerStartupRow = Readonly<{
  rowId: string;
  action: Record<string, unknown>;
  diagnosticRef: ProviderActionOwnerRef;
  ref: ProviderActionOwnerRef | null;
  owner: ProviderActionOwner;
  reason: "owner_integrity_failed" | "owner_classification_failed";
}>;

export type ProviderActionCustodyOwner = Exclude<ProviderActionOwner, "integrity_failed">;

export class ProviderActionOwnerError extends ProjectFabricCoreError {
  readonly expectedOwner: ProviderActionCustodyOwner;
  readonly actualOwner: ProviderActionOwner;

  constructor(
    expectedOwner: ProviderActionCustodyOwner,
    actualOwner: ProviderActionOwner,
    ref: ProviderActionOwnerRef,
  ) {
    super("CAPABILITY_FORBIDDEN", `provider action owner integrity check failed for ${ref.adapterId}/${ref.actionId}: expected ${expectedOwner}, got ${actualOwner}`, {
      expectedOwner,
      actualOwner,
      runId: ref.runId,
      adapterId: ref.adapterId,
      actionId: ref.actionId,
    });
    this.name = "ProviderActionOwnerError";
    this.expectedOwner = expectedOwner;
    this.actualOwner = actualOwner;
  }
}

function count(database: Database.Database, sql: string, ...parameters: unknown[]): number {
  const value = database.prepare(sql).pluck().get(...parameters);
  return typeof value === "number" && Number.isSafeInteger(value) ? value : -1;
}

function row(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Record<string, unknown>
    : undefined;
}

/** Canonical persisted-identity owner classifier for provider actions. */
export function classifyProviderActionOwner(
  database: Database.Database,
  ref: ProviderActionOwnerRef,
): ProviderActionOwner {
  const action = row(database.prepare(`
    SELECT run_id,operation,target_agent_id,provider_session_generation,
           turn_lease_generation,finding_capacity_reservation_digest
      FROM provider_actions WHERE adapter_id=? AND action_id=?
  `).get(ref.adapterId, ref.actionId));
  if (action === undefined || action.run_id !== ref.runId) return "integrity_failed";

  const launch = count(database, `
    SELECT COUNT(*) FROM project_session_launch_custody
     WHERE coordination_run_id=? AND provider_adapter_id=? AND provider_action_id=?
  `, ref.runId, ref.adapterId, ref.actionId);
  const providerAgent = count(database, `
    SELECT COUNT(*) FROM provider_agent_custody
     WHERE run_id=? AND adapter_id=? AND action_id=?
  `, ref.runId, ref.adapterId, ref.actionId);
  const lifecycle = count(database, `
    SELECT COUNT(*) FROM lifecycle_rotation_custodies
     WHERE run_id=? AND provider_action_adapter_id=? AND provider_action_id=?
  `, ref.runId, ref.adapterId, ref.actionId);
  const validLifecycleProviderAgent = count(database, `
    SELECT COUNT(*)
      FROM lifecycle_rotation_custodies lifecycle
      JOIN provider_agent_custody agent
        ON agent.run_id=lifecycle.run_id
       AND agent.adapter_id=lifecycle.provider_action_adapter_id
       AND agent.action_id=lifecycle.provider_action_id
     WHERE lifecycle.run_id=?
       AND lifecycle.provider_action_adapter_id=?
       AND lifecycle.provider_action_id=?
       AND agent.operation='spawn'
       AND agent.actor_agent_id=lifecycle.agent_id
       AND agent.target_agent_id=lifecycle.agent_id
       AND agent.bridge_contract_digest=lifecycle.replacement_contract_digest
       AND agent.capability_hash=lifecycle.staged_capability_hash
       AND agent.principal_generation=lifecycle.target_principal_generation
       AND agent.requested_provider_session_ref IS NULL
  `, ref.runId, ref.adapterId, ref.actionId);
  const chairRecovery = count(database, `
    SELECT COUNT(*)
      FROM chair_bridge_recovery_custody custody
      JOIN chair_bridge_losses loss ON loss.loss_id=custody.loss_id
     WHERE loss.coordination_run_id=? AND custody.path='rebind'
       AND custody.provider_adapter_id=? AND custody.provider_action_id=?
  `, ref.runId, ref.adapterId, ref.actionId);
  const chairLiveHandoff = count(database, `
    SELECT COUNT(*) FROM chair_live_handoff_custody
     WHERE coordination_run_id=? AND provider_adapter_id=? AND promotion_action_id=?
  `, ref.runId, ref.adapterId, ref.actionId);
  const operatorBindings = count(database, `
    SELECT COUNT(*) FROM operator_control_provider_action_bindings
     WHERE run_id=? AND adapter_id=? AND action_id=?
  `, ref.runId, ref.adapterId, ref.actionId);
  const operatorCandidates = database.prepare(`
    SELECT custody.operator_id,custody.project_id,custody.project_session_id,
           custody.intent_digest,lease.run_id,lease.agent_id,lease.adapter_id,
           lease.action_id AS source_action_id,lease.provider_session_generation,
           lease.turn_lease_generation,json_extract(source.result_json,'$.turnId') AS turn_id
      FROM operator_effect_custody custody
      JOIN runs run ON run.project_session_id=custody.project_session_id
      JOIN provider_session_turn_leases lease ON lease.run_id=run.run_id
      JOIN provider_actions source
        ON source.run_id=lease.run_id AND source.adapter_id=lease.adapter_id
       AND source.action_id=lease.action_id
     WHERE json_extract(custody.intent_json,'$.kind')='control'
       AND lease.adapter_id=? AND lease.agent_id IS ?
       AND lease.provider_session_generation IS ?
       AND lease.turn_lease_generation IS ?
       AND ((?='interrupt' AND custody.operation IN ('pause','cancel')) OR
            (?='steer' AND custody.operation='steer'))
  `).all(
    ref.adapterId,
    action.target_agent_id,
    action.provider_session_generation,
    action.turn_lease_generation,
    action.operation,
    action.operation,
  ).map(row);
  if (operatorCandidates.some((candidate) => candidate === undefined)) return "integrity_failed";
  const expectedOperatorBindings = operatorCandidates.filter((candidate) => {
    if (candidate === undefined || typeof candidate.turn_id !== "string") return false;
    const operation = action.operation;
    if (operation !== "interrupt" && operation !== "steer") return false;
    const actionId = `operator-${sha256(canonicalJson({
      schemaVersion: 1,
      operatorId: candidate.operator_id,
      projectId: candidate.project_id,
      projectSessionId: candidate.project_session_id,
      intentDigest: candidate.intent_digest,
      adapterId: candidate.adapter_id,
      runId: candidate.run_id,
      agentId: candidate.agent_id,
      providerSessionGeneration: candidate.provider_session_generation,
      sourceActionId: candidate.source_action_id,
      turnLeaseGeneration: candidate.turn_lease_generation,
      turnId: candidate.turn_id,
      operation,
    })).slice(0, 48)}`;
    return actionId === ref.actionId;
  }).length;
  const validOperatorBindings = count(database, `
    SELECT COUNT(*)
      FROM operator_control_provider_action_bindings binding
      JOIN operator_effect_custody custody ON custody.custody_id=binding.custody_id
      JOIN runs run
        ON run.run_id=binding.run_id
       AND run.project_session_id=custody.project_session_id
      JOIN provider_actions source
        ON source.run_id=binding.run_id
       AND source.adapter_id=binding.source_adapter_id
       AND source.action_id=binding.source_action_id
       AND source.payload_hash=binding.source_payload_hash
       AND json_extract(source.result_json,'$.turnId')=binding.turn_id
      JOIN provider_session_turn_leases lease
        ON lease.run_id=binding.run_id
       AND lease.agent_id=binding.target_agent_id
       AND lease.adapter_id=binding.source_adapter_id
       AND lease.action_id=binding.source_action_id
       AND lease.provider_session_generation=binding.provider_session_generation
       AND lease.turn_lease_generation=binding.turn_lease_generation
      JOIN agents agent
        ON agent.run_id=binding.run_id
       AND agent.agent_id=binding.target_agent_id
       AND agent.provider_session_ref=binding.provider_session_ref
     WHERE binding.run_id=? AND binding.adapter_id=? AND binding.action_id=?
       AND binding.operation=? AND binding.target_agent_id IS ?
       AND binding.provider_session_generation IS ?
       AND binding.turn_lease_generation IS ?
       AND json_extract(custody.intent_json,'$.kind')='control'
       AND json_extract(custody.intent_json,'$.action')=custody.operation
       AND (
         (binding.operation='interrupt' AND custody.operation IN ('pause','cancel')) OR
         (binding.operation='steer' AND custody.operation='steer')
       )
  `,
  ref.runId,
  ref.adapterId,
  ref.actionId,
  action.operation,
  action.target_agent_id,
  action.provider_session_generation,
  action.turn_lease_generation);
  if ([launch, providerAgent, lifecycle, validLifecycleProviderAgent, chairRecovery, chairLiveHandoff, operatorBindings, validOperatorBindings]
    .some((value) => value < 0 || value > 1)) return "integrity_failed";
  if (lifecycle === 1 && providerAgent === 1 && validLifecycleProviderAgent !== 1) return "integrity_failed";
  if (expectedOperatorBindings >= 1 && operatorBindings !== 1) return "integrity_failed";
  if (operatorBindings !== validOperatorBindings) return "integrity_failed";

  const routeRows = database.prepare(`
    SELECT certifying_review,target_generation,slot
      FROM provider_action_routes
     WHERE run_id=? AND adapter_id=? AND action_id=?
  `).all(ref.runId, ref.adapterId, ref.actionId).map(row);
  if (routeRows.some((route) => route === undefined) || routeRows.length > 1) return "integrity_failed";
  const route = routeRows[0];
  const reservationDigest = action.finding_capacity_reservation_digest;
  let certifyingReview = false;
  if (reservationDigest === null) {
    if (route?.certifying_review === 1) return "integrity_failed";
  } else if (typeof reservationDigest !== "string") {
    return "integrity_failed";
  } else {
    const reservations = database.prepare(`
      SELECT target_generation,slot
        FROM review_finding_capacity_reservations
       WHERE run_id=? AND adapter_id=? AND action_id=? AND reservation_digest=?
    `).all(ref.runId, ref.adapterId, ref.actionId, reservationDigest).map(row);
    const reservation = reservations[0];
    if (
      reservations.length !== 1 || reservation === undefined || route === undefined ||
      route.certifying_review !== 1 ||
      route.target_generation !== reservation.target_generation || route.slot !== reservation.slot
    ) return "integrity_failed";
    certifyingReview = true;
  }

  const herdr = ref.adapterId === "herdr-control-v1";
  const candidates: ProviderActionCustodyOwner[] = [];
  if (launch === 1) candidates.push("launch");
  if (lifecycle === 1) candidates.push("lifecycle");
  else if (providerAgent === 1) candidates.push("provider_agent");
  if (herdr) candidates.push("herdr");
  if (chairRecovery === 1) candidates.push("chair_recovery");
  if (chairLiveHandoff === 1) candidates.push("chair_live_handoff");
  if (operatorBindings === 1) candidates.push("operator_control");
  if (certifyingReview) candidates.push("certifying_review");
  if (candidates.length > 1) return "integrity_failed";
  return candidates[0] ?? "generic";
}

export function classifyProviderActionOwnerForStartup(
  database: Database.Database,
  ref: ProviderActionOwnerRef,
): Readonly<{ owner: ProviderActionOwner; reason: "owner_integrity_failed" | "owner_classification_failed" }> {
  try {
    return { owner: classifyProviderActionOwner(database, ref), reason: "owner_integrity_failed" };
  } catch {
    return { owner: "integrity_failed", reason: "owner_classification_failed" };
  }
}

function diagnosticIdentity(value: unknown, field: string): string {
  return typeof value === "string" ? value : `<invalid-${field}:${typeof value}>`;
}

export function classifyProviderActionOwnerRowForStartup(
  database: Database.Database,
  value: unknown,
): ProviderActionOwnerStartupRow {
  const action = row(value);
  if (action === undefined) throw new Error("startup provider action row is invalid");
  const rowId = action.provider_action_rowid;
  if (typeof rowId !== "string" || !/^-?[0-9]+$/u.test(rowId)) {
    throw new Error("startup provider action row locator is invalid");
  }
  const diagnosticRef = {
    runId: diagnosticIdentity(action.run_id, "run-id"),
    adapterId: diagnosticIdentity(action.adapter_id, "adapter-id"),
    actionId: diagnosticIdentity(action.action_id, "action-id"),
  };
  if (
    typeof action.run_id !== "string" ||
    typeof action.adapter_id !== "string" ||
    typeof action.action_id !== "string"
  ) {
    return {
      rowId, action, diagnosticRef, ref: null, owner: "integrity_failed",
      reason: "owner_classification_failed",
    };
  }
  const ref = { runId: action.run_id, adapterId: action.adapter_id, actionId: action.action_id };
  const classification = classifyProviderActionOwnerForStartup(database, ref);
  return { rowId, action, diagnosticRef, ref, ...classification };
}

export function quarantineProviderActionOwnerAtStartup(
  database: Database.Database,
  startupRow: Pick<ProviderActionOwnerStartupRow, "rowId">,
  now: number,
  appendEvent: () => void,
): boolean {
  return database.transaction(() => {
    const binding = row(database.prepare(`
      SELECT budget_authority_id,budget_state,budget_reservation_json FROM provider_actions
       WHERE rowid=CAST(? AS INTEGER) AND status IN ('prepared','dispatched','ambiguous')
    `).get(startupRow.rowId));
    if (binding === undefined) return false;
    let settlement: string | null = null;
    let reservationCorrupt = false;
    if (binding.budget_state === "reserved") {
      try {
        const reservation = row(JSON.parse(String(binding.budget_reservation_json)));
        if (reservation === undefined) reservationCorrupt = true;
        else settlement = canonicalJson(Object.fromEntries(
          Object.keys(reservation).sort().map((unit) => [unit, "unknown"]),
        ));
      } catch {
        reservationCorrupt = true;
      }
    }
    const tombstone = (): number => {
      const triggerNames = [
        "provider_actions_budget_binding_immutable",
        "provider_actions_budget_state_cas",
        "provider_actions_budget_settle",
        "authority_budget_provider_ledger_update",
      ];
      const triggers = database.prepare(`
        SELECT name,sql FROM sqlite_master WHERE type='trigger'
         AND name IN (${triggerNames.map(() => "?").join(",")}) ORDER BY name
      `).all(...triggerNames).map(row);
      if (triggers.length !== triggerNames.length || triggers.some(
        (trigger) => trigger === undefined || typeof trigger.name !== "string" || typeof trigger.sql !== "string",
      )) throw new Error("startup provider action budget tombstone triggers are unavailable");
      for (const trigger of triggers) {
        if (trigger === undefined || typeof trigger.name !== "string") continue;
        database.exec(`DROP TRIGGER "${trigger.name}"`);
      }
      try {
        const changes = database.prepare(`
          UPDATE provider_actions
             SET status='quarantined',history_json=CASE
                   WHEN json_valid(history_json) AND json_type(history_json)='array'
                   THEN json_insert(history_json,'$[#]','quarantined') ELSE '["quarantined"]' END,
                 idempotency_proven=0,result_json=NULL,journal_revision=journal_revision+1,updated_at=?,
                 task_id=NULL,budget_authority_id=NULL,budget_reservation_json=NULL,
                 budget_settlement_json=NULL,budget_state=NULL,budget_started_at=NULL
           WHERE rowid=CAST(? AS INTEGER) AND status IN ('prepared','dispatched','ambiguous')
        `).run(now, startupRow.rowId).changes;
        if (changes === 1 && typeof binding.budget_authority_id === "string") {
          database.prepare(`
            UPDATE authority_budget SET
              provider_reserved=(
                SELECT COALESCE(SUM(reservation.value),0) FROM provider_actions action
                JOIN json_each(CASE WHEN json_valid(action.budget_reservation_json)
                  THEN action.budget_reservation_json ELSE '{}' END) reservation
                  ON reservation.key=authority_budget.unit_key
                LEFT JOIN json_each(CASE WHEN json_valid(action.budget_settlement_json)
                  THEN action.budget_settlement_json ELSE '{}' END) settlement
                  ON settlement.key=reservation.key
                WHERE action.budget_authority_id=authority_budget.authority_id
                  AND (action.budget_state='reserved' OR
                    (action.budget_state='usage-unknown' AND settlement.type='text' AND settlement.value='unknown'))
              ),
              provider_consumed=(
                SELECT COALESCE(SUM(settlement.value),0) FROM provider_actions action
                JOIN json_each(CASE WHEN json_valid(action.budget_settlement_json)
                  THEN action.budget_settlement_json ELSE '{}' END) settlement
                  ON settlement.key=authority_budget.unit_key
                WHERE action.budget_authority_id=authority_budget.authority_id
                  AND action.budget_state IN ('settled','usage-unknown') AND settlement.type='integer'
              ),
              usage_unknown=1
             WHERE authority_id=?
          `).run(binding.budget_authority_id);
        }
        return changes;
      } finally {
        for (const trigger of triggers) {
          if (trigger === undefined || typeof trigger.sql !== "string") continue;
          database.exec(trigger.sql);
        }
      }
    };
    let changes: number;
    try {
      changes = reservationCorrupt ? tombstone() : database.prepare(`
          UPDATE provider_actions
             SET status='quarantined',history_json=CASE
                   WHEN json_valid(history_json) AND json_type(history_json)='array'
                   THEN json_insert(history_json,'$[#]','quarantined') ELSE '["quarantined"]' END,
                 idempotency_proven=0,result_json=NULL,journal_revision=journal_revision+1,updated_at=?,
                 budget_settlement_json=CASE WHEN budget_state='reserved' THEN ? ELSE budget_settlement_json END,
                 budget_state=CASE WHEN budget_state='reserved' THEN 'usage-unknown' ELSE budget_state END
           WHERE rowid=CAST(? AS INTEGER) AND status IN ('prepared','dispatched','ambiguous')
        `).run(now, settlement, startupRow.rowId).changes;
    } catch (error: unknown) {
      if (reservationCorrupt || binding.budget_state !== "reserved") throw error;
      changes = tombstone();
    }
    if (changes !== 1) return false;
    appendEvent();
    return true;
  }).immediate();
}

export function assertProviderActionOwner(
  database: Database.Database,
  ref: ProviderActionOwnerRef,
  expectedOwner: ProviderActionCustodyOwner,
): void {
  const actualOwner = classifyProviderActionOwner(database, ref);
  if (actualOwner !== expectedOwner) {
    throw new ProviderActionOwnerError(expectedOwner, actualOwner, ref);
  }
}
