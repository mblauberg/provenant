import {
  FABRIC_OPERATIONS,
  assertWriterAdmissionCurrent,
  parseResourceReservationRequest,
  type ResourceAmounts,
  type ResourceDimensionProjection,
  type ResourceReconcileRequest,
  type ResourceReleaseRequest,
  type ResourceReservation,
  type ResourceReservationRequest,
  type ResourceScopeRef,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";
import { resolveTaskBindingForActiveWork } from "../operator/task-run-admission.js";
import {
  assertScopedOperationAllowed,
  assertScopedTaskReadinessAllowed,
} from "../gates/store.js";

import {
  ProjectFabricCoreError,
  type AuthenticatedAgentContext,
  type CoreServiceOptions,
} from "../project-session/contracts.js";
import {
  canonicalJson,
  integer,
  isRow,
  row,
  sha256,
  text,
  type Row,
} from "../persistence/row-codec.js";

export type EnsureRunHierarchyContext = Readonly<{
  projectId: string;
  projectSessionId: string;
  coordinationRunId: string;
  actor: { kind: "operator-launch"; operatorId: string };
}>;

export type ScopeLimits = Readonly<Record<string, number>>;

export type EnsureRunHierarchyRequest = Readonly<{
  project: { scopeId: string; limits: ScopeLimits };
  session: { scopeId: string; limits: ScopeLimits };
  run: { scopeId: string; limits: ScopeLimits };
}>;

export type ResourceScopeProjection = Readonly<{
  scopeId: string;
  kind: "project" | "project-session" | "coordination-run" | "team" | "agent";
  parentScopeId: string | null;
  ownerRef: string;
  state: "active" | "usage-unknown" | "released";
  revision: number;
  dimensions: Readonly<Record<string, ResourceDimensionProjection & { limit: number }>>;
}>;

export type ChildScopeRequest = Readonly<{
  scopeId: string;
  parentScopeId: string;
  kind: "team" | "agent";
  ownerRef: string;
  limits: ScopeLimits;
}>;

export type MarkReservationAmbiguousRequest = Readonly<{
  commandId: string;
  reservationId: string;
  expectedRevision: number;
  evidence: string;
}>;

export class HierarchicalAdmissionStore {
  readonly #database: Database.Database;
  readonly #clock: () => number;
  readonly #fault: (label: string) => void;

  constructor(options: CoreServiceOptions) {
    this.#database = options.database;
    this.#clock = options.clock ?? Date.now;
    this.#fault = options.fault ?? (() => undefined);
  }

  ensureRunHierarchy(
    context: EnsureRunHierarchyContext,
    request: EnsureRunHierarchyRequest,
  ): readonly ResourceScopeProjection[] {
    const execute = this.#database.transaction((): readonly ResourceScopeProjection[] => {
      this.#assertHierarchyContext(context);
      this.#validateLimits(request.project.limits, "project");
      this.#validateLimits(request.session.limits, "project-session");
      this.#validateLimits(request.run.limits, "coordination-run");
      this.#assertNarrowing(request.project.limits, request.session.limits, "project-session");
      this.#assertNarrowing(request.session.limits, request.run.limits, "coordination-run");
      const definitions = [
        {
          scopeId: request.project.scopeId,
          kind: "project" as const,
          parentScopeId: null,
          projectSessionId: null,
          runId: null,
          ownerRef: context.projectId,
          limits: request.project.limits,
        },
        {
          scopeId: request.session.scopeId,
          kind: "project-session" as const,
          parentScopeId: request.project.scopeId,
          projectSessionId: context.projectSessionId,
          runId: null,
          ownerRef: context.projectSessionId,
          limits: request.session.limits,
        },
        {
          scopeId: request.run.scopeId,
          kind: "coordination-run" as const,
          parentScopeId: request.session.scopeId,
          projectSessionId: context.projectSessionId,
          runId: context.coordinationRunId,
          ownerRef: context.coordinationRunId,
          limits: request.run.limits,
        },
      ];
      for (const definition of definitions) this.#ensureScope(context.projectId, definition);
      this.#fault("resources:hierarchy:after-scopes");
      return definitions.map(({ scopeId }) => this.projectScope(scopeId));
    });
    return execute();
  }

  defineChildScope(context: AuthenticatedAgentContext, request: ChildScopeRequest): ResourceScopeProjection {
    const execute = this.#database.transaction((): ResourceScopeProjection => {
      this.#assertAgentContext(context);
      this.#validateLimits(request.limits, request.kind);
      const parent = this.#scopeRow(request.parentScopeId);
      const expectedParent = request.kind === "team" ? "coordination-run" : "team";
      if (text(parent, "scope_kind") !== expectedParent) {
        throw new ProjectFabricCoreError("CONFLICT", `${request.kind} scope has the wrong parent kind`);
      }
      this.#assertNarrowing(this.#scopeLimits(request.parentScopeId), request.limits, request.kind);
      this.#ensureScope(text(parent, "project_id"), {
        scopeId: request.scopeId,
        kind: request.kind,
        parentScopeId: request.parentScopeId,
        projectSessionId: context.projectSessionId,
        runId: context.coordinationRunId,
        ownerRef: request.ownerRef,
        limits: request.limits,
      });
      return this.projectScope(request.scopeId);
    });
    return execute();
  }

  reserve(context: AuthenticatedAgentContext, value: ResourceReservationRequest): ResourceReservation {
    const request = parseResourceReservationRequest(value);
    const requestedTaskId = (request as ResourceReservationRequest & { taskId?: string }).taskId;
    this.#assertAgentContext(context);
    if (
      request.projectSessionId !== context.projectSessionId ||
      !request.path.some(
        (scope) => scope.kind === "coordination-run" && scope.coordinationRunId === context.coordinationRunId,
      )
    ) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "reservation path is outside the authenticated run");
    }
    const execute = this.#database.transaction((): ResourceReservation => {
      const identityHash = sha256(canonicalJson({ actorAgentId: context.agentId, request }));
      const existing = this.#database.prepare(`
        SELECT identity_hash FROM resource_reservations WHERE reservation_id=?
      `).get(request.reservationId);
      if (isRow(existing)) {
        if (text(existing, "identity_hash") !== identityHash) {
          throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "reservation ID was reused with changed input");
        }
        return this.project(request.reservationId);
      }
      const taskId = resolveTaskBindingForActiveWork(
        this.#database,
        context.coordinationRunId,
        context.agentId,
        requestedTaskId,
      );
      assertScopedOperationAllowed(
        this.#database,
        context.coordinationRunId,
        FABRIC_OPERATIONS.resourceReserve,
        taskId === undefined ? { kind: "run" } : { kind: "task", taskId: taskId as never },
      );
      if (taskId !== undefined) {
        assertScopedTaskReadinessAllowed(this.#database, context.coordinationRunId, taskId);
      }
      this.#assertPath(request.path);
      const writer = request.writerAdmission === undefined
        ? undefined
        : assertWriterAdmissionCurrent(request.writerAdmission);
      if (writer !== undefined) this.#assertWriterPrefixesAvailable(writer.repositoryRoot, writer.sourcePrefixes);
      for (const scope of request.path) {
        for (const [unit, amount] of Object.entries(request.amounts)) {
          const dimension = this.#dimension(scope.scopeId, unit);
          if (integer(dimension, "usage_unknown") === 1) {
            throw new ProjectFabricCoreError("RESOURCE_USAGE_UNKNOWN", `${unit} usage is unknown at ${scope.kind}`);
          }
          const remaining = integer(dimension, "limit_value") - integer(dimension, "used") - integer(dimension, "reserved");
          if (amount > remaining) {
            throw new ProjectFabricCoreError("RESOURCE_EXHAUSTED", `${unit} is exhausted at ${scope.kind}`);
          }
        }
      }
      const now = this.#clock();
      const leaf = request.path.at(-1);
      if (leaf === undefined) throw new ProjectFabricCoreError("PROTOCOL_INVALID", "reservation path is empty");
      this.#database.prepare(`
        INSERT INTO resource_reservations(
          reservation_id, project_session_id, coordination_run_id, leaf_scope_id,
          operation_id, actor_agent_id, state, revision, generation, identity_hash,
          path_json, amounts_json, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, 'reserved', 1, 1, ?, ?, ?, ?, ?)
      `).run(
        request.reservationId,
        request.projectSessionId,
        context.coordinationRunId,
        leaf.scopeId,
        taskId ?? null,
        context.agentId,
        identityHash,
        canonicalJson(request.path),
        canonicalJson(request.amounts),
        now,
        now,
      );
      const insertDimension = this.#database.prepare(`
        INSERT INTO resource_reservation_dimensions(
          reservation_id, scope_id, unit_key, amount, consumed, released, usage_unknown
        ) VALUES (?, ?, ?, ?, 0, 0, 0)
      `);
      for (const scope of request.path) {
        for (const [unit, amount] of Object.entries(request.amounts)) {
          const changed = this.#database.prepare(`
            UPDATE resource_dimensions
               SET reserved=reserved+?
             WHERE scope_id=? AND unit_key=? AND usage_unknown=0
               AND limit_value-used-reserved>=?
          `).run(amount, scope.scopeId, unit, amount);
          if (changed.changes !== 1) {
            throw new ProjectFabricCoreError("RESOURCE_EXHAUSTED", `${unit} changed during admission`);
          }
          insertDimension.run(request.reservationId, scope.scopeId, unit, amount);
        }
      }
      if (writer !== undefined) this.#insertWriter(request.reservationId, writer);
      if (taskId !== undefined) {
        this.#database.prepare(`
          INSERT INTO task_obligation_bindings(
            coordination_run_id, task_id, obligation_kind, obligation_id, state, created_at, updated_at
          ) VALUES (?, ?, 'resource-reservation', ?, 'active', ?, ?)
        `).run(context.coordinationRunId, taskId, request.reservationId, now, now);
      }
      this.#fault("resources:reserve:after-ledger");
      return this.project(request.reservationId);
    });
    return execute();
  }

  release(context: AuthenticatedAgentContext, request: ResourceReleaseRequest): ResourceReservation {
    return this.#executeReservationCommand(context, request.commandId, request, () => {
      const reservation = this.#reservationRow(request.reservationId);
      this.#assertReservationContext(context, reservation);
      if (integer(reservation, "revision") !== request.expectedRevision) {
        throw new ProjectFabricCoreError("STALE_REVISION", "reservation revision changed");
      }
      const state = text(reservation, "state");
      if (state === "released") return this.project(request.reservationId);
      if (state !== "reserved" && state !== "partially-consumed" && state !== "consumed") {
        throw new ProjectFabricCoreError("CONFLICT", "only an active reservation may be released");
      }
      const amounts = this.#amounts(reservation);
      this.#validateObservedUsage(request.consumed, amounts, false);
      this.#settleReservation(request.reservationId, request.consumed, false);
      this.#database.prepare(`
        UPDATE resource_reservations
           SET state='released', revision=revision+1, updated_at=?
         WHERE reservation_id=? AND revision=?
      `).run(this.#clock(), request.reservationId, request.expectedRevision);
      this.#releaseWriter(request.reservationId);
      this.#fault("resources:release:after-ledger");
      return this.project(request.reservationId);
    });
  }

  markAmbiguous(
    context: AuthenticatedAgentContext,
    request: MarkReservationAmbiguousRequest,
  ): ResourceReservation {
    return this.#executeReservationCommand(context, request.commandId, request, () => {
      const reservation = this.#reservationRow(request.reservationId);
      this.#assertReservationContext(context, reservation);
      if (integer(reservation, "revision") !== request.expectedRevision) {
        throw new ProjectFabricCoreError("STALE_REVISION", "reservation revision changed");
      }
      if (text(reservation, "state") !== "reserved") {
        throw new ProjectFabricCoreError("CONFLICT", "only a reserved effect may become ambiguous");
      }
      if (request.evidence.trim().length === 0) {
        throw new ProjectFabricCoreError("PROTOCOL_INVALID", "ambiguity evidence is required");
      }
      this.#database.prepare(`
        UPDATE resource_reservations
           SET state='ambiguous', revision=revision+1, updated_at=?
         WHERE reservation_id=? AND revision=?
      `).run(this.#clock(), request.reservationId, request.expectedRevision);
      return this.project(request.reservationId);
    });
  }

  reconcile(context: AuthenticatedAgentContext, request: ResourceReconcileRequest): ResourceReservation {
    return this.#executeReservationCommand(context, request.commandId, request, () => {
      const reservation = this.#reservationRow(request.reservationId);
      this.#assertReservationContext(context, reservation);
      if (integer(reservation, "revision") !== request.expectedRevision) {
        throw new ProjectFabricCoreError("STALE_REVISION", "reservation revision changed");
      }
      if (text(reservation, "state") !== "ambiguous") {
        throw new ProjectFabricCoreError("CONFLICT", "only an ambiguous reservation may be reconciled");
      }
      if (request.evidence.trim().length === 0) {
        throw new ProjectFabricCoreError("PROTOCOL_INVALID", "reconciliation evidence is required");
      }
      const amounts = this.#amounts(reservation);
      this.#validateObservedUsage(request.observedUsage, amounts, true);
      this.#settleReservation(request.reservationId, request.observedUsage, true);
      this.#database.prepare(`
        UPDATE resource_reservations
           SET state='reconciled', revision=revision+1, updated_at=?
         WHERE reservation_id=? AND revision=?
      `).run(this.#clock(), request.reservationId, request.expectedRevision);
      this.#releaseWriter(request.reservationId);
      this.#fault("resources:reconcile:after-ledger");
      return this.project(request.reservationId);
    });
  }

  project(reservationId: string): ResourceReservation {
    const value = this.#reservationRow(reservationId);
    const path = JSON.parse(text(value, "path_json")) as ResourceScopeRef[];
    const amounts = this.#amounts(value);
    const leaf = path.at(-1);
    if (leaf === undefined) throw new Error("stored reservation path is empty");
    const state = text(value, "state");
    return {
      reservationId: reservationId as ResourceReservation["reservationId"],
      revision: integer(value, "revision"),
      state: state === "released" || state === "consumed" ? "released"
        : state === "ambiguous" ? "ambiguous"
          : state === "reconciled" ? "reconciled" : "active",
      path,
      amounts,
      capacity: this.#capacity(leaf.scopeId, Object.keys(amounts)),
    };
  }

  projectScope(scopeId: string): ResourceScopeProjection {
    const value = this.#scopeRow(scopeId);
    const dimensions = this.#database.prepare(`
      SELECT * FROM resource_dimensions WHERE scope_id=? ORDER BY unit_key
    `).all(scopeId).filter(isRow);
    const projected: Record<string, ResourceDimensionProjection & { limit: number }> = {};
    for (const dimension of dimensions) {
      const unit = text(dimension, "unit_key");
      const limit = integer(dimension, "limit_value");
      const used = integer(dimension, "used");
      const reserved = integer(dimension, "reserved");
      projected[unit] = integer(dimension, "usage_unknown") === 1
        ? { unknown: true, used: null, reserved, remaining: null, limit }
        : { unknown: false, used, reserved, remaining: limit - used - reserved, limit };
    }
    return {
      scopeId,
      kind: text(value, "scope_kind") as ResourceScopeProjection["kind"],
      parentScopeId: value.parent_scope_id === null ? null : text(value, "parent_scope_id"),
      ownerRef: text(value, "owner_ref"),
      state: text(value, "state") as ResourceScopeProjection["state"],
      revision: integer(value, "revision"),
      dimensions: projected,
    };
  }

  #ensureScope(
    projectId: string,
    definition: Readonly<{
      scopeId: string;
      kind: ResourceScopeProjection["kind"];
      parentScopeId: string | null;
      projectSessionId: string | null;
      runId: string | null;
      ownerRef: string;
      limits: ScopeLimits;
    }>,
  ): void {
    const existing = this.#database.prepare("SELECT * FROM resource_scopes WHERE scope_id=?")
      .get(definition.scopeId);
    if (isRow(existing)) {
      const exact = text(existing, "project_id") === projectId &&
        text(existing, "scope_kind") === definition.kind &&
        existing.parent_scope_id === definition.parentScopeId &&
        existing.project_session_id === definition.projectSessionId &&
        existing.coordination_run_id === definition.runId &&
        text(existing, "owner_ref") === definition.ownerRef &&
        canonicalJson(this.#scopeLimits(definition.scopeId)) === canonicalJson(definition.limits);
      if (!exact) {
        throw new ProjectFabricCoreError("AUTHORITY_WIDENING", `${definition.kind} scope replay conflicts or widens authority`);
      }
      return;
    }
    this.#database.prepare(`
      INSERT INTO resource_scopes(
        scope_id, project_id, project_session_id, coordination_run_id,
        parent_scope_id, scope_kind, owner_ref, state, revision
      ) VALUES (?, ?, ?, ?, ?, ?, ?, 'active', 1)
    `).run(
      definition.scopeId,
      projectId,
      definition.projectSessionId,
      definition.runId,
      definition.parentScopeId,
      definition.kind,
      definition.ownerRef,
    );
    const insert = this.#database.prepare(`
      INSERT INTO resource_dimensions(scope_id, unit_key, limit_value, used, reserved, usage_unknown)
      VALUES (?, ?, ?, 0, 0, 0)
    `);
    for (const [unit, limit] of Object.entries(definition.limits).sort(([left], [right]) => left.localeCompare(right))) {
      insert.run(definition.scopeId, unit, limit);
    }
  }

  #assertHierarchyContext(context: EnsureRunHierarchyContext): void {
    const run = row(this.#database.prepare(`
      SELECT r.project_session_id, s.project_id
        FROM runs r JOIN project_sessions s ON s.project_session_id=r.project_session_id
       WHERE r.run_id=?
    `).get(context.coordinationRunId), "coordination run");
    if (
      text(run, "project_session_id") !== context.projectSessionId ||
      text(run, "project_id") !== context.projectId
    ) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "run hierarchy context does not match persistence");
    }
    if (context.actor.operatorId.trim().length === 0) {
      throw new ProjectFabricCoreError("PROTOCOL_INVALID", "hierarchy actor is required");
    }
  }

  #assertAgentContext(context: AuthenticatedAgentContext): void {
    const run = row(this.#database.prepare(`
      SELECT project_session_id FROM runs WHERE run_id=?
    `).get(context.coordinationRunId), "coordination run");
    if (text(run, "project_session_id") !== context.projectSessionId) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "agent context is bound to another project session");
    }
    if (this.#database.prepare(`
      SELECT 1 FROM agents WHERE run_id=? AND agent_id=?
    `).get(context.coordinationRunId, context.agentId) === undefined) {
      throw new ProjectFabricCoreError("TASK_NOT_OWNER", "authenticated agent is not registered in the run");
    }
  }

  #assertPath(path: readonly ResourceScopeRef[]): void {
    let prior: Row | undefined;
    for (const scope of path) {
      const stored = this.#scopeRow(scope.scopeId);
      if (text(stored, "scope_kind") !== scope.kind) {
        throw new ProjectFabricCoreError("CONFLICT", "reservation scope kind changed");
      }
      if (prior !== undefined && stored.parent_scope_id !== text(prior, "scope_id")) {
        throw new ProjectFabricCoreError("AUTHORITY_WIDENING", "reservation path is not the stored ancestor chain");
      }
      const ownerMatches = scope.kind === "project" ? text(stored, "owner_ref") === scope.projectId
        : scope.kind === "project-session" ? text(stored, "owner_ref") === scope.projectSessionId
          : scope.kind === "coordination-run" ? text(stored, "owner_ref") === scope.coordinationRunId
            : scope.kind === "team" ? text(stored, "owner_ref") === scope.teamId
              : text(stored, "owner_ref") === scope.agentId;
      if (!ownerMatches || text(stored, "state") === "released") {
        throw new ProjectFabricCoreError("AUTHORITY_WIDENING", "reservation path identity is stale or inactive");
      }
      prior = stored;
    }
  }

  #assertNarrowing(parent: ScopeLimits, child: ScopeLimits, label: string): void {
    for (const [unit, limit] of Object.entries(child)) {
      const parentLimit = parent[unit];
      if (parentLimit === undefined || limit > parentLimit) {
        throw new ProjectFabricCoreError("AUTHORITY_WIDENING", `${label} limit widens ${unit}`);
      }
    }
  }

  #validateLimits(limits: ScopeLimits, label: string, allowEmpty = false): void {
    if (!allowEmpty && Object.keys(limits).length === 0) {
      throw new ProjectFabricCoreError("PROTOCOL_INVALID", `${label} limits are empty`);
    }
    for (const [unit, limit] of Object.entries(limits)) {
      if (!Number.isSafeInteger(limit) || limit < 0 || unit.length === 0) {
        throw new ProjectFabricCoreError("PROTOCOL_INVALID", `${label} limit ${unit} is invalid`);
      }
    }
  }

  #scopeLimits(scopeId: string): Record<string, number> {
    const result: Record<string, number> = {};
    for (const value of this.#database.prepare(`
      SELECT unit_key, limit_value FROM resource_dimensions WHERE scope_id=? ORDER BY unit_key
    `).all(scopeId).filter(isRow)) {
      result[text(value, "unit_key")] = integer(value, "limit_value");
    }
    return result;
  }

  #validateObservedUsage(
    observed: Readonly<Record<string, number | "unknown">>,
    amounts: ResourceAmounts,
    allowUnknown: boolean,
  ): void {
    if (canonicalJson(Object.keys(observed).sort()) !== canonicalJson(Object.keys(amounts).sort())) {
      throw new ProjectFabricCoreError("PROTOCOL_INVALID", "usage dimensions do not match the reservation");
    }
    for (const [unit, value] of Object.entries(observed)) {
      const maximum = amounts[unit];
      if (
        maximum === undefined ||
        (value === "unknown" ? !allowUnknown : !Number.isSafeInteger(value) || value < 0 || value > maximum)
      ) {
        throw new ProjectFabricCoreError("PROTOCOL_INVALID", `observed ${unit} usage is invalid`);
      }
    }
  }

  #settleReservation(
    reservationId: string,
    observed: Readonly<Record<string, number | "unknown">>,
    allowUnknown: boolean,
  ): void {
    const rows = this.#database.prepare(`
      SELECT * FROM resource_reservation_dimensions WHERE reservation_id=?
       ORDER BY scope_id, unit_key
    `).all(reservationId).filter(isRow);
    for (const entry of rows) {
      const unit = text(entry, "unit_key");
      const amount = integer(entry, "amount");
      const value = observed[unit];
      if (value === undefined) throw new ProjectFabricCoreError("PROTOCOL_INVALID", `missing ${unit} usage`);
      const unknown = value === "unknown";
      if (unknown && !allowUnknown) throw new ProjectFabricCoreError("PROTOCOL_INVALID", `${unit} usage cannot be unknown`);
      const consumed = unknown ? 0 : value;
      const changed = this.#database.prepare(`
        UPDATE resource_dimensions
           SET reserved=reserved-?, used=used+?, usage_unknown=CASE WHEN ?=1 THEN 1 ELSE usage_unknown END
         WHERE scope_id=? AND unit_key=? AND reserved>=?
           AND used+?<=limit_value
      `).run(
        amount,
        consumed,
        unknown ? 1 : 0,
        text(entry, "scope_id"),
        unit,
        amount,
        consumed,
      );
      if (changed.changes !== 1) {
        throw new ProjectFabricCoreError("RESOURCE_EXHAUSTED", `${unit} reconciliation exceeds its ancestor`);
      }
      this.#database.prepare(`
        UPDATE resource_reservation_dimensions
           SET consumed=?, released=?, usage_unknown=?
         WHERE reservation_id=? AND scope_id=? AND unit_key=?
      `).run(
        consumed,
        amount - consumed,
        unknown ? 1 : 0,
        reservationId,
        text(entry, "scope_id"),
        unit,
      );
      this.#refreshScopeState(text(entry, "scope_id"));
    }
  }

  #refreshScopeState(scopeId: string): void {
    const unknown = this.#database.prepare(`
      SELECT 1 FROM resource_dimensions WHERE scope_id=? AND usage_unknown=1 LIMIT 1
    `).get(scopeId) !== undefined;
    const current = this.#scopeRow(scopeId);
    const next = unknown ? "usage-unknown" : "active";
    if (text(current, "state") !== next) {
      this.#database.prepare(`
        UPDATE resource_scopes SET state=?, revision=revision+1 WHERE scope_id=?
      `).run(next, scopeId);
    }
  }

  #insertWriter(
    reservationId: string,
    writer: Readonly<{
      repositoryRoot: string;
      worktreePath: string;
      sourcePrefixes: readonly string[];
      writerGeneration: number;
    }>,
  ): void {
    const writerId = `writer:${reservationId}`;
    this.#database.prepare(`
      INSERT INTO writer_admissions(
        writer_admission_id, reservation_id, repository_root, worktree_path,
        writer_generation, state
      ) VALUES (?, ?, ?, ?, ?, 'active')
    `).run(writerId, reservationId, writer.repositoryRoot, writer.worktreePath, writer.writerGeneration);
    const insert = this.#database.prepare(`
      INSERT INTO writer_prefixes(writer_admission_id, canonical_prefix) VALUES (?, ?)
    `);
    for (const prefix of [...writer.sourcePrefixes].sort()) insert.run(writerId, prefix);
  }

  #assertWriterPrefixesAvailable(repositoryRoot: string, prefixes: readonly string[]): void {
    const active = this.#database.prepare(`
      SELECT w.repository_root, p.canonical_prefix
        FROM writer_admissions w JOIN writer_prefixes p USING(writer_admission_id)
       WHERE w.state='active'
    `).all().filter(isRow);
    for (const prefix of prefixes) {
      for (const value of active) {
        if (text(value, "repository_root") !== repositoryRoot) continue;
        const other = text(value, "canonical_prefix");
        if (prefix === other || prefix.startsWith(`${other}/`) || other.startsWith(`${prefix}/`)) {
          throw new ProjectFabricCoreError("WRITE_SCOPE_CONFLICT", `writer prefix ${prefix} overlaps ${other}`);
        }
      }
    }
  }

  #releaseWriter(reservationId: string): void {
    this.#database.prepare(`
      UPDATE writer_admissions SET state='released'
       WHERE reservation_id=? AND state='active'
    `).run(reservationId);
  }

  #executeReservationCommand<Result>(
    context: AuthenticatedAgentContext,
    commandId: string,
    payload: unknown,
    mutate: () => Result,
  ): Result {
    const execute = this.#database.transaction((): Result => {
      this.#assertAgentContext(context);
      const payloadHash = sha256(canonicalJson({ context, payload }));
      const existing = this.#database.prepare(`
        SELECT payload_hash, result_json FROM commands
         WHERE run_id=? AND actor_agent_id=? AND command_id=?
      `).get(context.coordinationRunId, context.agentId, commandId);
      if (isRow(existing)) {
        if (text(existing, "payload_hash") !== payloadHash) {
          throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "resource command ID was reused with changed input");
        }
        return JSON.parse(text(existing, "result_json")) as Result;
      }
      const result = mutate();
      this.#database.prepare(`
        INSERT INTO commands(run_id, actor_agent_id, command_id, payload_hash, result_json, created_at)
        VALUES (?, ?, ?, ?, ?, ?)
      `).run(
        context.coordinationRunId,
        context.agentId,
        commandId,
        payloadHash,
        canonicalJson(result),
        this.#clock(),
      );
      return result;
    });
    return execute();
  }

  #assertReservationContext(context: AuthenticatedAgentContext, reservation: Row): void {
    if (
      text(reservation, "project_session_id") !== context.projectSessionId ||
      reservation.coordination_run_id !== context.coordinationRunId
    ) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "reservation is outside the authenticated run");
    }
  }

  #capacity(scopeId: string, units: readonly string[]): Record<string, ResourceDimensionProjection> {
    const result: Record<string, ResourceDimensionProjection> = {};
    for (const unit of units) {
      const dimension = this.#dimension(scopeId, unit);
      const used = integer(dimension, "used");
      const reserved = integer(dimension, "reserved");
      const limit = integer(dimension, "limit_value");
      result[unit] = integer(dimension, "usage_unknown") === 1
        ? { unknown: true, used: null, reserved, remaining: null }
        : { unknown: false, used, reserved, remaining: limit - used - reserved };
    }
    return result;
  }

  #amounts(reservation: Row): ResourceAmounts {
    const value: unknown = JSON.parse(text(reservation, "amounts_json"));
    if (!isRow(value)) throw new Error("stored reservation amounts are invalid");
    const result: Record<string, number> = {};
    for (const [unit, amount] of Object.entries(value)) {
      if (typeof amount !== "number" || !Number.isSafeInteger(amount)) throw new Error("stored reservation amount is invalid");
      result[unit] = amount;
    }
    return result;
  }

  #scopeRow(scopeId: string): Row {
    return row(this.#database.prepare("SELECT * FROM resource_scopes WHERE scope_id=?").get(scopeId), "resource scope");
  }

  #dimension(scopeId: string, unit: string): Row {
    return row(this.#database.prepare(`
      SELECT * FROM resource_dimensions WHERE scope_id=? AND unit_key=?
    `).get(scopeId, unit), "resource dimension");
  }

  #reservationRow(reservationId: string): Row {
    return row(this.#database.prepare(`
      SELECT * FROM resource_reservations WHERE reservation_id=?
    `).get(reservationId), "resource reservation");
  }
}
