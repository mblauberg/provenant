import {
  assertChairMutationAuthority,
  parseScopedGate,
  type GateId,
  type GateOperationTarget,
  type FabricOperation,
  type BarrierId,
  type ChairMutationContext,
  type ScopedGate,
  type ScopedGateCheckRequest,
  type ScopedGateCheckResult,
  type ScopedGateCreateRequest,
  type ScopedGateResolveRequest,
  type TaskId,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { OperatorStore } from "../operator/store.js";
import { NotificationOutbox } from "../attention/outbox.js";
import {
  assertExactGateAttestationDigests,
  canonicalGateAttestationDigests,
  parseStoredAttestationDigests,
} from "./attestation-binding.js";
import {
  ProjectFabricCoreError,
  type AuthenticatedAgentContext,
  type AuthenticatedOperatorContext,
  type CoreServiceOptions,
} from "../project-session/contracts.js";
import {
  canonicalJson,
  integer,
  isRow,
  nullableText,
  row,
  sha256,
  text,
  timestampToMillis,
  type Row,
} from "../persistence/row-codec.js";

export type DependencyEdge = Readonly<{ taskId: string; dependencyTaskId: string }>;

export type DependencyMutationRequest = Readonly<{
  commandId: string;
  expectedRevision: number;
  edges: readonly DependencyEdge[];
}>;

export type BarrierBindingRequest = Readonly<{
  commandId: string;
  gateId: string;
  barrierId: string;
  expectedGateRevision: number;
}>;

export type SetTaskDependenciesRequest = Readonly<{
  commandId: string;
  expectedRevision: number;
  taskId: string;
  dependencyTaskIds: readonly string[];
}>;

type InternalChairCommand = Readonly<{
  commandId: string;
  expectedRevision: number;
}>;
type ChairCommand = InternalChairCommand | ChairMutationContext;

type GateRow = Row & { gate_id: string };

export class ScopedGateStore {
  readonly #database: Database.Database;
  readonly #operatorStore: OperatorStore | undefined;
  readonly #notifications: NotificationOutbox;
  readonly #clock: () => number;
  readonly #fault: (label: string) => void;

  constructor(options: CoreServiceOptions & { operatorStore?: OperatorStore }) {
    this.#database = options.database;
    this.#operatorStore = options.operatorStore;
    this.#clock = options.clock ?? Date.now;
    this.#fault = options.fault ?? (() => undefined);
    this.#notifications = new NotificationOutbox({
      database: this.#database,
      clock: this.#clock,
      fault: this.#fault,
    });
  }

  createGate(
    context: AuthenticatedAgentContext | AuthenticatedOperatorContext,
    request: ScopedGateCreateRequest,
  ): ScopedGate {
    this.#assertRequestContext(context, request.intent.projectSessionId, request.intent.coordinationRunId);
    if (request.origin === "operator") {
      if (!("operatorId" in context) || this.#operatorStore === undefined) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "operator gate creation requires the operator service");
      }
      const identity = this.#runIdentity(request.intent.coordinationRunId);
      return this.#operatorStore.executeCommand(
        context,
        request.command,
        {
          projectId: identity.projectId,
          projectSessionId: identity.projectSessionId,
          sessionGeneration: identity.sessionGeneration,
          requiredAction: "decide",
          commandPayload: { origin: request.origin, intent: request.intent },
        },
        () => ({ revision: identity.dependencyRevision, value: { dependencyRevision: identity.dependencyRevision } }),
        () => this.#insertGate(`operator:${context.operatorId}`, context.principalGeneration, request),
      );
    }
    if (!("agentId" in context)) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "chair gate creation requires an agent principal");
    }
    return this.#executeChairCommand(
      context,
      request.command,
      { operation: "scoped-gate:create", payload: request },
      () => this.#insertGate(`agent:${context.agentId}`, context.principalGeneration, request),
    );
  }

  resolveGate(context: AuthenticatedOperatorContext, request: ScopedGateResolveRequest): ScopedGate {
    if (this.#operatorStore === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "gate resolution requires the operator service");
    }
    const current = this.#gateRow(request.gateId);
    const identity = this.#runIdentity(text(current, "coordination_run_id"));
    return this.#operatorStore.executeCommand(
      context,
      request.command,
      {
        projectId: identity.projectId,
        projectSessionId: identity.projectSessionId,
        sessionGeneration: identity.sessionGeneration,
        requiredAction: "decide",
        commandPayload: {
          gateId: request.gateId,
          status: request.status,
          decisionEvidence: request.decisionEvidence,
        },
      },
      () => {
        const gate = this.getGate(request.gateId);
        return { revision: gate.revision, value: gate };
      },
      () => {
        const gate = this.getGate(request.gateId);
        if (gate.status !== "pending" && gate.status !== "deferred") {
          throw new ProjectFabricCoreError("CONFLICT", "gate is already resolved");
        }
        if (request.status === "approved" && this.#isFinalCloseGate(gate)) {
          this.#assertFinalCloseReady(gate);
        }
        const resolution = this.#resolution(context, request, gate);
        const remainsOpen = request.status === "deferred";
        this.#fault("gates:resolve:before-update");
        this.#database.prepare(`
          UPDATE scoped_gates
             SET status=?, resolved_by_operator_id=?, resolution_json=?,
                 revision=revision+1, updated_at=?
           WHERE gate_id=? AND revision=? AND status IN ('pending','deferred')
        `).run(
          request.status,
          remainsOpen ? null : context.operatorId,
          remainsOpen ? null : canonicalJson(resolution),
          this.#clock(),
          request.gateId,
          gate.revision,
        );
        this.#fault("gates:resolve:after-attention");
        if (!remainsOpen) {
          const membershipState = request.status === "cancelled" ? "abandoned" : "reconciled";
          const abandonmentReason = request.status === "cancelled" ? "gate source status cancelled" : null;
          const membership = this.#database.prepare(`
            UPDATE project_session_memberships
               SET state=?, abandoned_reason=?, revision=revision+1, updated_at=?
             WHERE project_session_id=? AND coordination_run_id=?
               AND member_kind='gate' AND member_id=? AND state='active'
          `).run(
            membershipState,
            abandonmentReason,
            this.#clock(),
            gate.projectSessionId,
            gate.coordinationRunId,
            gate.gateId,
          );
          if (membership.changes !== 1) {
            throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "terminal gate membership was not active");
          }
          this.#touchSessionMembership(gate.projectSessionId, 1);
        }
        return this.getGate(request.gateId);
      },
    );
  }

  getGate(gateId: string): ScopedGate {
    return this.#gateFromRow(this.#gateRow(gateId));
  }

  getGateByDedupe(
    context: AuthenticatedAgentContext | AuthenticatedOperatorContext,
    projectSessionId: string,
    coordinationRunId: string,
    dedupeKey: string,
  ): ScopedGate | undefined {
    this.#assertRequestContext(context, projectSessionId, coordinationRunId);
    const value = this.#database.prepare(`
      SELECT * FROM scoped_gates
       WHERE project_session_id=? AND coordination_run_id=? AND dedupe_key=?
    `).get(projectSessionId, coordinationRunId, dedupeKey);
    return isRow(value) ? this.#gateFromRow(value as GateRow) : undefined;
  }

  check(
    context: AuthenticatedAgentContext | AuthenticatedOperatorContext,
    request: ScopedGateCheckRequest,
  ): ScopedGateCheckResult {
    this.#assertRequestContext(context, request.projectSessionId, request.coordinationRunId);
    return this.checkAuthoritative(request);
  }

  checkAuthoritative(request: ScopedGateCheckRequest): ScopedGateCheckResult {
    const identity = this.#runIdentity(request.coordinationRunId);
    if (identity.projectSessionId !== request.projectSessionId) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "run is outside the requested project session");
    }
    if (identity.dependencyRevision !== request.dependencyRevision) {
      throw new ProjectFabricCoreError("STALE_REVISION", "dependency graph revision changed", {
        expected: request.dependencyRevision,
        actual: identity.dependencyRevision,
      });
    }
    if (request.enforcementPoint === "task-readiness") {
      this.#assertTaskTarget(request.coordinationRunId, request.taskId);
    } else if (request.enforcementPoint === "operation" && request.operationTarget.kind === "task") {
      this.#assertTaskTarget(request.coordinationRunId, request.operationTarget.taskId);
    }
    const open = this.#database.prepare(`
      SELECT * FROM scoped_gates
       WHERE project_session_id=? AND coordination_run_id=?
         AND status IN ('pending','deferred')
       ORDER BY gate_id
    `).all(request.projectSessionId, request.coordinationRunId).filter(isRow) as GateRow[];
    const checkedGateRevisions: Record<string, number> = {};
    const blockingGateIds: GateId[] = [];
    for (const gate of open) {
      const gateId = text(gate, "gate_id");
      checkedGateRevisions[gateId] = integer(gate, "revision");
      if (integer(gate, "dependency_revision") !== request.dependencyRevision) {
        throw new ProjectFabricCoreError("STALE_REVISION", "gate dependency binding is stale");
      }
      const points = this.#jsonStrings(gate, "enforcement_points_json");
      if (!points.includes(request.enforcementPoint)) continue;
      const blocked = request.enforcementPoint === "task-readiness"
        ? this.#database.prepare(`
            SELECT 1 FROM scoped_gate_tasks
             WHERE gate_id=? AND project_session_id=? AND run_id=? AND task_id=?
               AND bound_dependency_revision=?
          `).get(
            gateId,
            request.projectSessionId,
            request.coordinationRunId,
            request.taskId,
            request.dependencyRevision,
          ) !== undefined
        : request.enforcementPoint === "operation"
          ? this.#operationBlocked(gate, request.operationId, request.operationTarget, request.dependencyRevision)
          : this.#database.prepare(`
              SELECT 1 FROM scoped_gate_barriers WHERE gate_id=? AND barrier_id=?
            `).get(gateId, request.barrierId) !== undefined;
      if (blocked) blockingGateIds.push(gateId as GateId);
    }
    return blockingGateIds.length === 0
      ? { allowed: true, checkedGateRevisions }
      : { allowed: false, blockingGateIds, checkedGateRevisions };
  }

  #operationBlocked(
    gate: GateRow,
    operationId: FabricOperation,
    target: GateOperationTarget,
    dependencyRevision: number,
  ): boolean {
    const gateId = text(gate, "gate_id");
    if (this.#database.prepare(`
      SELECT 1 FROM scoped_gate_operations WHERE gate_id=? AND operation_id=?
    `).get(gateId, operationId) === undefined) return false;
    const scopeKind = text(gate, "scope_kind");
    if (scopeKind === "run" || scopeKind === "release") return true;
    if (target.kind !== "task") return false;
    return this.#database.prepare(`
      SELECT 1 FROM scoped_gate_tasks
       WHERE gate_id=? AND project_session_id=? AND run_id=? AND task_id=?
         AND bound_dependency_revision=?
    `).get(
      gateId,
      text(gate, "project_session_id"),
      text(gate, "coordination_run_id"),
      target.taskId,
      dependencyRevision,
    ) !== undefined;
  }

  #assertTaskTarget(runId: string, taskId: string): void {
    if (this.#database.prepare(`
      SELECT 1 FROM tasks WHERE run_id=? AND task_id=?
    `).get(runId, taskId) !== undefined) return;
    if (this.#database.prepare("SELECT 1 FROM tasks WHERE task_id=? LIMIT 1").get(taskId) !== undefined) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "gate operation task target belongs to another run");
    }
    throw new ProjectFabricCoreError("NOT_FOUND", "gate operation task target was not found");
  }

  bindBarrier(context: AuthenticatedAgentContext, request: BarrierBindingRequest): void {
    const gate = this.#gateRow(request.gateId);
    this.#assertRequestContext(
      context,
      text(gate, "project_session_id"),
      text(gate, "coordination_run_id"),
    );
    this.#executeChairCommand(
      context,
      { commandId: request.commandId, expectedRevision: request.expectedGateRevision },
      { operation: "scoped-gate:bind-barrier", payload: request },
      () => {
        this.#assertGateTopologyMutable(text(gate, "coordination_run_id"));
        const current = this.getGate(request.gateId);
        if (current.revision !== request.expectedGateRevision) {
          throw new ProjectFabricCoreError("STALE_REVISION", "gate revision changed");
        }
        if (!current.enforcementPoints.includes("scoped-barrier")) {
          throw new ProjectFabricCoreError("CONFLICT", "gate does not enforce scoped barriers");
        }
        this.#database.prepare(`
          INSERT INTO scoped_gate_barriers(gate_id, barrier_id) VALUES (?, ?)
          ON CONFLICT(gate_id, barrier_id) DO NOTHING
        `).run(request.gateId, request.barrierId);
        return { bound: true };
      },
      false,
    );
  }

  mutateDependencies(
    context: AuthenticatedAgentContext,
    request: DependencyMutationRequest,
  ): { dependencyRevision: number; edgeCount: number; bindingCount: number } {
    this.#assertChair(context, request);
    return this.#executeChairCommand(
      context,
      request,
      { operation: "dependencies:replace", payload: request.edges },
      () => {
        const identity = this.#runIdentity(context.coordinationRunId);
        this.#assertGateTopologyMutable(context.coordinationRunId);
        if (identity.dependencyRevision !== request.expectedRevision) {
          throw new ProjectFabricCoreError("STALE_REVISION", "dependency graph revision changed");
        }
        const edges = this.#validateEdges(context.coordinationRunId, request.edges);
        const targetRevision = identity.dependencyRevision + 1;
        const plannedBindings = this.#plannedOpenGateBindings(context.coordinationRunId, edges);
        const bindingCount = [...plannedBindings.values()].reduce((count, tasks) => count + tasks.length, 0);
        this.#database.prepare(`
          INSERT INTO dependency_mutation_guards(
            run_id, project_session_id, target_revision, expected_edge_count, expected_binding_count
          ) VALUES (?, ?, ?, ?, ?)
        `).run(
          context.coordinationRunId,
          context.projectSessionId,
          targetRevision,
          edges.length,
          bindingCount,
        );
        this.#database.prepare("DELETE FROM task_dependencies WHERE run_id=?")
          .run(context.coordinationRunId);
        const insertEdge = this.#database.prepare(`
          INSERT INTO task_dependencies(
            run_id, task_id, dependency_task_id, project_session_id, dependency_revision
          ) VALUES (?, ?, ?, ?, ?)
        `);
        for (const edge of edges) {
          insertEdge.run(
            context.coordinationRunId,
            edge.taskId,
            edge.dependencyTaskId,
            context.projectSessionId,
            targetRevision,
          );
        }
        this.#database.prepare(`
          UPDATE runs SET dependency_revision=? WHERE run_id=? AND dependency_revision=?
        `).run(targetRevision, context.coordinationRunId, request.expectedRevision);
        this.#fault("gates:dependency:after-edges");
        const openGates = this.#database.prepare(`
          SELECT gate_id FROM scoped_gates
           WHERE coordination_run_id=? AND status IN ('pending','deferred')
           ORDER BY gate_id
        `).all(context.coordinationRunId).filter(isRow);
        for (const gate of openGates) {
          const gateId = text(gate, "gate_id");
          this.#database.prepare("DELETE FROM scoped_gate_tasks WHERE gate_id=?").run(gateId);
          this.#insertGateTaskBindings(
            gateId,
            context.projectSessionId,
            context.coordinationRunId,
            plannedBindings.get(gateId) ?? [],
            targetRevision,
          );
          this.#database.prepare(`
            UPDATE scoped_gates
               SET dependency_revision=?, revision=revision+1, updated_at=?
             WHERE gate_id=?
          `).run(targetRevision, this.#clock(), gateId);
        }
        this.#fault("gates:dependency:after-rebind");
        this.#database.prepare("DELETE FROM dependency_mutation_guards WHERE run_id=?")
          .run(context.coordinationRunId);
        return { dependencyRevision: targetRevision, edgeCount: edges.length, bindingCount };
      },
    );
  }

  setTaskDependencies(
    context: AuthenticatedAgentContext,
    request: SetTaskDependenciesRequest,
  ): { dependencyRevision: number; edgeCount: number; bindingCount: number } {
    const otherEdges = this.#loadEdges(context.coordinationRunId)
      .filter((edge) => edge.taskId !== request.taskId);
    return this.mutateDependencies(context, {
      commandId: request.commandId,
      expectedRevision: request.expectedRevision,
      edges: [
        ...otherEdges,
        ...request.dependencyTaskIds.map((dependencyTaskId) => ({
          taskId: request.taskId,
          dependencyTaskId,
        })),
      ],
    });
  }

  #insertGate(
    createdByRef: string,
    principalGeneration: number,
    request: ScopedGateCreateRequest,
  ): ScopedGate {
    const intent = request.intent;
    const identity = this.#runIdentity(intent.coordinationRunId);
    const existing = this.#database.prepare(`
      SELECT * FROM scoped_gates
       WHERE project_session_id=? AND coordination_run_id=? AND dedupe_key=?
    `).get(intent.projectSessionId, intent.coordinationRunId, intent.dedupeKey);
    if (isRow(existing)) {
      const gate = this.#gateFromRow(existing as GateRow);
      if (!this.#sameIntent(gate, intent)) {
        throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "gate dedupe key was reused with changed intent");
      }
      if (gate.status === "pending" || gate.status === "deferred") {
        this.#ensureGateAttention(createdByRef, principalGeneration, identity, intent, gate.gateId);
      }
      return gate;
    }
    this.#assertGateTopologyMutable(intent.coordinationRunId);
    const expectedRevision = request.command.expectedRevision;
    if (identity.dependencyRevision !== expectedRevision) {
      throw new ProjectFabricCoreError("STALE_REVISION", "gate dependency revision changed");
    }
    const edges = this.#loadEdges(intent.coordinationRunId);
    const affected = this.#affectedTasks(intent.coordinationRunId, intent.scope, edges);
    const gateId = `gate_${sha256(`${intent.projectSessionId}\0${intent.coordinationRunId}\0${intent.dedupeKey}`).slice(0, 24)}`;
    const now = this.#clock();
    this.#database.prepare(`
      INSERT INTO scoped_gates(
        gate_id, project_session_id, coordination_run_id, dedupe_key, scope_kind,
        scope_task_id, dependency_revision, blocked_operation_ids_json,
        enforcement_points_json, question, reason, options_json, recommendation,
        consequences_json, evidence_refs_json, created_by_ref,
        expected_approver_ref, deadline, default_action, status, human_required,
        release_binding_json, revision, created_at, updated_at
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', 1, ?, 1, ?, ?)
    `).run(
      gateId,
      intent.projectSessionId,
      intent.coordinationRunId,
      intent.dedupeKey,
      intent.scope.kind,
      intent.scope.kind === "task" ? intent.scope.taskId
        : intent.scope.kind === "subtree" ? intent.scope.rootTaskId : null,
      identity.dependencyRevision,
      canonicalJson(intent.blockedOperationIds),
      canonicalJson(intent.enforcementPoints),
      intent.question,
      intent.reason,
      canonicalJson(intent.options),
      intent.recommendation,
      canonicalJson(intent.consequences),
      canonicalJson(intent.evidenceRefs),
      createdByRef,
      "authenticated-human-operator",
      intent.deadline === undefined ? null : timestampToMillis(intent.deadline),
      intent.default ?? null,
      intent.releaseBinding === undefined ? null : canonicalJson(intent.releaseBinding),
      now,
      now,
    );
    this.#insertGateTaskBindings(
      gateId,
      intent.projectSessionId,
      intent.coordinationRunId,
      affected,
      identity.dependencyRevision,
    );
    for (const operationId of intent.blockedOperationIds) {
      this.#database.prepare(`
        INSERT INTO scoped_gate_operations(gate_id, operation_id) VALUES (?, ?)
      `).run(gateId, operationId);
    }
    const membership = this.#database.prepare(`
      INSERT INTO project_session_memberships(
        project_session_id, coordination_run_id, member_kind, member_id,
        required, state, revision, created_at, updated_at
      ) VALUES (?, ?, 'gate', ?, 1, 'active', 1, ?, ?)
      ON CONFLICT(project_session_id, coordination_run_id, member_kind, member_id, member_adapter_id) DO NOTHING
    `).run(intent.projectSessionId, intent.coordinationRunId, gateId, now, now);
    if (membership.changes !== 1) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "new gate membership was not inserted");
    }
    this.#touchSessionMembership(intent.projectSessionId, 1);
    this.#ensureGateAttention(createdByRef, principalGeneration, identity, intent, gateId);
    this.#fault("gates:create:after-attention");
    this.#fault("gates:create:after-bindings");
    return this.getGate(gateId);
  }

  #ensureGateAttention(
    createdByRef: string,
    principalGeneration: number,
    identity: { projectId: string },
    intent: ScopedGateCreateRequest["intent"],
    gateId: string,
  ): void {
    const producer = {
      producerId: createdByRef,
      projectId: identity.projectId,
      projectSessionId: intent.projectSessionId,
      coordinationRunId: intent.coordinationRunId,
      principalGeneration,
    } as const;
    const attention = this.#notifications.upsertAttention(producer, {
      dedupeKey: `scoped-gate:${gateId}`,
      kind: "consequential-gate",
      severity: "critical",
      payload: {
        gateId,
        title: intent.question,
        priority: intent.scope.kind === "release" ? "safety-integrity" : "critical-path",
        duplicateCount: 1,
        summary: intent.reason,
      },
    });
    this.#notifications.enqueue(producer, {
      itemId: attention.itemId,
      expectedItemRevision: attention.revision,
      targetIntegration: "native-desktop",
    });
  }

  #insertGateTaskBindings(
    gateId: string,
    projectSessionId: string,
    runId: string,
    affected: readonly { taskId: string; kind: "direct" | "descendant" }[],
    dependencyRevision: number,
  ): void {
    const statement = this.#database.prepare(`
      INSERT INTO scoped_gate_tasks(
        gate_id, project_session_id, run_id, task_id, binding_kind, bound_dependency_revision
      ) VALUES (?, ?, ?, ?, ?, ?)
    `);
    for (const binding of affected) {
      statement.run(gateId, projectSessionId, runId, binding.taskId, binding.kind, dependencyRevision);
    }
  }

  #plannedOpenGateBindings(
    runId: string,
    edges: readonly DependencyEdge[],
  ): Map<string, { taskId: string; kind: "direct" | "descendant" }[]> {
    const result = new Map<string, { taskId: string; kind: "direct" | "descendant" }[]>();
    const gates = this.#database.prepare(`
      SELECT gate_id, scope_kind, scope_task_id FROM scoped_gates
       WHERE coordination_run_id=? AND status IN ('pending','deferred')
       ORDER BY gate_id
    `).all(runId).filter(isRow);
    for (const gate of gates) {
      const kind = text(gate, "scope_kind");
      const scope = kind === "task"
        ? { kind: "task" as const, taskId: text(gate, "scope_task_id") as TaskId }
        : kind === "subtree"
          ? { kind: "subtree" as const, rootTaskId: text(gate, "scope_task_id") as TaskId }
          : kind === "run"
            ? { kind: "run" as const }
            : { kind: "release" as const };
      result.set(text(gate, "gate_id"), this.#affectedTasks(runId, scope, edges));
    }
    return result;
  }

  #affectedTasks(
    runId: string,
    scope: ScopedGateCreateRequest["intent"]["scope"],
    edges: readonly DependencyEdge[],
  ): { taskId: string; kind: "direct" | "descendant" }[] {
    if (scope.kind === "release") return [];
    if (scope.kind === "task") return [{ taskId: scope.taskId, kind: "direct" }];
    if (scope.kind === "run") {
      return this.#database.prepare("SELECT task_id FROM tasks WHERE run_id=? ORDER BY task_id")
        .all(runId).filter(isRow).map((value) => ({ taskId: text(value, "task_id"), kind: "direct" as const }));
    }
    const reverse = new Map<string, string[]>();
    for (const edge of edges) {
      const descendants = reverse.get(edge.dependencyTaskId) ?? [];
      descendants.push(edge.taskId);
      reverse.set(edge.dependencyTaskId, descendants);
    }
    const visited = new Set<string>([scope.rootTaskId]);
    const queue = [scope.rootTaskId as string];
    while (queue.length > 0) {
      const current = queue.shift();
      if (current === undefined) break;
      for (const descendant of reverse.get(current) ?? []) {
        if (!visited.has(descendant)) {
          visited.add(descendant);
          queue.push(descendant);
        }
      }
    }
    return [...visited].sort().map((taskId) => ({
      taskId,
      kind: taskId === scope.rootTaskId ? "direct" : "descendant",
    }));
  }

  #validateEdges(runId: string, values: readonly DependencyEdge[]): DependencyEdge[] {
    const known = new Set(
      this.#database.prepare("SELECT task_id FROM tasks WHERE run_id=?").all(runId)
        .filter(isRow).map((value) => text(value, "task_id")),
    );
    const keys = new Set<string>();
    const edges = values.map((edge) => ({ taskId: edge.taskId, dependencyTaskId: edge.dependencyTaskId }));
    for (const edge of edges) {
      if (!known.has(edge.taskId) || !known.has(edge.dependencyTaskId)) {
        throw new ProjectFabricCoreError("NOT_FOUND", "dependency edge references an unknown task");
      }
      if (edge.taskId === edge.dependencyTaskId) {
        throw new ProjectFabricCoreError("TASK_DEPENDENCY_BLOCKED", "task cannot depend on itself");
      }
      const key = `${edge.taskId}\0${edge.dependencyTaskId}`;
      if (keys.has(key)) throw new ProjectFabricCoreError("CONFLICT", "dependency edge is duplicated");
      keys.add(key);
    }
    const dependencies = new Map<string, string[]>();
    for (const edge of edges) dependencies.set(edge.taskId, [...(dependencies.get(edge.taskId) ?? []), edge.dependencyTaskId]);
    const visiting = new Set<string>();
    const visited = new Set<string>();
    const visit = (taskId: string): void => {
      if (visiting.has(taskId)) throw new ProjectFabricCoreError("TASK_DEPENDENCY_BLOCKED", "dependency graph contains a cycle");
      if (visited.has(taskId)) return;
      visiting.add(taskId);
      for (const dependency of dependencies.get(taskId) ?? []) visit(dependency);
      visiting.delete(taskId);
      visited.add(taskId);
    };
    for (const taskId of known) visit(taskId);
    return edges.sort((left, right) =>
      left.taskId.localeCompare(right.taskId) || left.dependencyTaskId.localeCompare(right.dependencyTaskId));
  }

  #loadEdges(runId: string): DependencyEdge[] {
    return this.#database.prepare(`
      SELECT task_id, dependency_task_id FROM task_dependencies
       WHERE run_id=? ORDER BY task_id, dependency_task_id
    `).all(runId).filter(isRow).map((value) => ({
      taskId: text(value, "task_id"),
      dependencyTaskId: text(value, "dependency_task_id"),
    }));
  }

  #executeChairCommand<Result>(
    context: AuthenticatedAgentContext,
    command: ChairCommand,
    identity: Readonly<{ operation: string; payload: unknown }>,
    mutate: () => Result,
    requireChairRevision = true,
  ): Result {
    const execute = this.#database.transaction((): Result => {
      this.#assertChair(context, command);
      const commandIdentity = {
        commandId: command.commandId,
        expectedRevision: command.expectedRevision,
        ...(this.#isProtocolChairCommand(command) ? {
          agentId: command.agentId,
          projectSessionId: command.projectSessionId,
          coordinationRunId: command.coordinationRunId,
          principalGeneration: command.principalGeneration,
          chairLeaseId: command.chairLeaseId,
          chairLeaseGeneration: command.chairLeaseGeneration,
          expectedRunRevision: command.expectedRunRevision,
        } : {}),
      };
      const payloadHash = sha256(canonicalJson({ command: commandIdentity, identity }));
      const existing = this.#database.prepare(`
        SELECT payload_hash, result_json FROM commands
         WHERE run_id=? AND actor_agent_id=? AND command_id=?
      `).get(context.coordinationRunId, context.agentId, command.commandId);
      if (isRow(existing)) {
        if (text(existing, "payload_hash") !== payloadHash) {
          throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "chair command ID was reused with changed input");
        }
        return JSON.parse(text(existing, "result_json")) as Result;
      }
      if (requireChairRevision) {
        const current = this.#runIdentity(context.coordinationRunId).dependencyRevision;
        if (current !== command.expectedRevision) {
          throw new ProjectFabricCoreError("STALE_REVISION", "chair command revision changed");
        }
      }
      const result = mutate();
      this.#database.prepare(`
        INSERT INTO commands(run_id, actor_agent_id, command_id, payload_hash, result_json, created_at)
        VALUES (?, ?, ?, ?, ?, ?)
      `).run(
        context.coordinationRunId,
        context.agentId,
        command.commandId,
        payloadHash,
        canonicalJson(result),
        this.#clock(),
      );
      return result;
    });
    return execute();
  }

  #assertChair(context: AuthenticatedAgentContext, command?: ChairCommand): void {
    const lease = this.#activeChairLease(context);
    const status = text(lease, "status");
    const compatibilityCommand = command !== undefined && !this.#isProtocolChairCommand(command);
    if (
      text(lease, "holder_agent_id") !== context.agentId ||
      (status !== "active" && !(status === "frozen" && compatibilityCommand))
    ) {
      throw new ProjectFabricCoreError("TASK_NOT_OWNER", "authenticated agent is not the active chair");
    }
    if (command !== undefined && this.#isProtocolChairCommand(command)) {
      const run = row(this.#database.prepare(`
        SELECT revision FROM runs WHERE run_id=?
      `).get(context.coordinationRunId), "coordination run");
      try {
        assertChairMutationAuthority(command, {
          agentId: context.agentId,
          projectSessionId: context.projectSessionId,
          coordinationRunId: context.coordinationRunId,
          principalGeneration: context.principalGeneration,
          chairLeaseId: text(lease, "lease_id") as ChairMutationContext["chairLeaseId"],
          chairLeaseGeneration: integer(lease, "generation"),
          runRevision: integer(run, "revision"),
        });
      } catch (error: unknown) {
        throw new ProjectFabricCoreError(
          "STALE_LEASE_GENERATION",
          error instanceof Error ? error.message : String(error),
        );
      }
    }
  }

  #isProtocolChairCommand(command: ChairCommand): command is ChairMutationContext {
    return "chairLeaseId" in command;
  }

  #activeChairLease(context: AuthenticatedAgentContext): Row {
    return row(this.#database.prepare(`
      SELECT lease_id, holder_agent_id, generation, status
        FROM run_chair_leases
       WHERE project_session_id=? AND run_id=? AND generation=(
         SELECT chair_generation FROM runs WHERE run_id=?
       )
    `).get(context.projectSessionId, context.coordinationRunId, context.coordinationRunId), "chair lease");
  }

  #assertRequestContext(
    context: AuthenticatedAgentContext | AuthenticatedOperatorContext,
    projectSessionId: string,
    runId: string,
  ): void {
    const identity = this.#runIdentity(runId);
    if (identity.projectSessionId !== projectSessionId) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "run is outside the requested project session");
    }
    if ("agentId" in context) {
      if (context.projectSessionId !== projectSessionId || context.coordinationRunId !== runId) {
        throw new ProjectFabricCoreError("WRONG_PROJECT", "agent context is bound to another run");
      }
    } else if (context.projectId !== identity.projectId) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "operator context is bound to another project");
    }
  }

  #runIdentity(runId: string): {
    projectId: string;
    projectSessionId: string;
    sessionGeneration: number;
    dependencyRevision: number;
    sessionState: string;
  } {
    const value = row(this.#database.prepare(`
      SELECT s.project_id, s.project_session_id, s.generation, s.state,
             r.dependency_revision
        FROM runs r JOIN project_sessions s ON s.project_session_id=r.project_session_id
       WHERE r.run_id=?
    `).get(runId), "coordination run");
    return {
      projectId: text(value, "project_id"),
      projectSessionId: text(value, "project_session_id"),
      sessionGeneration: integer(value, "generation"),
      dependencyRevision: integer(value, "dependency_revision"),
      sessionState: text(value, "state"),
    };
  }

  #assertGateTopologyMutable(runId: string): void {
    const identity = this.#runIdentity(runId);
    if (["quiescing", "awaiting_acceptance", "closed", "cancelled"].includes(identity.sessionState)) {
      throw new ProjectFabricCoreError(
        "LIFECYCLE_PRECONDITION_FAILED",
        "project-session gate and dependency topology is frozen",
      );
    }
  }

  #gateRow(gateId: string): GateRow {
    return row(this.#database.prepare("SELECT * FROM scoped_gates WHERE gate_id=?").get(gateId), "scoped gate") as GateRow;
  }

  #gateFromRow(value: GateRow): ScopedGate {
    const kind = text(value, "scope_kind");
    const scopeTaskId = nullableText(value, "scope_task_id");
    const scope = kind === "task"
      ? { kind, taskId: scopeTaskId }
      : kind === "subtree"
        ? { kind, rootTaskId: scopeTaskId }
        : { kind };
    const bindings = this.#database.prepare(`
      SELECT task_id FROM scoped_gate_tasks WHERE gate_id=? ORDER BY task_id
    `).all(text(value, "gate_id")).filter(isRow).map((binding) => text(binding, "task_id"));
    const resolutionJson = nullableText(value, "resolution_json");
    const releaseJson = nullableText(value, "release_binding_json");
    const deadline = value.deadline === null ? undefined : new Date(integer(value, "deadline")).toISOString();
    const defaultAction = nullableText(value, "default_action");
    return parseScopedGate({
      gateId: text(value, "gate_id"),
      projectSessionId: text(value, "project_session_id"),
      coordinationRunId: text(value, "coordination_run_id"),
      scope,
      affectedTaskIds: bindings,
      dependencyRevision: integer(value, "dependency_revision"),
      blockedOperationIds: this.#jsonStrings(value, "blocked_operation_ids_json"),
      enforcementPoints: this.#jsonStrings(value, "enforcement_points_json"),
      question: text(value, "question"),
      reason: text(value, "reason"),
      options: this.#jsonStrings(value, "options_json"),
      recommendation: text(value, "recommendation"),
      consequences: this.#jsonStrings(value, "consequences_json"),
      evidenceRefs: JSON.parse(text(value, "evidence_refs_json")),
      revision: integer(value, "revision"),
      createdByRef: text(value, "created_by_ref"),
      expectedApproverRef: text(value, "expected_approver_ref"),
      ...(deadline === undefined ? {} : { deadline }),
      ...(defaultAction === null ? {} : { default: defaultAction }),
      status: text(value, "status"),
      ...(resolutionJson === null ? {} : { resolution: JSON.parse(resolutionJson) }),
      ...(releaseJson === null ? {} : { releaseBinding: JSON.parse(releaseJson) }),
    });
  }

  #jsonStrings(value: Row, field: string): string[] {
    const parsed: unknown = JSON.parse(text(value, field));
    if (!Array.isArray(parsed) || !parsed.every((item) => typeof item === "string")) {
      throw new Error(`${field} is not a string array`);
    }
    return parsed;
  }

  #touchSessionMembership(projectSessionId: string, changes: number): void {
    if (changes === 0) return;
    const updated = this.#database.prepare(`
      UPDATE project_sessions
         SET membership_revision=membership_revision+1,
             revision=revision+1,
             updated_at=?
       WHERE project_session_id=?
    `).run(this.#clock(), projectSessionId);
    if (updated.changes !== 1) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "gate membership owner session was not found");
    }
  }

  #isFinalCloseGate(gate: ScopedGate): boolean {
    return gate.scope.kind === "run" &&
      gate.enforcementPoints.includes("operation") &&
      gate.blockedOperationIds.includes("fabric.v1.project-session.close");
  }

  #assertFinalCloseReady(gate: ScopedGate): void {
    const lifecycle = row(this.#database.prepare(`
      SELECT session.state AS session_state, run.lifecycle_state AS run_state,
             run.dependency_revision
        FROM project_sessions session
        JOIN runs run ON run.project_session_id=session.project_session_id
       WHERE session.project_session_id=? AND run.run_id=?
    `).get(gate.projectSessionId, gate.coordinationRunId), "final-close lifecycle");
    if (text(lifecycle, "session_state") !== "quiescing" || text(lifecycle, "run_state") !== "quiescing") {
      throw new ProjectFabricCoreError(
        "LIFECYCLE_PRECONDITION_FAILED",
        "final-close approval requires a quiescing session and run",
      );
    }
    if (integer(lifecycle, "dependency_revision") !== gate.dependencyRevision) {
      throw new ProjectFabricCoreError("STALE_REVISION", "final-close gate dependency binding changed");
    }
    const blockers: ReadonlyArray<readonly [string, string]> = [
      ["task", `SELECT 1 FROM tasks task JOIN runs run ON run.run_id=task.run_id
        WHERE run.project_session_id=? AND task.state NOT IN ('complete','cancelled','degraded') LIMIT 1`],
      ["write lease", `SELECT 1 FROM leases lease JOIN runs run ON run.run_id=lease.run_id
        WHERE run.project_session_id=? AND lease.status IN ('active','quarantined') LIMIT 1`],
      ["task-owner lease", `SELECT 1 FROM task_owner_leases
        WHERE project_session_id=? AND status IN ('active','frozen') LIMIT 1`],
      ["provider action", `SELECT 1 FROM provider_actions action JOIN runs run ON run.run_id=action.run_id
        WHERE run.project_session_id=? AND action.status IN ('prepared','dispatched','accepted','ambiguous','quarantined') LIMIT 1`],
      ["required result", `SELECT 1 FROM result_deliveries
        WHERE project_session_id=? AND required=1 AND state NOT IN ('consumed','abandoned') LIMIT 1`],
      ["non-final gate", `SELECT 1 FROM scoped_gates gate
        WHERE gate.project_session_id=? AND gate.status IN ('pending','deferred')
          AND NOT EXISTS (
            SELECT 1 FROM scoped_gate_operations operation
             WHERE operation.gate_id=gate.gate_id
               AND operation.operation_id='fabric.v1.project-session.close'
          ) LIMIT 1`],
      ["barrier", `SELECT 1 FROM barriers barrier JOIN runs run ON run.run_id=barrier.run_id
        WHERE run.project_session_id=? AND barrier.state<>'closed' LIMIT 1`],
      ["artifact obligation", `SELECT 1 FROM task_expected_artifacts expected
        JOIN runs run ON run.run_id=expected.run_id
        JOIN tasks task ON task.run_id=expected.run_id AND task.task_id=expected.task_id
        WHERE run.project_session_id=? AND task.state NOT IN ('cancelled','degraded') AND NOT EXISTS (
          SELECT 1 FROM artifacts artifact WHERE artifact.run_id=expected.run_id
            AND artifact.task_id=expected.task_id AND artifact.relative_path=expected.relative_path
        ) LIMIT 1`],
      ["agent context", `SELECT 1 FROM agents agent JOIN runs run ON run.run_id=agent.run_id
        WHERE run.project_session_id=? AND agent.lifecycle='context-unreconciled' LIMIT 1`],
    ];
    for (const [label, sql] of blockers) {
      if (this.#database.prepare(sql).get(gate.projectSessionId) !== undefined) {
        throw new ProjectFabricCoreError("BARRIER_PRECONDITION_FAILED", `${label} blocks final-close approval`);
      }
    }
    if (this.#database.prepare(`
      SELECT 1 FROM project_session_memberships membership
       WHERE membership.project_session_id=? AND membership.required=1 AND membership.state='active'
         AND NOT (
           membership.member_kind='coordination-run' AND membership.member_id=membership.coordination_run_id
         )
         AND NOT (
           membership.member_kind='lease' AND EXISTS (
             SELECT 1 FROM runs run
              WHERE run.project_session_id=membership.project_session_id
                AND run.run_id=membership.coordination_run_id
                AND run.chair_lease_id=membership.member_id
           )
         )
         AND NOT (
           membership.member_kind='gate' AND EXISTS (
             SELECT 1 FROM scoped_gates final_gate
              WHERE final_gate.project_session_id=membership.project_session_id
                AND final_gate.coordination_run_id=membership.coordination_run_id
                AND final_gate.gate_id=membership.member_id
                AND final_gate.scope_kind='run'
                AND final_gate.status IN ('pending','deferred')
                AND EXISTS (
                  SELECT 1 FROM scoped_gate_operations operation
                   WHERE operation.gate_id=final_gate.gate_id
                     AND operation.operation_id='fabric.v1.project-session.close'
                )
           )
         )
       LIMIT 1
    `).get(gate.projectSessionId) !== undefined) {
      throw new ProjectFabricCoreError("BARRIER_PRECONDITION_FAILED", "required non-final membership blocks close approval");
    }
    if (this.#database.prepare(`
      SELECT 1 FROM operator_effect_custody
       WHERE project_session_id=? AND state IN ('prepared','dispatching','conflict','ambiguous','quarantined','failed')
         AND operation<>'project-session-drain'
       LIMIT 1
    `).get(gate.projectSessionId) !== undefined) {
      throw new ProjectFabricCoreError("BARRIER_PRECONDITION_FAILED", "unrelated operator effect blocks close approval");
    }
  }

  #sameIntent(gate: ScopedGate, intent: ScopedGateCreateRequest["intent"]): boolean {
    const projected = {
      projectSessionId: gate.projectSessionId,
      coordinationRunId: gate.coordinationRunId,
      scope: gate.scope,
      blockedOperationIds: gate.blockedOperationIds,
      enforcementPoints: gate.enforcementPoints,
      question: gate.question,
      reason: gate.reason,
      options: gate.options,
      recommendation: gate.recommendation,
      consequences: gate.consequences,
      evidenceRefs: gate.evidenceRefs,
      ...(gate.deadline === undefined ? {} : { deadline: gate.deadline }),
      ...(gate.default === undefined ? {} : { default: gate.default }),
      ...(gate.releaseBinding === undefined ? {} : { releaseBinding: gate.releaseBinding }),
    };
    const candidate = {
      projectSessionId: intent.projectSessionId,
      coordinationRunId: intent.coordinationRunId,
      scope: intent.scope,
      blockedOperationIds: intent.blockedOperationIds,
      enforcementPoints: intent.enforcementPoints,
      question: intent.question,
      reason: intent.reason,
      options: intent.options,
      recommendation: intent.recommendation,
      consequences: intent.consequences,
      evidenceRefs: intent.evidenceRefs,
      ...(intent.deadline === undefined ? {} : { deadline: intent.deadline }),
      ...(intent.default === undefined ? {} : { default: intent.default }),
      ...(intent.releaseBinding === undefined ? {} : { releaseBinding: intent.releaseBinding }),
    };
    return canonicalJson(projected) === canonicalJson(candidate);
  }

  #resolution(
    context: AuthenticatedOperatorContext,
    request: ScopedGateResolveRequest,
    gate: ScopedGate,
  ): Readonly<Record<string, unknown>> {
    const base = {
      operatorId: context.operatorId,
      decidedAt: new Date(this.#clock()).toISOString(),
      evidenceRefs: request.command.evidenceRefs,
    };
    if (request.decisionEvidence.kind === "typed-console") {
      if (request.decisionEvidence.confirmationCommandId !== request.command.commandId) {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed gate confirmation must bind the resolving command");
      }
      if (request.command.provenance.kind !== "console-direct-input") {
        throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "typed gate resolution requires direct Console input");
      }
      return { ...base, kind: "typed-console", confirmationCommandId: request.decisionEvidence.confirmationCommandId };
    }
    if (
      request.command.provenance.kind !== "attested-provider-input" ||
      request.command.provenance.attestationId !== request.decisionEvidence.attestationId ||
      request.command.provenance.integrationGeneration !== request.decisionEvidence.expectedIntegrationGeneration
    ) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "gate resolution command lacks the exact attested-input provenance");
    }
    const attestation = row(this.#database.prepare(`
      SELECT * FROM operator_input_attestations WHERE attestation_id=?
    `).get(request.decisionEvidence.attestationId), "operator input attestation");
    if (
      text(attestation, "project_session_id") !== gate.projectSessionId ||
      text(attestation, "gate_id") !== gate.gateId ||
      integer(attestation, "expected_gate_revision") !== gate.revision ||
      integer(attestation, "integration_generation") !== request.decisionEvidence.expectedIntegrationGeneration ||
      text(attestation, "operator_id") !== context.operatorId
    ) {
      throw new ProjectFabricCoreError("STALE_REVISION", "input attestation no longer matches the gate");
    }
    if (request.command.provenance.integrationId !== text(attestation, "integration_id")) {
      throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "gate resolution integration provenance changed");
    }
    assertExactGateAttestationDigests(
      canonicalGateAttestationDigests({
        evidenceRefs: gate.evidenceRefs,
        ...(gate.releaseBinding === undefined ? {} : { releaseBinding: gate.releaseBinding }),
      }),
      parseStoredAttestationDigests(text(attestation, "artifact_digests_json")),
      "STALE_REVISION",
    );
    const interpreted = text(attestation, "interpreted_decision");
    const expectedStatus = interpreted === "approve" ? "approved"
      : interpreted === "reject" ? "rejected"
        : interpreted === "defer" ? "deferred" : "rejected";
    if (request.status !== expectedStatus) {
      throw new ProjectFabricCoreError("CONFLICT", "attested decision does not match requested gate status");
    }
    return {
      ...base,
      kind: "attested-input",
      attestationId: request.decisionEvidence.attestationId,
      integrationId: text(attestation, "integration_id"),
      integrationGeneration: integer(attestation, "integration_generation"),
    };
  }
}

function gateCheckIdentity(database: Database.Database, runId: string): {
  projectSessionId: string;
  dependencyRevision: number;
} {
  const value = row(database.prepare(`
    SELECT project_session_id, dependency_revision FROM runs WHERE run_id=?
  `).get(runId), "coordination run gate identity");
  return {
    projectSessionId: text(value, "project_session_id"),
    dependencyRevision: integer(value, "dependency_revision"),
  };
}

function assertGateCheckAllowed(result: ScopedGateCheckResult): void {
  if (result.allowed) return;
  throw new ProjectFabricCoreError(
    "GATE_BLOCKED",
    `scoped gate blocks this action: ${result.blockingGateIds.join(",")}`,
  );
}

export function assertScopedTaskReadinessAllowed(
  database: Database.Database,
  runId: string,
  taskId: string,
): void {
  const identity = gateCheckIdentity(database, runId);
  assertGateCheckAllowed(new ScopedGateStore({ database }).checkAuthoritative({
    projectSessionId: identity.projectSessionId as ScopedGateCheckRequest["projectSessionId"],
    coordinationRunId: runId as ScopedGateCheckRequest["coordinationRunId"],
    dependencyRevision: identity.dependencyRevision,
    enforcementPoint: "task-readiness",
    taskId: taskId as TaskId,
  }));
}

export function assertScopedOperationAllowed(
  database: Database.Database,
  runId: string,
  operationId: FabricOperation,
  operationTarget: GateOperationTarget,
): void {
  const identity = gateCheckIdentity(database, runId);
  assertGateCheckAllowed(new ScopedGateStore({ database }).checkAuthoritative({
    projectSessionId: identity.projectSessionId as ScopedGateCheckRequest["projectSessionId"],
    coordinationRunId: runId as ScopedGateCheckRequest["coordinationRunId"],
    dependencyRevision: identity.dependencyRevision,
    enforcementPoint: "operation",
    operationId,
    operationTarget,
  }));
}

export function assertScopedBarrierAllowed(
  database: Database.Database,
  runId: string,
  barrierId: string,
): void {
  const identity = gateCheckIdentity(database, runId);
  assertGateCheckAllowed(new ScopedGateStore({ database }).checkAuthoritative({
    projectSessionId: identity.projectSessionId as ScopedGateCheckRequest["projectSessionId"],
    coordinationRunId: runId as ScopedGateCheckRequest["coordinationRunId"],
    dependencyRevision: identity.dependencyRevision,
    enforcementPoint: "scoped-barrier",
    barrierId: barrierId as BarrierId,
  }));
}
