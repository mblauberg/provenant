import type Database from "better-sqlite3";

import { ProjectFabricCoreError } from "../project-session/contracts.js";
import { canonicalJson, sha256 } from "../persistence/row-codec.js";
import { touchProjectSessionMembershipRevision } from "../project-session/membership-store.js";
import type { ResolvedControlTarget } from "./control-eligibility.js";
import { cancelEffectFreeProjectSession } from "./effect-free-session-cancellation.js";
import type { OperatorEffectOutcome, OperatorEffectRequest } from "./action-store.js";

export type Row = Record<string, unknown>;

export type EffectScope = {
  operatorId: string;
  projectId: string;
  projectSessionId: string;
  principalGeneration: number;
  operation: string;
};

export interface OperatorControlFenceHostPort {
  effectScope(request: OperatorEffectRequest): EffectScope;
  storeCustodyOutcome(scope: EffectScope, commandId: string, outcome: OperatorEffectOutcome): void;
}

function isRow(value: unknown): value is Row {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function row(value: unknown, label: string): Row {
  if (!isRow(value)) throw new ProjectFabricCoreError("NOT_FOUND", `${label} was not found`);
  return value;
}

function text(value: Row, field: string): string {
  const candidate = value[field];
  if (typeof candidate !== "string") throw new Error(`${field} is not text`);
  return candidate;
}

function integer(value: Row, field: string): number {
  const candidate = value[field];
  if (typeof candidate !== "number" || !Number.isSafeInteger(candidate)) throw new Error(`${field} is not an integer`);
  return candidate;
}

function unsupported(): never {
  throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "operator action runtime is unavailable for this intent");
}

/**
 * Byte-moved from `OperatorControlActions` (S4g, split out of that class to keep both files under
 * the 1,000-line ratchet): the pause/resume/cancel fence family — `#freeze`, `#resume`, `#cancel`,
 * `#cancelEffectFreeSession`, `#settleCancelledTask`. Preserves: freeze/resume/cancel/outcome
 * atomicity (each runs inside one immediate transaction that also stores the custody outcome, matching
 * the original ordering); cancellation refuses to proceed past an ambiguous resource or write
 * obligation; provider effects are only marked no-effect when still `prepared` with zero executions.
 */
export class OperatorControlFenceActions {
  readonly #database: Database.Database;
  readonly #clock: () => number;
  readonly #host: OperatorControlFenceHostPort;
  readonly #retireVolatileProjectSession: ((projectSessionId: string) => void) | undefined;

  constructor(options: {
    database: Database.Database;
    clock: () => number;
    host: OperatorControlFenceHostPort;
    retireVolatileProjectSession?: (projectSessionId: string) => void;
  }) {
    this.#database = options.database;
    this.#clock = options.clock;
    this.#host = options.host;
    this.#retireVolatileProjectSession = options.retireVolatileProjectSession;
  }

  freeze(
    target: ResolvedControlTarget,
    reason: string,
    request: OperatorEffectRequest,
  ): OperatorEffectOutcome {
    const outcome: OperatorEffectOutcome = { status: "committed", afterState: { lifecycleState: "paused" } };
    this.#database.transaction(() => {
      const commandId = reason.slice("operator-pause:".length);
      for (const task of target.tasks) {
        const fenceId = `operator-fence-${sha256(canonicalJson({
          commandId,
          runId: task.runId,
          taskId: task.taskId,
        })).slice(0, 48)}`;
        const existing = this.#database.prepare(`
          SELECT command_id FROM operator_control_fences
           WHERE coordination_run_id=? AND task_id=? AND state='paused'
        `).get(task.runId, task.taskId);
        if (isRow(existing)) {
          if (existing.command_id !== commandId) {
            throw new ProjectFabricCoreError("CONFLICT", "task is paused by another operator command");
          }
          continue;
        }
        this.#database.prepare(`
          INSERT INTO operator_control_fences(
            fence_id, project_session_id, coordination_run_id, task_id, scope_kind,
            target_revision, session_generation, command_id, state, created_at, released_at
          ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'paused', ?, NULL)
        `).run(
          fenceId,
          target.projectSessionId,
          task.runId,
          task.taskId,
          target.scopeKind,
          task.revision,
          target.sessionGeneration,
          commandId,
          this.#clock(),
        );
      }
      if (target.scopeKind === "run" || target.scopeKind === "session") {
        for (const agent of target.agents) {
          const existing = this.#database.prepare(`
            SELECT reason FROM delivery_freezes WHERE run_id=? AND agent_id=?
          `).get(agent.runId, agent.agentId);
          if (isRow(existing) && existing.reason !== reason) {
            throw new ProjectFabricCoreError("CONFLICT", "agent delivery is frozen by another lifecycle owner");
          }
          if (!isRow(existing)) {
            this.#database.prepare(`
              INSERT INTO delivery_freezes(run_id, agent_id, reason, created_at)
              VALUES (?, ?, ?, ?)
            `).run(agent.runId, agent.agentId, reason, this.#clock());
          }
          this.#database.prepare(`
            UPDATE agents SET lifecycle='suspended' WHERE run_id=? AND agent_id=? AND lifecycle='ready'
          `).run(agent.runId, agent.agentId);
        }
      }
      this.#host.storeCustodyOutcome(this.#host.effectScope(request), request.commandId, outcome);
    })();
    return outcome;
  }

  resume(request: OperatorEffectRequest, target: ResolvedControlTarget): OperatorEffectOutcome {
    const outcome: OperatorEffectOutcome = { status: "committed", afterState: { lifecycleState: "active" } };
    this.#database.transaction(() => {
      for (const task of target.tasks) {
        const changed = this.#database.prepare(`
          UPDATE operator_control_fences
             SET state='released', released_at=?
           WHERE coordination_run_id=? AND task_id=? AND state='paused'
        `).run(this.#clock(), task.runId, task.taskId);
        if (changed.changes !== 1) {
          throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "resume requires an exact paused task fence");
        }
      }
      if (target.scopeKind === "run" || target.scopeKind === "session") {
        for (const agent of target.agents) {
          const freeze = row(this.#database.prepare(`
            SELECT reason FROM delivery_freezes WHERE run_id=? AND agent_id=?
          `).get(agent.runId, agent.agentId), "operator pause fence");
          if (!text(freeze, "reason").startsWith("operator-pause:")) {
            throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "resume cannot release another lifecycle owner");
          }
          this.#database.prepare("DELETE FROM delivery_freezes WHERE run_id=? AND agent_id=?")
            .run(agent.runId, agent.agentId);
          this.#database.prepare("UPDATE agents SET lifecycle='ready' WHERE run_id=? AND agent_id=? AND lifecycle='suspended'")
            .run(agent.runId, agent.agentId);
        }
      }
      this.#host.storeCustodyOutcome(this.#host.effectScope(request), request.commandId, outcome);
    })();
    return outcome;
  }

  cancel(target: ResolvedControlTarget, request: OperatorEffectRequest): OperatorEffectOutcome {
    if (request.intent.kind !== "control" || request.intent.action !== "cancel") unsupported();
    if (target.scopeKind === "session" && target.runs.length === 0) {
      return this.#cancelEffectFreeSession(target, request, this.#host.effectScope(request));
    }
    const reason = request.intent.reason;
    const commandId = request.commandId;
    let cancelledTasks = 0;
    let outcome: OperatorEffectOutcome;
    this.#database.transaction(() => {
      for (const task of target.tasks) {
        const changed = this.#database.prepare(`
          UPDATE tasks
             SET state='cancelled', revision=revision+1
           WHERE run_id=? AND task_id=? AND revision=?
             AND state NOT IN ('complete','cancelled','degraded')
        `).run(task.runId, task.taskId, task.revision);
        cancelledTasks += changed.changes;
        if (changed.changes === 1) {
          this.#settleCancelledTask(target.projectSessionId, task, reason, commandId);
          this.#database.prepare(`
            UPDATE task_owner_leases SET status='released', updated_at=?
             WHERE run_id=? AND task_id=? AND generation=? AND status IN ('active','frozen')
          `).run(this.#clock(), task.runId, task.taskId, task.ownerLeaseGeneration);
          this.#database.prepare(`
            UPDATE project_session_memberships
               SET state='abandoned', abandoned_reason=?, revision=revision+1, updated_at=?
             WHERE project_session_id=? AND coordination_run_id=?
               AND member_kind='task' AND member_id=? AND state='active'
          `).run(reason, this.#clock(), target.projectSessionId, task.runId, task.taskId);
        }
        this.#database.prepare(`
          UPDATE operator_control_fences SET state='cancelled', released_at=?
           WHERE coordination_run_id=? AND task_id=? AND state='paused'
        `).run(this.#clock(), task.runId, task.taskId);
      }
      if (cancelledTasks === 0) {
        outcome = { status: "rejected", code: "state-changed", evidenceRefs: [] };
        return;
      }
      touchProjectSessionMembershipRevision(
        this.#database,
        target.projectSessionId,
        this.#clock(),
        cancelledTasks,
      );
      outcome = { status: "committed", afterState: { lifecycleState: "cancelled", cancelledTasks } };
      this.#host.storeCustodyOutcome(this.#host.effectScope(request), request.commandId, outcome);
    })();
    return outcome!;
  }

  #cancelEffectFreeSession(
    target: ResolvedControlTarget,
    request: OperatorEffectRequest,
    scope: EffectScope,
  ): OperatorEffectOutcome {
    if (request.intent.kind !== "control" || request.intent.action !== "cancel") unsupported();
    return cancelEffectFreeProjectSession({
      database: this.#database,
      clock: this.#clock,
      input: {
        projectSessionId: target.projectSessionId,
        expectedRevision: target.revision,
        expectedGeneration: target.sessionGeneration,
        reason: request.intent.reason,
        commandId: request.commandId,
      },
      storeCustodyOutcome: (outcome) => this.#host.storeCustodyOutcome(scope, request.commandId, outcome),
      ...(this.#retireVolatileProjectSession === undefined
        ? {}
        : { retireVolatileProjectSession: this.#retireVolatileProjectSession }),
    });
  }

  #settleCancelledTask(
    projectSessionId: string,
    task: ResolvedControlTarget["tasks"][number],
    reason: string,
    commandId: string,
  ): void {
    const unsafeReservation = this.#database.prepare(`
      SELECT 1 FROM task_obligation_bindings binding
      JOIN resource_reservations reservation ON reservation.reservation_id=binding.obligation_id
      WHERE binding.coordination_run_id=? AND binding.task_id=?
        AND binding.obligation_kind='resource-reservation' AND binding.state='active'
        AND (reservation.state='ambiguous' OR EXISTS (
          SELECT 1 FROM resource_reservation_dimensions dimension
           WHERE dimension.reservation_id=reservation.reservation_id AND dimension.usage_unknown=1
        ))
    `).get(task.runId, task.taskId);
    const unsafeLease = this.#database.prepare(`
      SELECT 1 FROM task_obligation_bindings binding
      JOIN leases lease ON lease.lease_id=binding.obligation_id
      WHERE binding.coordination_run_id=? AND binding.task_id=?
        AND binding.obligation_kind='write-lease' AND binding.state='active'
        AND lease.status='quarantined'
    `).get(task.runId, task.taskId);
    if (isRow(unsafeReservation) || isRow(unsafeLease)) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "task cancellation has an ambiguous resource or write obligation");
    }

    const providerRows = this.#database.prepare(`
      SELECT adapter_id, action_id, status, execution_count FROM provider_actions
       WHERE run_id=? AND json_extract(payload_json, '$.taskId')=?
         AND status IN ('prepared','dispatched','accepted','ambiguous','quarantined')
    `).all(task.runId, task.taskId).filter(isRow);
    if (providerRows.some((action) => text(action, "status") !== "prepared" || integer(action, "execution_count") !== 0)) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "task cancellation has an unresolved provider effect");
    }
    for (const action of providerRows) {
      const actionId = text(action, "action_id");
      const adapterId = text(action, "adapter_id");
      this.#database.prepare(`
        UPDATE provider_actions
           SET status='terminal', history_json='["prepared","terminal"]',
               effect_count=0, idempotency_proven=1, result_json=?,
               journal_revision=journal_revision+1, updated_at=?
         WHERE run_id=? AND adapter_id=? AND action_id=? AND status='prepared' AND execution_count=0
      `).run(
        canonicalJson({ cancelled: true, reason, commandId, provedNoEffect: true }),
        this.#clock(),
        task.runId,
        adapterId,
        actionId,
      );
      this.#database.prepare(`
        UPDATE provider_session_turn_leases SET status='released', updated_at=?
         WHERE run_id=? AND adapter_id=? AND action_id=? AND status='active'
      `).run(this.#clock(), task.runId, adapterId, actionId);
      this.#database.prepare(`
        UPDATE project_session_memberships
           SET state='abandoned', abandoned_reason=?, revision=revision+1, updated_at=?
         WHERE project_session_id=? AND coordination_run_id=?
           AND member_kind='provider-action' AND member_adapter_id=? AND member_id=? AND state='active'
      `).run(reason, this.#clock(), projectSessionId, task.runId, adapterId, actionId);
    }

    const requests = this.#database.prepare(`
      SELECT request_id, request_message_id, dependent_barrier_id
        FROM task_requests WHERE run_id=? AND task_id=?
    `).all(task.runId, task.taskId).filter(isRow);
    for (const request of requests) {
      const requestId = text(request, "request_id");
      this.#database.prepare(`
        UPDATE task_requests SET state='abandoned', updated_at=?
         WHERE request_id=? AND state<>'abandoned'
      `).run(this.#clock(), requestId);
      this.#database.prepare(`
        UPDATE task_request_barriers SET state='abandoned'
         WHERE request_id=? AND state='blocked'
      `).run(requestId);
      this.#database.prepare(`
        UPDATE deliveries SET state='abandoned', resolution_reason=?, resolved_at=?
         WHERE delivery_id IN (
           SELECT delivery_id FROM task_request_recipients WHERE request_id=?
         ) AND state NOT IN ('acknowledged','abandoned','expired')
      `).run(reason, this.#clock(), requestId);
      this.#database.prepare(`
        UPDATE project_session_memberships
           SET state='abandoned', abandoned_reason=?, revision=revision+1, updated_at=?
         WHERE project_session_id=? AND coordination_run_id=? AND state='active' AND (
           (member_kind='required-message' AND member_id=?) OR
           (member_kind='scoped-barrier' AND member_id=?)
         )
      `).run(
        reason,
        this.#clock(),
        projectSessionId,
        task.runId,
        text(request, "request_message_id"),
        text(request, "dependent_barrier_id"),
      );
    }
    this.#database.prepare(`
      UPDATE result_deliveries
         SET state='abandoned', required=0, abandoned_reason=?, abandoned_at=?, updated_at=?, revision=revision+1
       WHERE run_id=? AND task_id=? AND state NOT IN ('consumed','abandoned')
    `).run(reason, this.#clock(), this.#clock(), task.runId, task.taskId);
    this.#database.prepare(`
      UPDATE deliveries SET state='abandoned', resolution_reason=?, resolved_at=?
       WHERE message_id IN (
         SELECT reply_message_id FROM task_results WHERE run_id=? AND task_id=?
       ) AND state NOT IN ('acknowledged','abandoned','expired')
    `).run(reason, this.#clock(), task.runId, task.taskId);
    this.#database.prepare(`
      UPDATE project_session_memberships
         SET state='abandoned', abandoned_reason=?, revision=revision+1, updated_at=?
       WHERE project_session_id=? AND coordination_run_id=? AND member_kind='required-message'
         AND member_id IN (SELECT reply_message_id FROM task_results WHERE run_id=? AND task_id=?)
         AND state='active'
    `).run(reason, this.#clock(), projectSessionId, task.runId, task.runId, task.taskId);

    const bindings = this.#database.prepare(`
      SELECT obligation_kind, obligation_id FROM task_obligation_bindings
       WHERE coordination_run_id=? AND task_id=? AND state='active'
       ORDER BY obligation_kind, obligation_id
    `).all(task.runId, task.taskId).filter(isRow);
    for (const binding of bindings) {
      const kind = text(binding, "obligation_kind");
      const obligationId = text(binding, "obligation_id");
      if (kind === "write-lease") {
        this.#database.prepare(`
          UPDATE leases SET status='released', updated_at=?
           WHERE lease_id=? AND run_id=? AND status='active'
        `).run(this.#clock(), obligationId, task.runId);
        this.#database.prepare(`
          UPDATE project_session_memberships
             SET state='abandoned', abandoned_reason=?, revision=revision+1, updated_at=?
           WHERE project_session_id=? AND coordination_run_id=?
             AND member_kind='lease' AND member_id=? AND state='active'
        `).run(reason, this.#clock(), projectSessionId, task.runId, obligationId);
      } else if (kind === "resource-reservation") {
        const dimensions = this.#database.prepare(`
          SELECT scope_id, unit_key, amount-consumed-released AS remainder
            FROM resource_reservation_dimensions WHERE reservation_id=?
        `).all(obligationId).filter(isRow);
        for (const dimension of dimensions) {
          const remainder = integer(dimension, "remainder");
          if (remainder > 0) {
            const released = this.#database.prepare(`
              UPDATE resource_dimensions SET reserved=reserved-?
               WHERE scope_id=? AND unit_key=? AND reserved>=?
            `).run(
              remainder,
              text(dimension, "scope_id"),
              text(dimension, "unit_key"),
              remainder,
            );
            if (released.changes !== 1) throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "resource cancellation ledger changed");
            this.#database.prepare(`
              UPDATE resource_reservation_dimensions SET released=released+?
               WHERE reservation_id=? AND scope_id=? AND unit_key=?
            `).run(remainder, obligationId, text(dimension, "scope_id"), text(dimension, "unit_key"));
          }
        }
        this.#database.prepare(`
          UPDATE resource_reservations SET state='released', revision=revision+1, updated_at=?
           WHERE reservation_id=? AND state IN ('reserved','partially-consumed')
        `).run(this.#clock(), obligationId);
        this.#database.prepare(`
          UPDATE writer_admissions SET state='revoked'
           WHERE reservation_id=? AND state='active'
        `).run(obligationId);
      }
      this.#database.prepare(`
        UPDATE task_obligation_bindings SET state='abandoned', updated_at=?
         WHERE coordination_run_id=? AND task_id=? AND obligation_kind=? AND obligation_id=? AND state='active'
      `).run(this.#clock(), task.runId, task.taskId, kind, obligationId);
    }

    const dedupeKey = `operator-cancel:${task.runId}:${task.taskId}:${String(task.revision)}`;
    const itemId = `attention_${sha256(`${projectSessionId}\0${dedupeKey}`).slice(0, 24)}`;
    this.#database.prepare(`
      INSERT INTO attention_items(
        item_id, project_session_id, coordination_run_id, kind, severity,
        revision, state, dedupe_key, payload_json, created_at, updated_at
      ) VALUES (?, ?, ?, 'operator-task-cancelled', 'info', 1, 'open', ?, ?, ?, ?)
      ON CONFLICT(project_session_id, dedupe_key) DO NOTHING
    `).run(
      itemId,
      projectSessionId,
      task.runId,
      dedupeKey,
      canonicalJson({ taskId: task.taskId, taskRevision: task.revision, reason, commandId }),
      this.#clock(),
      this.#clock(),
    );
  }
}
