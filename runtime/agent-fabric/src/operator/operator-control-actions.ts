import type { ArtifactRef, OperatorActionIntent, Sha256Digest } from "@local/agent-fabric-protocol";
import { parseArtifactRef, parseSha256Digest } from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError } from "../project-session/contracts.js";
import { canonicalJson, sha256 } from "../persistence/row-codec.js";
import { touchProjectSessionMembershipRevisionForRun } from "../project-session/membership-store.js";
import {
  readControlActiveTurns,
  type ActiveTurn,
} from "./control-eligibility.js";
import { resolveControlTarget } from "./control-target.js";
import {
  ProviderActionAdmissionTransactionError,
  type ProviderActionAdmissionCoordinator,
  type ProviderActionTicket,
} from "../application/provider-action-admission.js";
import {
  assertProviderActionOwner,
  ProviderActionOwnerError,
} from "../application/provider-action-owner.js";
import { OperatorControlFenceActions } from "./operator-control-fences.js";
import type { OperatorEffectOutcome, OperatorEffectRequest } from "./action-store.js";

export type Row = Record<string, unknown>;

export type EffectScope = {
  operatorId: string;
  projectId: string;
  projectSessionId: string;
  principalGeneration: number;
  operation: string;
};

export type OperatorControlAdapterPort = {
  capabilities(adapterId: string): Promise<unknown>;
  dispatch(
    adapterId: string,
    input: { actionId: string; operation: "interrupt" | "steer"; payload: Record<string, unknown> },
  ): Promise<unknown>;
  lookup(adapterId: string, actionId: string): Promise<unknown>;
};

export interface OperatorControlHostPort {
  effectScope(request: OperatorEffectRequest): EffectScope;
  storeCustodyOutcome(scope: EffectScope, commandId: string, outcome: OperatorEffectOutcome): void;
  /**
   * The generic operator-effect custody ID (`#custodyId` on the facade). Deliberately distinct
   * from `#controlCustodyId` below — the two are different persisted identities and must never
   * be folded (plan §4).
   */
  custodyId(scope: EffectScope, commandId: string): string;
  read(intent: OperatorActionIntent): Promise<unknown>;
}

type StoredControlAction = {
  runId: string;
  actionId: string;
  adapterId: string;
  operation: "interrupt" | "steer";
  status: "prepared" | "dispatched" | "accepted" | "terminal" | "ambiguous" | "quarantined";
  payloadHash: string;
  sourceActionId: string;
  sourcePayloadHash: string;
  agentId: string;
  resumeReference: string;
  providerSessionGeneration: number;
  turnLeaseGeneration: number;
  turnId: string;
};

type PlannedControlAction = {
  runId: string;
  agentId: string;
  sourceActionId: string;
  sourcePayloadHash: string;
  turnLeaseGeneration: number;
  providerSessionGeneration: number;
  providerSessionRef: string;
  turnId: string;
  adapterId: string;
  actionId: string;
  identityHash: string;
  payloadJson: string;
  ticket: ProviderActionTicket;
};

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

function nullableText(value: Row, field: string): string | null {
  const candidate = value[field];
  if (candidate !== null && typeof candidate !== "string") throw new Error(`${field} is not nullable text`);
  return candidate;
}

function unsupported(): never {
  throw new ProjectFabricCoreError("CAPABILITY_FORBIDDEN", "operator action runtime is unavailable for this intent");
}

function digestValue(value: unknown, path: string): Sha256Digest {
  return parseSha256Digest(`sha256:${sha256(canonicalJson(value))}`, path);
}

/**
 * Byte-moved from `ProductionOperatorActions` (S4g): the operator-control effect family —
 * interrupt/steer preflight, admission and dispatch (`#dispatchExternalControl*`); durable
 * action/owner proof and settlement (`#controlActions`, `#assertPersistedControlActionOwners`,
 * `#effectRef`, `#controlCustodyId`, `#persistAdapterResult`, `#sourceTupleCurrent`,
 * `#settleInterruptedSource`, `#quarantine`); and pause/resume/cancel fences (`#freeze`,
 * `#resume`, `#cancel`, `#cancelEffectFreeSession`, `#settleCancelledTask`). `dispatchControl`
 * and `observeControl` are the former `control` branches of the facade's `#dispatchOwned` and
 * `#observeOwned`, moved wholesale so the facade's generic dispatch/observe routers keep their
 * one-line dispatch to this class. `assertPersistedControlActionOwners` stays public because the
 * facade's shared effect-custody `#dispatch` still calls it directly around the async read
 * (`#332` owner-fence boundary, preserved at the same position relative to that read) until S4h
 * folds the shared envelope in. Preserves: preflight admission outside any transaction, revalidation
 * and action+owner binding inside the one immediate transaction, durable `dispatched` persisted
 * before adapter I/O, terminal proof and source-tuple validation before settlement, pause/cancel
 * only reached after terminal interrupt proof, and freeze/cancel/outcome atomicity.
 */
export class OperatorControlActions {
  readonly #database: Database.Database;
  readonly #clock: () => number;
  readonly #adapter: OperatorControlAdapterPort;
  readonly #providerActionAdmission: ProviderActionAdmissionCoordinator;
  readonly #retireVolatileProjectSession: ((projectSessionId: string) => void) | undefined;
  readonly #host: OperatorControlHostPort;
  readonly #fences: OperatorControlFenceActions;

  constructor(options: {
    database: Database.Database;
    clock: () => number;
    adapter: OperatorControlAdapterPort;
    providerActionAdmission: ProviderActionAdmissionCoordinator;
    host: OperatorControlHostPort;
    retireVolatileProjectSession?: (projectSessionId: string) => void;
  }) {
    this.#database = options.database;
    this.#clock = options.clock;
    this.#adapter = options.adapter;
    this.#providerActionAdmission = options.providerActionAdmission;
    this.#host = options.host;
    this.#retireVolatileProjectSession = options.retireVolatileProjectSession;
    this.#fences = new OperatorControlFenceActions({
      database: this.#database,
      clock: this.#clock,
      host: {
        effectScope: (request) => this.#host.effectScope(request),
        storeCustodyOutcome: (scope, commandId, outcome) => this.#host.storeCustodyOutcome(scope, commandId, outcome),
      },
      ...(this.#retireVolatileProjectSession === undefined
        ? {}
        : { retireVolatileProjectSession: this.#retireVolatileProjectSession }),
    });
  }

  async dispatchControl(
    request: OperatorEffectRequest & { intent: Extract<OperatorActionIntent, { kind: "control" }> },
  ): Promise<OperatorEffectOutcome> {
    const target = resolveControlTarget(this.#database, request.intent);
    if (request.intent.action === "resume") {
      return this.#fences.resume(request, target);
    }
    if (request.intent.action === "pause") {
      const activeTurns = readControlActiveTurns(this.#database, target);
      if (activeTurns.length > 0) {
        return await this.#dispatchExternalControl(request, activeTurns, "interrupt");
      }
      return this.#fences.freeze(target, `operator-pause:${request.commandId}`, request);
    }
    if (request.intent.action === "cancel") {
      const activeTurns = readControlActiveTurns(this.#database, target);
      if (activeTurns.length > 0) {
        return await this.#dispatchExternalControl(request, activeTurns, "interrupt");
      }
      return this.#fences.cancel(target, request);
    }
    if (request.intent.action === "steer") {
      const activeTurns = readControlActiveTurns(this.#database, target);
      if (activeTurns.length === 0) {
        return { status: "rejected", code: "state-changed", evidenceRefs: [] };
      }
      return await this.#dispatchExternalControl(request, activeTurns, "steer");
    }
    unsupported();
  }

  async observeControl(
    request: OperatorEffectRequest & {
      effectRef: ArtifactRef | null;
      intent: Extract<OperatorActionIntent, { kind: "control" }>;
    },
  ): Promise<OperatorEffectOutcome> {
    const stored = this.#controlActions(request);
    if (stored.length === 0) {
      return { status: "rejected", code: "state-changed", evidenceRefs: [] };
    }
    const expectedEffectRef = this.#effectRef(request, stored);
    if (
      request.effectRef === null ||
      request.effectRef.path !== expectedEffectRef.path ||
      request.effectRef.digest !== expectedEffectRef.digest
    ) {
      throw new ProjectFabricCoreError("STALE_REVISION", "operator control effect reference changed");
    }
    for (const action of stored) {
      if (action.status === "prepared") continue;
      if (action.status === "terminal") continue;
      if (action.status === "quarantined") {
        return { status: "ambiguous", effectRef: expectedEffectRef };
      }
      let result: unknown;
      try {
        result = await this.#adapter.lookup(action.adapterId, action.actionId);
      } catch (error: unknown) {
        if (error instanceof ProviderActionOwnerError) throw error;
        this.#quarantine(action);
        return { status: "ambiguous", effectRef: this.#effectRef(request, this.#controlActions(request)) };
      }
      this.#persistAdapterResult(action, result);
    }
    const reconciled = this.#controlActions(request);
    if (!reconciled.every((action) => action.status === "terminal")) {
      return { status: "ambiguous", effectRef: this.#effectRef(request, reconciled) };
    }
    const target = resolveControlTarget(this.#database, request.intent);
    if (request.intent.action === "pause") {
      return this.#fences.freeze(target, `operator-pause:${request.commandId}`, request);
    }
    if (request.intent.action === "steer") {
      return { status: "committed", afterState: { lifecycleState: "active", steered: true } };
    }
    if (request.intent.action === "cancel") return this.#fences.cancel(target, request);
    unsupported();
  }

  assertPersistedControlActionOwners(request: OperatorEffectRequest): void {
    const custodyId = this.#controlCustodyId(this.#host.effectScope(request), request);
    const candidates = this.#database.prepare(`
      SELECT action.run_id,action.adapter_id,action.action_id
        FROM provider_actions action
       WHERE json_extract(action.payload_json,'$.operatorCustodyId')=?
       ORDER BY action.run_id,action.adapter_id,action.action_id
    `).all(custodyId);
    for (const value of candidates) {
      const action = row(value, "persisted operator provider action");
      assertProviderActionOwner(this.#database, {
        runId: text(action, "run_id"),
        adapterId: text(action, "adapter_id"),
        actionId: text(action, "action_id"),
      }, "operator_control");
    }
  }

  async #dispatchExternalControl(
    request: OperatorEffectRequest,
    turns: readonly ActiveTurn[],
    operation: "interrupt" | "steer",
  ): Promise<OperatorEffectOutcome> {
    return await this.#dispatchExternalControlOwned(request, turns, operation);
  }

  async #dispatchExternalControlOwned(
    request: OperatorEffectRequest,
    turns: readonly ActiveTurn[],
    operation: "interrupt" | "steer",
  ): Promise<OperatorEffectOutcome> {
    if (turns.some((turn) => turn.turnId === null)) {
      return { status: "rejected", code: "state-changed", evidenceRefs: [] };
    }
    const planned: PlannedControlAction[] = [];
    const scope = this.#host.effectScope(request);
    const custodyId = this.#controlCustodyId(scope, request);
    const projectAuthorityGeneration = integer(row(this.#database.prepare(`
      SELECT authority_generation FROM projects WHERE project_id=?
    `).get(scope.projectId), "operator control project"), "authority_generation");
    for (const turn of turns) {
      const target = row(this.#database.prepare(`
        SELECT a.provider_session_ref, b.adapter_id,
               COALESCE(p.provider_session_generation, 1) AS provider_session_generation,
               action.result_json AS source_result_json
          FROM agents a
          JOIN agent_adapter_bindings b ON b.run_id=a.run_id AND b.agent_id=a.agent_id
          LEFT JOIN provider_state p ON p.run_id=a.run_id AND p.agent_id=a.agent_id
          JOIN provider_actions action
            ON action.run_id=a.run_id AND action.adapter_id=b.adapter_id AND action.action_id=?
         WHERE a.run_id=? AND a.agent_id=?
      `).get(turn.actionId, turn.runId, turn.agentId), "operator provider target");
      const adapterId = text(target, "adapter_id");
      const turnId = turn.turnId;
      if (turnId === null) throw new Error("operator provider turn ID changed after preflight");
      const payload = {
        operatorCustodyId: custodyId,
        operatorId: scope.operatorId,
        projectId: scope.projectId,
        projectSessionId: scope.projectSessionId,
        operatorIntentDigest: request.intentDigest,
        sourceActionId: turn.actionId,
        sourcePayloadHash: turn.sourcePayloadHash,
        agentId: turn.agentId,
        resumeReference: text(target, "provider_session_ref"),
        providerSessionGeneration: integer(target, "provider_session_generation"),
        turnLeaseGeneration: turn.turnLeaseGeneration,
        turnId,
        expectedTurnId: turnId,
        ...(request.intent.kind === "control" && request.intent.action === "steer"
          ? { instruction: request.intent.instruction, prompt: request.intent.instruction }
          : {}),
      };
      const actionId = `operator-${sha256(canonicalJson({
        schemaVersion: 1,
        operatorId: scope.operatorId,
        projectId: scope.projectId,
        projectSessionId: scope.projectSessionId,
        intentDigest: request.intentDigest,
        adapterId,
        runId: turn.runId,
        agentId: turn.agentId,
        providerSessionGeneration: payload.providerSessionGeneration,
        sourceActionId: turn.actionId,
        turnLeaseGeneration: turn.turnLeaseGeneration,
        turnId,
        operation,
      })).slice(0, 48)}`;
      const identityHash = sha256(canonicalJson({ adapterId, actionId, operation, payload }));
      const payloadJson = canonicalJson(payload);
      const ticket = this.#providerActionAdmission.preflight({
        actionRef: { adapterId, actionId },
        scope: { kind: "run-action", runId: turn.runId },
        principal: {
          operatorId: scope.operatorId,
          projectId: scope.projectId,
          projectAuthorityGeneration,
          principalGeneration: scope.principalGeneration,
        },
        canonicalInput: {
          schemaVersion: 1,
          scope: { kind: "run-action", runId: turn.runId },
          actionRef: { adapterId, actionId },
          operation,
          intent: request.intent,
          intentDigest: request.intentDigest,
          beforeStateDigest: request.beforeStateDigest,
          source: {
            actionId: turn.actionId,
            payloadHash: turn.sourcePayloadHash,
            agentId: turn.agentId,
            resumeReference: payload.resumeReference,
            providerSessionGeneration: payload.providerSessionGeneration,
            turnLeaseGeneration: turn.turnLeaseGeneration,
            turnId,
          },
          providerPayload: {
            operatorId: scope.operatorId,
            projectId: scope.projectId,
            projectSessionId: scope.projectSessionId,
            operatorIntentDigest: request.intentDigest,
            sourceActionId: turn.actionId,
            sourcePayloadHash: turn.sourcePayloadHash,
            agentId: turn.agentId,
            resumeReference: payload.resumeReference,
            providerSessionGeneration: payload.providerSessionGeneration,
            turnLeaseGeneration: turn.turnLeaseGeneration,
            turnId,
            expectedTurnId: turnId,
            ...(request.intent.kind === "control" && request.intent.action === "steer"
              ? { instruction: request.intent.instruction, prompt: request.intent.instruction }
              : {}),
          },
        },
      });
      planned.push({
        runId: turn.runId,
        agentId: turn.agentId,
        sourceActionId: turn.actionId,
        sourcePayloadHash: turn.sourcePayloadHash,
        turnLeaseGeneration: turn.turnLeaseGeneration,
        providerSessionGeneration: payload.providerSessionGeneration,
        providerSessionRef: payload.resumeReference,
        turnId,
        adapterId,
        actionId,
        identityHash,
        payloadJson,
        ticket,
      });
    }
    const anchor = planned[0];
    if (anchor === undefined) {
      return { status: "rejected", code: "state-changed", evidenceRefs: [] };
    }
    for (const plan of planned) {
      if (plan.ticket.disposition === "admitted") {
        assertProviderActionOwner(this.#database, {
          runId: plan.runId,
          adapterId: plan.adapterId,
          actionId: plan.actionId,
        }, "operator_control");
      }
    }
    return (await this.#providerActionAdmission.join(
      anchor.ticket,
      async () => await this.#dispatchExternalControlPairOwned(request, planned, operation),
    )).value;
  }

  async #dispatchExternalControlPairOwned(
    request: OperatorEffectRequest,
    planned: readonly PlannedControlAction[],
    operation: "interrupt" | "steer",
  ): Promise<OperatorEffectOutcome> {
    for (const plan of planned) {
      if (plan.ticket.disposition === "admitted") continue;
      const capabilities = await this.#adapter.capabilities(plan.adapterId);
      if (
        !isRow(capabilities) || capabilities.actionJournal !== true ||
        !Array.isArray(capabilities.operations) ||
        !capabilities.operations.includes(operation) ||
        !capabilities.operations.includes("lookup_action")
      ) {
        // Capability discovery is mutable provider state. Keep exact unresolved
        // pairs retryable instead of turning a transient observation into a
        // terminal replayed admission failure.
        return { status: "rejected", code: "state-changed", evidenceRefs: [] };
      }
    }
    const hasFreshAdmission = planned.some((plan) => plan.ticket.disposition === "resolving");
    if (hasFreshAdmission) {
      const operatorCustodyId = this.#host.custodyId(this.#host.effectScope(request), request.commandId);
      const rebound = await this.#host.read(request.intent);
      if (digestValue(rebound, "operatorControl.preDispatchState") !== request.beforeStateDigest) {
        const failure = new ProjectFabricCoreError("STALE_REVISION", "operator control state changed after preflight");
        for (const plan of planned) {
          if (plan.ticket.disposition === "resolving") this.#providerActionAdmission.release(plan.ticket, failure);
        }
        return { status: "rejected", code: "state-changed", evidenceRefs: [] };
      }
      try {
        this.#database.transaction(() => {
        for (const plan of planned) {
        const current = this.#database.prepare(`
          SELECT lease.action_id, lease.turn_lease_generation,
                 COALESCE(state.provider_session_generation, 1) AS provider_session_generation
            FROM provider_session_turn_leases lease
            LEFT JOIN provider_state state ON state.run_id=lease.run_id AND state.agent_id=lease.agent_id
           WHERE lease.run_id=? AND lease.agent_id=? AND lease.adapter_id=?
             AND lease.action_id=? AND lease.status='active'
        `).get(plan.runId, plan.agentId, plan.adapterId, plan.sourceActionId);
        if (
          !isRow(current) || current.action_id !== plan.sourceActionId ||
          current.turn_lease_generation !== plan.turnLeaseGeneration ||
          current.provider_session_generation !== plan.providerSessionGeneration
        ) {
          throw new ProjectFabricCoreError("STALE_GENERATION", "operator provider turn changed during preflight");
        }
        const existing = this.#database.prepare(`
          SELECT adapter_id, operation, identity_hash FROM provider_actions
           WHERE run_id=? AND adapter_id=? AND action_id=?
        `).get(plan.runId, plan.adapterId, plan.actionId);
        if (isRow(existing)) {
          if (
            existing.adapter_id !== plan.adapterId || existing.operation !== operation ||
            existing.identity_hash !== plan.identityHash
          ) {
            throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "operator provider action identity changed");
          }
          assertProviderActionOwner(this.#database, {
            runId: plan.runId,
            adapterId: plan.adapterId,
            actionId: plan.actionId,
          }, "operator_control");
          continue;
        }
        this.#providerActionAdmission.admitUnroutedInCurrentTransaction(plan.ticket, {
          runId: plan.runId,
          actionId: plan.actionId,
          adapterId: plan.adapterId,
          operation,
          targetAgentId: plan.agentId,
          providerSessionGeneration: plan.providerSessionGeneration,
          turnLeaseGeneration: plan.turnLeaseGeneration,
          identityHash: plan.identityHash,
          payloadHash: sha256(plan.payloadJson),
          payloadJson: plan.payloadJson,
          status: "prepared",
          historyJson: '["prepared"]',
          executionCount: 0,
          updatedAt: this.#clock(),
        }, "operator_control", () => {
          this.#database.prepare(`
            INSERT INTO operator_control_provider_action_bindings(
              custody_id,run_id,adapter_id,action_id,source_adapter_id,
              source_action_id,source_payload_hash,operation,target_agent_id,
              provider_session_ref,provider_session_generation,turn_lease_generation,
              turn_id,created_at
            ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)
          `).run(
            operatorCustodyId,
            plan.runId,
            plan.adapterId,
            plan.actionId,
            plan.adapterId,
            plan.sourceActionId,
            plan.sourcePayloadHash,
            operation,
            plan.agentId,
            plan.providerSessionRef,
            plan.providerSessionGeneration,
            plan.turnLeaseGeneration,
            plan.turnId,
            this.#clock(),
          );
        });
        }
        }).immediate();
      } catch (error: unknown) {
        if (!(error instanceof ProviderActionAdmissionTransactionError)) {
          for (const plan of planned) {
            if (plan.ticket.disposition === "resolving") this.#providerActionAdmission.release(plan.ticket, error);
          }
        }
        throw error;
      }
    }

    for (const action of this.#controlActions(request)) {
      if (action.status !== "prepared") continue;
      assertProviderActionOwner(this.#database, action, "operator_control");
      this.#database.prepare(`
        UPDATE provider_actions
           SET status='dispatched', history_json='["prepared","dispatched"]',
               execution_count=1, journal_revision=journal_revision+1, updated_at=?
         WHERE run_id=? AND adapter_id=? AND action_id=? AND status='prepared'
      `).run(this.#clock(), action.runId, action.adapterId, action.actionId);
      try {
        const stored = row(this.#database.prepare(`
          SELECT payload_json FROM provider_actions WHERE run_id=? AND adapter_id=? AND action_id=?
        `).get(action.runId, action.adapterId, action.actionId), "operator provider payload");
        const payload: unknown = JSON.parse(text(stored, "payload_json"));
        if (!isRow(payload)) throw new Error("operator provider payload is invalid");
        const result = await this.#adapter.dispatch(action.adapterId, {
          actionId: action.actionId,
          operation,
          payload,
        });
        this.#persistAdapterResult(action, result);
      } catch (error: unknown) {
        if (error instanceof ProviderActionOwnerError) throw error;
        assertProviderActionOwner(this.#database, action, "operator_control");
        this.#database.prepare(`
          UPDATE provider_actions
             SET status='ambiguous', history_json='["prepared","dispatched","ambiguous"]',
                 journal_revision=journal_revision+1, updated_at=?
           WHERE run_id=? AND adapter_id=? AND action_id=?
        `).run(this.#clock(), action.runId, action.adapterId, action.actionId);
      }
    }
    const completed = this.#controlActions(request);
    if (!completed.every((action) => action.status === "terminal")) {
      return { status: "ambiguous", effectRef: this.#effectRef(request, completed) };
    }
    if (request.intent.kind === "control" && request.intent.action === "pause") {
      const target = resolveControlTarget(this.#database, request.intent);
      return this.#fences.freeze(target, `operator-pause:${request.commandId}`, request);
    }
    if (request.intent.kind === "control" && request.intent.action === "cancel") {
      return this.#fences.cancel(resolveControlTarget(this.#database, request.intent), request);
    }
    return { status: "committed", afterState: { lifecycleState: "active", steered: true } };
  }

  #controlActions(request: OperatorEffectRequest): StoredControlAction[] {
    const custodyId = this.#host.custodyId(this.#host.effectScope(request), request.commandId);
    return this.#database.prepare(`
      SELECT action.run_id,action.action_id,action.adapter_id,action.operation,
             action.status,action.payload_hash,binding.source_action_id,
             binding.source_payload_hash,binding.target_agent_id,
             binding.provider_session_ref,binding.provider_session_generation,
             binding.turn_lease_generation,binding.turn_id
        FROM operator_control_provider_action_bindings binding
        JOIN provider_actions action
          ON action.run_id=binding.run_id
         AND action.adapter_id=binding.adapter_id
         AND action.action_id=binding.action_id
       WHERE binding.custody_id=?
       ORDER BY action.run_id,action.action_id
    `).all(custodyId).map((value) => {
      const stored = row(value, "operator provider action");
      const status = text(stored, "status");
      const operation = text(stored, "operation");
      if (operation !== "interrupt" && operation !== "steer") {
        throw new Error("operator provider action operation is invalid");
      }
      if (!["prepared", "dispatched", "accepted", "terminal", "ambiguous", "quarantined"].includes(status)) {
        throw new Error("operator provider action status is invalid");
      }
      assertProviderActionOwner(this.#database, {
        runId: text(stored, "run_id"),
        adapterId: text(stored, "adapter_id"),
        actionId: text(stored, "action_id"),
      }, "operator_control");
      return {
        runId: text(stored, "run_id"),
        actionId: text(stored, "action_id"),
        adapterId: text(stored, "adapter_id"),
        operation,
        status: status as StoredControlAction["status"],
        payloadHash: text(stored, "payload_hash"),
        sourceActionId: text(stored, "source_action_id"),
        sourcePayloadHash: text(stored, "source_payload_hash"),
        agentId: text(stored, "target_agent_id"),
        resumeReference: text(stored, "provider_session_ref"),
        providerSessionGeneration: integer(stored, "provider_session_generation"),
        turnLeaseGeneration: integer(stored, "turn_lease_generation"),
        turnId: text(stored, "turn_id"),
      };
    });
  }

  #effectRef(request: OperatorEffectRequest, actions: readonly StoredControlAction[]): ArtifactRef {
    const custodyId = this.#controlCustodyId(this.#host.effectScope(request), request);
    return parseArtifactRef({
      path: `.agent-fabric/operator-effects/${custodyId}.json`,
      digest: `sha256:${sha256(canonicalJson(actions))}`,
    }, "productionOperatorAction.effectRef");
  }

  #controlCustodyId(
    scope: EffectScope,
    request: OperatorEffectRequest,
  ): string {
    return `operator-control:${sha256(canonicalJson({
      schemaVersion: 1,
      operatorId: scope.operatorId,
      projectId: scope.projectId,
      projectSessionId: scope.projectSessionId,
      intentDigest: request.intentDigest,
    }))}`;
  }

  #persistAdapterResult(action: StoredControlAction, result: unknown): void {
    assertProviderActionOwner(this.#database, action, "operator_control");
    if (!isRow(result) || result.actionId !== action.actionId || result.operation !== action.operation ||
      result.payloadHash !== action.payloadHash ||
      !Array.isArray(result.history) ||
      !result.history.every((item) => typeof item === "string") ||
      !Number.isSafeInteger(result.executionCount) || result.executionCount !== 1 ||
      !Number.isSafeInteger(result.effectCount) || (result.effectCount as number) < 0) {
      this.#quarantine(action);
      return;
    }
    const status = result.status;
    if (status !== "terminal" && status !== "ambiguous" && status !== "dispatched" && status !== "accepted") {
      this.#quarantine(action);
      return;
    }
    if (status === "terminal") {
      const effect = result.result;
      const proved = result.effectCount === 1 && result.history.at(-1) === "terminal" && isRow(effect) &&
        effect.resumeReference === action.resumeReference && effect.turnId === action.turnId && (
        (action.operation === "interrupt" && effect.interrupted === true) ||
        (action.operation === "steer" && effect.steered === true)
      );
      if (!proved) {
        this.#quarantine(action);
        return;
      }
      if (!this.#sourceTupleCurrent(action)) {
        this.#quarantine(action);
        return;
      }
    }
    this.#database.transaction(() => {
      this.#database.prepare(`
        UPDATE provider_actions
           SET status=?, history_json=?, execution_count=?, effect_count=?, result_json=?,
               journal_revision=journal_revision+1, updated_at=?
         WHERE run_id=? AND adapter_id=? AND action_id=?
      `).run(
        status,
        canonicalJson(result.history),
        result.executionCount,
        result.effectCount,
        result.result === undefined ? null : canonicalJson(result.result),
        this.#clock(),
        action.runId,
        action.adapterId,
        action.actionId,
      );
      if (status === "terminal" && action.operation === "interrupt") {
        this.#settleInterruptedSource(action);
      }
    })();
  }

  #sourceTupleCurrent(action: StoredControlAction): boolean {
    const current = this.#database.prepare(`
      SELECT lease.status AS lease_status, source.adapter_id AS source_adapter_id,
             source.payload_hash AS source_payload_hash, source.status AS source_status,
             source.execution_count AS source_execution_count, source.result_json,
             agent.provider_session_ref, binding.adapter_id AS bound_adapter_id,
             COALESCE(provider.provider_session_generation, 1) AS current_provider_generation
        FROM provider_session_turn_leases lease
        JOIN provider_actions source
          ON source.run_id=lease.run_id AND source.adapter_id=lease.adapter_id AND source.action_id=lease.action_id
        JOIN agents agent ON agent.run_id=lease.run_id AND agent.agent_id=lease.agent_id
        JOIN agent_adapter_bindings binding ON binding.run_id=agent.run_id AND binding.agent_id=agent.agent_id
        LEFT JOIN provider_state provider ON provider.run_id=agent.run_id AND provider.agent_id=agent.agent_id
       WHERE lease.run_id=? AND lease.agent_id=? AND lease.action_id=?
         AND lease.provider_session_generation=? AND lease.turn_lease_generation=?
    `).get(
      action.runId,
      action.agentId,
      action.sourceActionId,
      action.providerSessionGeneration,
      action.turnLeaseGeneration,
    );
    if (!isRow(current) || current.lease_status !== "active" ||
      current.source_adapter_id !== action.adapterId || current.bound_adapter_id !== action.adapterId ||
      current.source_payload_hash !== action.sourcePayloadHash ||
      !["dispatched", "accepted", "ambiguous"].includes(String(current.source_status)) ||
      current.source_execution_count !== 1 || current.provider_session_ref !== action.resumeReference ||
      current.current_provider_generation !== action.providerSessionGeneration ||
      typeof current.result_json !== "string") return false;
    const sourceResult: unknown = JSON.parse(current.result_json);
    return isRow(sourceResult) && sourceResult.turnId === action.turnId;
  }

  #settleInterruptedSource(action: StoredControlAction): void {
    const source = row(this.#database.prepare(`
      SELECT history_json, result_json FROM provider_actions
       WHERE run_id=? AND adapter_id=? AND action_id=?
    `).get(action.runId, action.adapterId, action.sourceActionId), "interrupted source action");
    const historyValue: unknown = JSON.parse(text(source, "history_json"));
    if (!Array.isArray(historyValue) || !historyValue.every((item) => typeof item === "string")) {
      throw new Error("interrupted source action history is invalid");
    }
    const originalResult = nullableText(source, "result_json");
    const sourceOutcome = {
      interrupted: true,
      sourceActionId: action.sourceActionId,
      operatorActionId: action.actionId,
      resumeReference: action.resumeReference,
      providerSessionGeneration: action.providerSessionGeneration,
      turnLeaseGeneration: action.turnLeaseGeneration,
      turnId: action.turnId,
      originalResult: originalResult === null ? null : JSON.parse(originalResult) as unknown,
    };
    const lease = this.#database.prepare(`
      UPDATE provider_session_turn_leases SET status='released', updated_at=?
       WHERE run_id=? AND agent_id=? AND action_id=?
         AND provider_session_generation=? AND turn_lease_generation=? AND status='active'
    `).run(
      this.#clock(),
      action.runId,
      action.agentId,
      action.sourceActionId,
      action.providerSessionGeneration,
      action.turnLeaseGeneration,
    );
    if (lease.changes !== 1) throw new ProjectFabricCoreError("STALE_GENERATION", "source provider turn changed after interrupt");
    this.#database.prepare(`
      UPDATE provider_actions
         SET status='terminal', history_json=?, effect_count=1, idempotency_proven=1,
             result_json=?, journal_revision=journal_revision+1, updated_at=?
       WHERE run_id=? AND adapter_id=? AND action_id=? AND status<>'terminal'
    `).run(
      canonicalJson([...historyValue, "terminal"]),
      canonicalJson(sourceOutcome),
      this.#clock(),
      action.runId,
      action.adapterId,
      action.sourceActionId,
    );
    const membership = this.#database.prepare(`
      UPDATE project_session_memberships
         SET state='reconciled', revision=revision+1, updated_at=?
       WHERE coordination_run_id=? AND member_kind='provider-action'
         AND member_adapter_id=? AND member_id=? AND state='active'
    `).run(this.#clock(), action.runId, action.adapterId, action.sourceActionId);
    touchProjectSessionMembershipRevisionForRun(
      this.#database,
      action.runId,
      this.#clock(),
      membership.changes,
    );
  }

  #quarantine(action: StoredControlAction): void {
    assertProviderActionOwner(this.#database, action, "operator_control");
    this.#database.prepare(`
      UPDATE provider_actions
         SET status='quarantined', journal_revision=journal_revision+1, updated_at=?
       WHERE run_id=? AND adapter_id=? AND action_id=?
    `).run(this.#clock(), action.runId, action.adapterId, action.actionId);
  }
}
