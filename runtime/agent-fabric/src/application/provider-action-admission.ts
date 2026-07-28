import type Database from "better-sqlite3";

import { FabricError } from "../errors.js";
import { digest, isRow } from "../persistence/row-codec.js";
import {
  assertProviderActionOwner,
  type ProviderActionCustodyOwner,
} from "./provider-action-owner.js";

export type ProviderActionRef = Readonly<{
  adapterId: string;
  actionId: string;
}>;

export type ProviderActionScope =
  | Readonly<{ kind: "run-action"; runId: string }>
  | Readonly<{ kind: "provider-smoke" }>;

export type ProviderActionPrincipal =
  | Readonly<{
      agentId: string;
      projectSessionId: string;
      coordinationRunId: string;
      principalGeneration: number;
    }>
  | Readonly<{
      operatorId: string;
      projectId: string;
      projectAuthorityGeneration: number;
      principalGeneration: number;
    }>
  | Readonly<{ kind: "integration"; integrationId: string; projectId: string }>
  | Readonly<{ kind: "daemon-owner"; ownerId: string; generation: number }>;

const providerActionTicketBrand: unique symbol = Symbol("provider-action-ticket");

export type ProviderActionTicket = Readonly<{
  actionRef: ProviderActionRef;
  scope: ProviderActionScope;
  actorPrincipalDigest: `sha256:${string}`;
  inputDigest: `sha256:${string}`;
  ownerDigest: `sha256:${string}`;
  disposition: "resolving" | "admitted";
  [providerActionTicketBrand]: true;
}>;

type PersistedPreflightFailure = Readonly<{
  name: string;
  message: string;
  code?: string;
  field?: string;
  details?: Readonly<Record<string, unknown>>;
}>;

export type ProviderActionInsert = Readonly<{
  runId: string;
  actionId: string;
  adapterId: string;
  operation: string;
  targetAgentId?: string | null;
  providerSessionGeneration?: number | null;
  turnLeaseGeneration?: number | null;
  identityHash: string;
  payloadHash: string;
  payloadJson: string;
  status: string;
  historyJson: string;
  executionCount: number;
  effectCount?: number;
  idempotencyProven?: boolean;
  resultJson?: string | null;
  taskId?: string | null;
  budgetAuthorityId?: string | null;
  budgetReservationJson?: string | null;
  budgetSettlementJson?: string | null;
  budgetState?: string | null;
  budgetStartedAt?: number | null;
  findingCapacityReservationDigest?: string | null;
  updatedAt: number;
}>;

type PreflightRequest = Readonly<{
  actionRef: ProviderActionRef;
  scope: ProviderActionScope;
  principal: ProviderActionPrincipal;
  canonicalInput: unknown;
}>;

type AgentPreflightRequest = Readonly<{
  runId: string;
  actorAgentId: string;
  actionRef: ProviderActionRef;
  canonicalInput: unknown;
}>;

type CoordinatorOptions = Readonly<{
  database: Database.Database;
  clock: () => number;
  fault?: (label: string) => void;
}>;

export class ProviderActionAdmissionTransactionError extends Error {
  constructor(cause: unknown) {
    super(cause instanceof Error ? cause.message : "provider action admission transaction failed", { cause });
    this.name = "ProviderActionAdmissionTransactionError";
    if (cause instanceof Error) {
      const record = cause as Error & { code?: unknown; field?: unknown; details?: unknown };
      if (typeof record.code === "string") Object.assign(this, { code: record.code });
      if (typeof record.field === "string") Object.assign(this, { field: record.field });
      if (isRow(record.details)) Object.assign(this, { details: record.details });
    }
  }
}

function textField(row: Record<string, unknown>, field: string): string {
  const value = row[field];
  if (typeof value !== "string") throw new Error(`${field} is not text`);
  return value;
}

function integerField(row: Record<string, unknown>, field: string): number {
  const value = row[field];
  if (typeof value !== "number" || !Number.isSafeInteger(value)) {
    throw new Error(`${field} is not an integer`);
  }
  return value;
}

function persistedFailure(error: unknown): PersistedPreflightFailure {
  if (!(error instanceof Error)) {
    return { name: "Error", message: String(error) };
  }
  const record = error as Error & {
    code?: unknown;
    field?: unknown;
    details?: unknown;
  };
  return {
    name: error.name,
    message: error.message,
    ...(typeof record.code === "string" ? { code: record.code } : {}),
    ...(typeof record.field === "string" ? { field: record.field } : {}),
    ...(isRow(record.details) ? { details: record.details } : {}),
  };
}

function replayFailure(value: unknown): never {
  if (!isRow(value) || typeof value.name !== "string" || typeof value.message !== "string") {
    throw new Error("stored provider action preflight failure is invalid");
  }
  const error = new Error(value.message);
  error.name = value.name;
  if (typeof value.code === "string") Object.assign(error, { code: value.code });
  if (typeof value.field === "string") Object.assign(error, { field: value.field });
  if (isRow(value.details)) Object.assign(error, { details: value.details });
  throw error;
}

export class ProviderActionAdmissionCoordinator {
  readonly #database: Database.Database;
  readonly #clock: () => number;
  readonly #fault: (label: string) => void;
  readonly #flights = new Map<string, Promise<unknown>>();

  constructor(options: CoordinatorOptions) {
    this.#database = options.database;
    this.#clock = options.clock;
    this.#fault = options.fault ?? (() => undefined);
  }

  preflightAgentAction(request: AgentPreflightRequest): ProviderActionTicket {
    const principalRow = this.#database.prepare(`
      SELECT run.project_session_id AS project_session_id,
             capability.principal_generation AS principal_generation
        FROM runs run
        JOIN capabilities capability
          ON capability.run_id=run.run_id AND capability.agent_id=?
       WHERE run.run_id=? AND capability.revoked_at IS NULL
         AND capability.expires_at>?
       ORDER BY capability.principal_generation DESC
       LIMIT 1
    `).get(request.actorAgentId, request.runId, this.#clock());
    if (!isRow(principalRow)) {
      throw new FabricError("CAPABILITY_FORBIDDEN", "provider action principal is not active");
    }
    const principal: ProviderActionPrincipal = {
      agentId: request.actorAgentId,
      projectSessionId: textField(principalRow, "project_session_id"),
      coordinationRunId: request.runId,
      principalGeneration: integerField(principalRow, "principal_generation"),
    };
    return this.preflight({
      actionRef: request.actionRef,
      scope: { kind: "run-action", runId: request.runId },
      principal,
      canonicalInput: request.canonicalInput,
    });
  }

  preflight(request: PreflightRequest): ProviderActionTicket {
    const actorPrincipalDigest = digest(request.principal);
    const inputDigest = digest(request.canonicalInput);
    const ownerDigest = digest({
      schemaVersion: 1,
      scope: request.scope,
      actionRef: request.actionRef,
      actorPrincipalDigest,
      inputDigest,
    });
    const disposition = this.#database.transaction((): "resolving" | "admitted" => {
      const existing = this.#database.prepare(`
        SELECT scope_kind,run_id,owner_digest,actor_principal_digest,input_digest,state,failure_json
          FROM provider_action_pair_preflights
         WHERE adapter_id=? AND action_id=?
      `).get(request.actionRef.adapterId, request.actionRef.actionId);
      if (existing === undefined) {
        const now = this.#clock();
        this.#database.prepare(`
          INSERT INTO provider_action_pair_preflights(
            adapter_id,action_id,scope_kind,run_id,owner_digest,actor_principal_digest,
            input_digest,state,created_at,updated_at
          ) VALUES (?,?,?,?,?,?,?,'resolving',?,?)
        `).run(
          request.actionRef.adapterId,
          request.actionRef.actionId,
          request.scope.kind,
          request.scope.kind === "run-action" ? request.scope.runId : null,
          ownerDigest,
          actorPrincipalDigest,
          inputDigest,
          now,
          now,
        );
        this.#fault("provider-action-admission:after-preflight-insert");
        return "resolving";
      }
      if (!isRow(existing)) throw new Error("provider action preflight row is invalid");
      const storedRunId = existing.run_id;
      const exactScope = textField(existing, "scope_kind") === request.scope.kind && (
        request.scope.kind === "run-action"
          ? storedRunId === request.scope.runId
          : storedRunId === null
      );
      const exact = exactScope &&
        textField(existing, "owner_digest") === ownerDigest &&
        textField(existing, "actor_principal_digest") === actorPrincipalDigest &&
        textField(existing, "input_digest") === inputDigest;
      if (!exact) {
        throw new FabricError(
          "ACTION_INPUT_CONFLICT",
          "provider action pair was reused with changed run, principal or input",
        );
      }
      const state = textField(existing, "state");
      if (state !== "resolving") {
        if (state === "admitted") return "admitted";
        const failureJson = textField(existing, "failure_json");
        replayFailure(JSON.parse(failureJson) as unknown);
      }
      return "resolving";
    }).immediate();
    return {
      actionRef: request.actionRef,
      scope: request.scope,
      actorPrincipalDigest,
      inputDigest,
      ownerDigest,
      disposition,
      [providerActionTicketBrand]: true,
    };
  }

  async join<T>(
    ticket: ProviderActionTicket,
    ownerWork: () => Promise<T>,
  ): Promise<Readonly<{ value: T; joined: boolean }>> {
    const key = `${ticket.actionRef.adapterId}\0${ticket.actionRef.actionId}`;
    const existing = this.#flights.get(key);
    if (existing !== undefined) return { value: await existing as T, joined: true };
    const work = ownerWork();
    this.#flights.set(key, work);
    try {
      return { value: await work, joined: false };
    } finally {
      if (this.#flights.get(key) === work) this.#flights.delete(key);
    }
  }

  admitUnroutedInCurrentTransaction<T>(
    ticket: ProviderActionTicket,
    action: ProviderActionInsert,
    expectedOwner: ProviderActionCustodyOwner,
    appendDependants?: () => T,
  ): T | undefined {
    if (!this.#database.inTransaction) {
      throw new Error("provider action admission requires an active transaction");
    }
    if (ticket.scope.kind !== "run-action") {
      throw new FabricError("ACTION_INPUT_CONFLICT", "provider action rows require run-action scope");
    }
    if (
      action.runId !== ticket.scope.runId || action.adapterId !== ticket.actionRef.adapterId ||
      action.actionId !== ticket.actionRef.actionId
    ) {
      throw new FabricError("ACTION_INPUT_CONFLICT", "provider action row does not match its admission ticket");
    }
    try {
      this.#database.prepare(`
        INSERT INTO provider_actions(
        run_id,action_id,adapter_id,operation,target_agent_id,
        provider_session_generation,turn_lease_generation,identity_hash,
        payload_hash,payload_json,status,history_json,execution_count,effect_count,
        idempotency_proven,result_json,updated_at,task_id,budget_authority_id,
        budget_reservation_json,budget_settlement_json,budget_state,budget_started_at,
        finding_capacity_reservation_digest
      ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
      `).run(
        action.runId,
      action.actionId,
      action.adapterId,
      action.operation,
      action.targetAgentId ?? null,
      action.providerSessionGeneration ?? null,
      action.turnLeaseGeneration ?? null,
      action.identityHash,
      action.payloadHash,
      action.payloadJson,
      action.status,
      action.historyJson,
      action.executionCount,
      action.effectCount ?? 0,
      action.idempotencyProven === true ? 1 : 0,
      action.resultJson ?? null,
      action.updatedAt,
      action.taskId ?? null,
      action.budgetAuthorityId ?? null,
      action.budgetReservationJson ?? null,
      action.budgetSettlementJson ?? null,
      action.budgetState ?? null,
      action.budgetStartedAt ?? null,
        action.findingCapacityReservationDigest ?? null,
      );
      this.#fault("provider-action-admission:after-action-insert");
      const dependantResult = appendDependants?.();
      this.#fault("provider-action-admission:after-dependants");
      assertProviderActionOwner(this.#database, {
        runId: action.runId,
        adapterId: action.adapterId,
        actionId: action.actionId,
      }, expectedOwner);
      this.#fault("provider-action-admission:after-owner-revalidation");
      this.admitInCurrentTransaction(ticket);
      this.#fault("provider-action-admission:after-final-cas");
      return dependantResult;
    } catch (error: unknown) {
      if (error instanceof ProviderActionAdmissionTransactionError) throw error;
      throw new ProviderActionAdmissionTransactionError(error);
    }
  }

  admitInCurrentTransaction(ticket: ProviderActionTicket): void {
    if (!this.#database.inTransaction) {
      throw new Error("provider action admission requires an active transaction");
    }
    if (ticket.scope.kind !== "run-action") {
      throw new FabricError("ACTION_INPUT_CONFLICT", "provider action rows require run-action scope");
    }
    const changed = this.#database.prepare(`
      UPDATE provider_action_pair_preflights
         SET state='admitted',updated_at=?
       WHERE adapter_id=? AND action_id=? AND run_id=? AND owner_digest=?
         AND actor_principal_digest=? AND input_digest=? AND state='resolving'
    `).run(
      this.#clock(),
      ticket.actionRef.adapterId,
      ticket.actionRef.actionId,
      ticket.scope.runId,
      ticket.ownerDigest,
      ticket.actorPrincipalDigest,
      ticket.inputDigest,
    );
    if (changed.changes !== 1) {
      throw new FabricError("DEDUPE_CONFLICT", "provider action admission ticket is stale or changed");
    }
  }

  release(ticket: ProviderActionTicket, failure: unknown): void {
    const failureJson = JSON.stringify(persistedFailure(failure));
    this.#database.transaction(() => {
      const dependant = ticket.scope.kind === "run-action" ? this.#database.prepare(`
        SELECT 1 FROM provider_actions
         WHERE run_id=? AND adapter_id=? AND action_id=?
      `).get(ticket.scope.runId, ticket.actionRef.adapterId, ticket.actionRef.actionId) : undefined;
      if (dependant !== undefined) {
        throw new FabricError("DEDUPE_CONFLICT", "admitted provider action cannot be released");
      }
      const changed = this.#database.prepare(`
        UPDATE provider_action_pair_preflights
           SET state='released',failure_json=?,updated_at=?
         WHERE adapter_id=? AND action_id=? AND scope_kind=? AND run_id IS ? AND owner_digest=?
           AND actor_principal_digest=? AND input_digest=? AND state='resolving'
      `).run(
        failureJson,
        this.#clock(),
        ticket.actionRef.adapterId,
        ticket.actionRef.actionId,
        ticket.scope.kind,
        ticket.scope.kind === "run-action" ? ticket.scope.runId : null,
        ticket.ownerDigest,
        ticket.actorPrincipalDigest,
        ticket.inputDigest,
      );
      if (changed.changes !== 1) {
        throw new FabricError("DEDUPE_CONFLICT", "provider action release ticket is stale or changed");
      }
    }).immediate();
  }
}
