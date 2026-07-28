import { createHash } from "node:crypto";

import { parseIdentifier, parseJsonValue, parseSha256Digest } from "@local/agent-fabric-protocol";
import type {
  AgentId,
  BarrierId,
  CoordinationRunId,
  JsonValue,
  MessageId,
  ProjectId,
  ProjectSessionId,
  ProviderActionId,
  Sha256Digest,
  TaskId,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { canonicalJson, integer, isRow, nullableText, row, text } from "../persistence/row-codec.js";
import { touchProjectSessionMembershipRevision } from "../project-session/membership-store.js";
import {
  ProviderActionAdmissionCoordinator,
  type ProviderActionTicket,
} from "../application/provider-action-admission.js";
import { assertProviderActionOwner } from "../application/provider-action-owner.js";

export type HerdrFabricPortsOptions = Readonly<{
  database: Database.Database;
  providerActionAdmission: ProviderActionAdmissionCoordinator;
  clock?: () => number;
}>;

type HerdrAppliedActionInput = Readonly<{
  actionId: ProviderActionId;
  projectId: ProjectId;
  projectSessionId: ProjectSessionId;
  coordinationRunId: CoordinationRunId;
  targetAgentId: AgentId | null;
  intent: JsonValue;
}>;

export type FabricSteerReference =
  | {
      kind: "task";
      projectId: ProjectId;
      projectSessionId: ProjectSessionId;
      coordinationRunId: CoordinationRunId;
      taskId: TaskId;
      expectedRevision: number;
    }
  | {
      kind: "message";
      projectId: ProjectId;
      projectSessionId: ProjectSessionId;
      coordinationRunId: CoordinationRunId;
      taskId: TaskId;
      messageId: MessageId;
      expectedRevision: number;
    };

export type FabricSteerReferenceValidation =
  | {
      status: "valid";
      referenceDigest: Sha256Digest;
      targetAgentId: AgentId;
      purpose: "steer" | "request" | "response";
      requiresAck: boolean;
      expectsResult: boolean;
      dependentBarrierId: BarrierId | null;
    }
  | {
      status: "rejected";
      code: "unknown-reference" | "stale-reference" | "scope-mismatch";
      reason: string;
    };

export type DirectSteerIntent = {
  kind: "steer.inject-fire-and-forget";
  targetAgentId: AgentId;
  paneRef: string;
  reference: FabricSteerReference;
  validatedReferenceDigest: Sha256Digest;
  prompt: string;
};

export type HerdrAppliedOperation =
  | "console.ensure-pane"
  | "agent.ensure-pane"
  | "panes.arrange"
  | "agent.project-metadata"
  | "attention.project"
  | "target.focus"
  | "agent.wake"
  | "notification.show";

export type HerdrEffectReceipt =
  | {
      status: "applied";
      operation: HerdrAppliedOperation;
      paneRef?: string;
      detail?: JsonValue;
    }
  | {
      status: "dispatched-unconfirmed";
      operation: "steer.inject-fire-and-forget";
      referenceValidation: "verified";
      deliveryEvidence: "none";
      canSatisfyExpectedResult: false;
      canCloseBarrier: false;
    };

export type HerdrActionRecord = {
  actionId: ProviderActionId;
  revision: number;
  intentDigest: Sha256Digest;
  status: "prepared" | "dispatched" | "ambiguous" | "terminal";
  receipt?: HerdrEffectReceipt;
  ambiguityReason?: string;
};

export type HerdrActionEvidence =
  | { status: "observed"; receipt: HerdrEffectReceipt }
  | { status: "absent" }
  | { status: "unknown" };

export type HerdrRecoverySummary = Readonly<{
  observed: number;
  terminal: number;
  ambiguous: number;
  prepared: number;
}>;

export const HERDR_CONTROL_ADAPTER_ID = "herdr-control-v1";
const ADAPTER_ID = HERDR_CONTROL_ADAPTER_ID;
const CLOSED_PREFLIGHT_FAILURE_CODES = new Set([
  "ACTION_INPUT_CONFLICT",
  "CAPABILITY_EXPIRED",
  "CAPABILITY_FORBIDDEN",
  "DEDUPE_CONFLICT",
  "PROTOCOL_INVALID",
  "WRONG_PROJECT",
]);
const SECRET_PATTERN = /\b(?:afb|afc|afop)_[A-Za-z0-9_-]{8,}|\bghp_[A-Za-z0-9_]{8,}|\bgithub_pat_[A-Za-z0-9_]{8,}/u;
const HERDR_APPLIED_OPERATIONS = new Set<HerdrAppliedOperation>([
  "console.ensure-pane",
  "agent.ensure-pane",
  "panes.arrange",
  "agent.project-metadata",
  "attention.project",
  "target.focus",
  "agent.wake",
  "notification.show",
]);

function isDeterministicClosedPreflightFailure(error: unknown): boolean {
  let current = error;
  for (let depth = 0; depth < 4; depth += 1) {
    if (current === null || typeof current !== "object") return false;
    const record = current as { code?: unknown; cause?: unknown };
    if (typeof record.code === "string") return CLOSED_PREFLIGHT_FAILURE_CODES.has(record.code);
    if (record.cause === undefined || record.cause === current) return false;
    current = record.cause;
  }
  return false;
}

/** Canonical daemon-side journal/reference seam for the optional Herdr package. */
export class HerdrFabricPorts {
  readonly #database: Database.Database;
  readonly #clock: () => number;
  readonly #providerActionAdmission: ProviderActionAdmissionCoordinator;

  constructor(options: HerdrFabricPortsOptions) {
    this.#database = options.database;
    this.#clock = options.clock ?? Date.now;
    this.#providerActionAdmission = options.providerActionAdmission;
  }

  async validateSteerReference(reference: FabricSteerReference): Promise<FabricSteerReferenceValidation> {
    return this.#validateSteerReference(reference);
  }

  async prepareDirectSteerAction(
    actionId: ProviderActionId,
    intent: DirectSteerIntent,
  ): Promise<HerdrActionRecord> {
    assertClosedDirectSteerIntent(intent);
    const parsedActionId = safeActionId(actionId);
    const payload = canonicalise(parseJsonValue(intent, "herdr.directSteer.intent"));
    assertNoCredentialLikeValue(payload, "Herdr direct-steer intent");
    const validation = this.#validateSteerReference(intent.reference);
    if (validation.status !== "valid") throw new TypeError(`Herdr direct-steer reference is ${validation.code}`);
    if (
      validation.referenceDigest !== intent.validatedReferenceDigest ||
      validation.targetAgentId !== intent.targetAgentId || validation.purpose !== "steer" ||
      validation.requiresAck || validation.expectsResult || validation.dependentBarrierId !== null
    ) throw new TypeError("Herdr direct-steer intent does not match authoritative Fabric validation");
    const preparedInput = {
      actionId: parsedActionId,
      runId: intent.reference.coordinationRunId,
      projectId: intent.reference.projectId,
      projectSessionId: intent.reference.projectSessionId,
      targetAgentId: intent.targetAgentId,
      operation: intent.kind,
      payload,
    } as const;
    const ticket = this.#preflightAction(preparedInput);
    try {
      return this.#database.transaction(() => {
      const rebound = this.#validateSteerReference(intent.reference);
      if (rebound.status !== "valid") throw new TypeError(`Herdr direct-steer reference is ${rebound.code}`);
      if (
        rebound.referenceDigest !== intent.validatedReferenceDigest ||
        rebound.targetAgentId !== intent.targetAgentId || rebound.purpose !== "steer" ||
        rebound.requiresAck || rebound.expectsResult || rebound.dependentBarrierId !== null
      ) throw new TypeError("Herdr direct-steer intent does not match authoritative Fabric validation");
      return this.#prepareAction(preparedInput, ticket);
      }).immediate();
    } catch (error: unknown) {
      this.#releaseProviderActionPreflightAfterRollback(ticket, error);
      throw error;
    }
  }

  /** Pure closed-family validation used even when the optional integration is disabled. */
  validateAction(input: HerdrAppliedActionInput): void {
    this.#validatedActionInput(input);
  }

  /** Internal daemon seam for already-authorised non-steer Herdr intents. */
  prepareAction(input: HerdrAppliedActionInput): HerdrActionRecord {
    const { actionId, intent, operation } = this.#validatedActionInput(input);
    const preparedInput = {
      actionId,
      runId: input.coordinationRunId,
      projectId: input.projectId,
      projectSessionId: input.projectSessionId,
      targetAgentId: input.targetAgentId,
      operation,
      payload: intent,
    } as const;
    const ticket = this.#preflightAction(preparedInput);
    try {
      return this.#database.transaction(() => this.#prepareAction(preparedInput, ticket)).immediate();
    } catch (error: unknown) {
      this.#releaseProviderActionPreflightAfterRollback(ticket, error);
      throw error;
    }
  }

  async readAction(actionId: ProviderActionId): Promise<HerdrActionRecord | null> {
    return this.#readAction(safeActionId(actionId));
  }

  async markDispatched(actionId: ProviderActionId, expectedRevision: number): Promise<HerdrActionRecord> {
    const parsedActionId = safeActionId(actionId);
    return this.#database.transaction(() => {
      const current = requiredAction(this.#readAction(parsedActionId), parsedActionId);
      if (current.status === "terminal" || current.status === "ambiguous" || current.status === "dispatched") return exactRevision(current, expectedRevision);
      exactRevision(current, expectedRevision);
      const changed = this.#database.prepare(`
        UPDATE provider_actions
           SET status='dispatched', history_json='["prepared","dispatched"]',
               execution_count=execution_count+1, journal_revision=journal_revision+1, updated_at=?
         WHERE adapter_id=? AND action_id=? AND status='prepared' AND journal_revision=?
      `).run(this.#clock(), ADAPTER_ID, parsedActionId, expectedRevision);
      if (changed.changes !== 1) throw new TypeError("Herdr action dispatch compare-and-set failed");
      return requiredAction(this.#readAction(parsedActionId), parsedActionId);
    })();
  }

  async completeAction(
    actionId: ProviderActionId,
    expectedRevision: number,
    receipt: HerdrEffectReceipt,
  ): Promise<HerdrActionRecord> {
    const parsedActionId = safeActionId(actionId);
    const parsedReceipt = parseReceipt(receipt);
    return this.#database.transaction(() => {
      const current = exactRevision(requiredAction(this.#readAction(parsedActionId), parsedActionId), expectedRevision);
      if (current.status === "terminal") {
        if (canonicalJson(current.receipt) !== canonicalJson(parsedReceipt)) throw new TypeError("Herdr terminal receipt conflicts with exact replay");
        return current;
      }
      if (current.status !== "dispatched" && current.status !== "ambiguous") throw new TypeError("Herdr action is not dispatch-reconcilable");
      const payload = this.#actionPayload(parsedActionId);
      if (!isRecord(payload) || payload.kind !== parsedReceipt.operation) throw new TypeError("Herdr receipt operation differs from its prepared intent");
      const changed = this.#database.prepare(`
        UPDATE provider_actions
           SET status='terminal', history_json=?, effect_count=1, idempotency_proven=1,
               result_json=?, journal_revision=journal_revision+1, updated_at=?
         WHERE adapter_id=? AND action_id=? AND journal_revision=? AND status IN ('dispatched','ambiguous')
      `).run(
        canonicalJson(current.status === "ambiguous" ? ["prepared", "dispatched", "ambiguous", "terminal"] : ["prepared", "dispatched", "terminal"]),
        canonicalJson(parsedReceipt),
        this.#clock(),
        ADAPTER_ID,
        parsedActionId,
        expectedRevision,
      );
      if (changed.changes !== 1) throw new TypeError("Herdr action completion compare-and-set failed");
      const binding = row(this.#database.prepare(`
        SELECT project_session_id FROM project_session_memberships
         WHERE member_kind='provider-action' AND member_adapter_id=? AND member_id=? AND state='active'
      `).get(ADAPTER_ID, parsedActionId), "Herdr action membership");
      const membership = this.#database.prepare(`
        UPDATE project_session_memberships
           SET state='reconciled', revision=revision+1, updated_at=?
         WHERE member_kind='provider-action' AND member_adapter_id=? AND member_id=? AND state='active'
      `).run(this.#clock(), ADAPTER_ID, parsedActionId);
      touchProjectSessionMembershipRevision(
        this.#database,
        text(binding, "project_session_id"),
        this.#clock(),
        membership.changes,
      );
      return requiredAction(this.#readAction(parsedActionId), parsedActionId);
    })();
  }

  async markAmbiguous(
    actionId: ProviderActionId,
    expectedRevision: number,
    reason: string,
  ): Promise<HerdrActionRecord> {
    const parsedActionId = safeActionId(actionId);
    boundedSafeText(reason, "Herdr ambiguity reason", 1024);
    assertNoCredentialLikeValue(reason, "Herdr ambiguity reason");
    return this.#database.transaction(() => {
      const current = exactRevision(requiredAction(this.#readAction(parsedActionId), parsedActionId), expectedRevision);
      if (current.status === "ambiguous") {
        if (current.ambiguityReason !== reason) throw new TypeError("Herdr ambiguity reason conflicts with exact replay");
        return current;
      }
      if (current.status !== "dispatched") throw new TypeError("Herdr action is not dispatch-ambiguous");
      const changed = this.#database.prepare(`
        UPDATE provider_actions
           SET status='ambiguous', history_json='["prepared","dispatched","ambiguous"]',
               result_json=?, journal_revision=journal_revision+1, updated_at=?
         WHERE adapter_id=? AND action_id=? AND status='dispatched' AND journal_revision=?
      `).run(canonicalJson({ ambiguityReason: reason }), this.#clock(), ADAPTER_ID, parsedActionId, expectedRevision);
      if (changed.changes !== 1) throw new TypeError("Herdr ambiguity compare-and-set failed");
      return requiredAction(this.#readAction(parsedActionId), parsedActionId);
    })();
  }

  /**
   * Reconciles restart evidence without replaying an external effect. Prepared
   * actions remain prepared; only already-dispatched actions reach Herdr's
   * adapter-local evidence lookup.
   */
  async recover(evidence: Readonly<{
    lookupAction(actionId: ProviderActionId): Promise<HerdrActionEvidence>;
  }>): Promise<HerdrRecoverySummary> {
    const candidates = this.#database.prepare(`
      SELECT action_id FROM provider_actions
       WHERE adapter_id=? AND status IN ('prepared','dispatched','ambiguous')
       ORDER BY updated_at ASC, action_id ASC
    `).all(ADAPTER_ID).filter(isRow).map((value) =>
      safeActionId(parseIdentifier<"ProviderActionId">(text(value, "action_id"), "herdrRecovery.actionId"))
    );
    const summary = { observed: 0, terminal: 0, ambiguous: 0, prepared: 0 };
    for (const actionId of candidates) {
      const current = await this.readAction(actionId);
      if (current === null || current.status === "terminal") continue;
      if (current.status === "prepared") {
        summary.prepared += 1;
        continue;
      }
      let lookup: HerdrActionEvidence;
      try {
        lookup = parseEvidence(await evidence.lookupAction(actionId));
      } catch {
        lookup = { status: "unknown" };
      }
      if (lookup.status === "observed") {
        const payload = this.#actionPayload(actionId);
        if (!isRecord(payload) || payload.kind !== lookup.receipt.operation) {
          if (current.status === "dispatched") {
            await this.markAmbiguous(
              actionId,
              current.revision,
              "Herdr effect evidence conflicts with the prepared intent; automatic replay is forbidden",
            );
          }
          summary.ambiguous += 1;
          continue;
        }
        summary.observed += 1;
        await this.completeAction(actionId, current.revision, lookup.receipt);
        summary.terminal += 1;
        continue;
      }
      if (current.status === "ambiguous") {
        summary.ambiguous += 1;
        continue;
      }
      const reason = lookup.status === "absent"
        ? "Herdr effect lookup reported absent; automatic replay is forbidden"
        : "Herdr effect outcome is unknown; automatic replay is forbidden";
      await this.markAmbiguous(actionId, current.revision, reason);
      summary.ambiguous += 1;
    }
    return summary;
  }

  #validateSteerReference(reference: FabricSteerReference): FabricSteerReferenceValidation {
    const parsed = parseSteerReference(reference);
    let run: { projectId: ProjectId; projectSessionId: ProjectSessionId };
    try {
      run = this.#runIdentity(parsed.coordinationRunId);
    } catch {
      return rejection("unknown-reference", "Fabric coordination run does not exist");
    }
    if (run.projectId !== parsed.projectId || run.projectSessionId !== parsed.projectSessionId) {
      return rejection("scope-mismatch", "reference belongs to another project session");
    }
    const taskRow = this.#database.prepare(`
      SELECT revision, state, owner_agent_id FROM tasks WHERE run_id=? AND task_id=?
    `).get(parsed.coordinationRunId, parsed.taskId);
    if (!isRow(taskRow)) return rejection("unknown-reference", "Fabric task does not exist");
    const revision = integer(taskRow, "revision");
    if (revision !== parsed.expectedRevision) return rejection("stale-reference", "Fabric task revision changed");
    const ownerAgentId = nullableText(taskRow, "owner_agent_id");
    if (ownerAgentId === null || text(taskRow, "state") !== "active") {
      return rejection("scope-mismatch", "Fabric task has no active target owner");
    }
    if (!isRow(this.#database.prepare("SELECT 1 FROM agents WHERE run_id=? AND agent_id=?").get(parsed.coordinationRunId, ownerAgentId))) {
      return rejection("scope-mismatch", "Fabric task owner is unavailable");
    }
    if (parsed.kind === "message") {
      const message = this.#database.prepare(`
        SELECT m.kind, m.requires_ack, m.task_revision, m.expires_at,
               d.state AS delivery_state, context.context_json
          FROM messages m
          JOIN deliveries d ON d.message_id=m.message_id
          JOIN message_contexts context ON context.message_id=m.message_id
         WHERE m.run_id=? AND m.message_id=? AND d.run_id=m.run_id AND d.recipient_id=?
      `).get(parsed.coordinationRunId, parsed.messageId, ownerAgentId);
      if (!isRow(message)) return rejection("unknown-reference", "Fabric message does not target the active task owner");
      if (message.task_revision !== revision) {
        return rejection("stale-reference", "Fabric message task revision changed");
      }
      if (
        text(message, "context_json") !== canonicalJson({ kind: "task", taskId: parsed.taskId }) ||
        text(message, "kind") !== "steer" || integer(message, "requires_ack") !== 0 ||
        !["ready", "claimed"].includes(text(message, "delivery_state")) ||
        (message.expires_at !== null && integer(message, "expires_at") <= this.#clock()) ||
        isRow(this.#database.prepare(`
          SELECT 1 FROM task_requests WHERE request_message_id=?
          UNION ALL
          SELECT 1 FROM task_results WHERE reply_message_id=?
          LIMIT 1
        `).get(parsed.messageId, parsed.messageId))
      ) {
        return rejection("scope-mismatch", "Fabric message is answer-bearing, resolved or outside one-way steering");
      }
      const base = {
        targetAgentId: parseIdentifier<"AgentId">(ownerAgentId, "herdrSteer.targetAgentId"),
        purpose: "steer" as const,
        requiresAck: false,
        expectsResult: false,
        dependentBarrierId: null,
      };
      return {
        status: "valid",
        referenceDigest: digestJson({ reference: parsed, ...base }),
        ...base,
      };
    }
    const request = this.#database.prepare(`
      SELECT dependent_barrier_id FROM task_requests
       WHERE run_id=? AND task_id=? AND state IN ('pending','overdue','reassigned')
       ORDER BY created_at DESC LIMIT 1
    `).get(parsed.coordinationRunId, parsed.taskId);
    const expectsResult = isRow(request);
    const dependentBarrierId = expectsResult
      ? parseIdentifier<"BarrierId">(text(request, "dependent_barrier_id"), "herdrSteer.dependentBarrierId")
      : null;
    const base = {
      targetAgentId: parseIdentifier<"AgentId">(ownerAgentId, "herdrSteer.targetAgentId"),
      purpose: expectsResult ? "request" as const : "steer" as const,
      requiresAck: expectsResult,
      expectsResult,
      dependentBarrierId,
    };
    return {
      status: "valid",
      referenceDigest: digestJson({ reference: parsed, ...base }),
      ...base,
    };
  }

  #runIdentity(runId: CoordinationRunId): { projectId: ProjectId; projectSessionId: ProjectSessionId } {
    const value = this.#database.prepare(`
      SELECT s.project_id, r.project_session_id
        FROM runs r JOIN project_sessions s ON s.project_session_id=r.project_session_id
       WHERE r.run_id=?
    `).get(runId);
    if (!isRow(value)) throw new TypeError("Herdr action run is unavailable");
    return {
      projectId: parseIdentifier<"ProjectId">(text(value, "project_id"), "herdr.projectId"),
      projectSessionId: parseIdentifier<"ProjectSessionId">(text(value, "project_session_id"), "herdr.projectSessionId"),
    };
  }

  #validatedActionInput(input: HerdrAppliedActionInput): {
    actionId: ProviderActionId;
    intent: JsonValue;
    operation: HerdrAppliedOperation;
  } {
    const actionId = safeActionId(input.actionId);
    const run = this.#runIdentity(input.coordinationRunId);
    if (run.projectId !== input.projectId || run.projectSessionId !== input.projectSessionId) {
      throw new TypeError("Herdr action binding names another project session");
    }
    assertClosedAppliedIntent(input.intent, input);
    const intent = canonicalise(parseJsonValue(input.intent, "herdr.intent"));
    assertNoCredentialLikeValue(intent, "Herdr intent");
    if (!isRecord(intent) || typeof intent.kind !== "string" || !HERDR_APPLIED_OPERATIONS.has(intent.kind as HerdrAppliedOperation)) {
      throw new TypeError("Herdr action kind is outside the closed integration family");
    }
    this.#assertAuthoritativeIntentBinding(intent, input);
    return { actionId, intent, operation: intent.kind as HerdrAppliedOperation };
  }

  #assertAuthoritativeIntentBinding(intent: Record<string, unknown>, input: HerdrAppliedActionInput): void {
    if (intent.kind === "agent.ensure-pane") {
      const identity = intent.identity;
      if (!isRecord(identity) || input.targetAgentId === null) throw new TypeError("Herdr agent identity is unavailable");
      const current = this.#database.prepare(`
        SELECT agent.provider_session_ref,
               COALESCE(state.provider_session_generation, 1) AS provider_session_generation,
               binding.adapter_id
          FROM agents agent
          LEFT JOIN provider_state state ON state.run_id=agent.run_id AND state.agent_id=agent.agent_id
          LEFT JOIN agent_adapter_bindings binding ON binding.run_id=agent.run_id AND binding.agent_id=agent.agent_id
         WHERE agent.run_id=? AND agent.agent_id=?
      `).get(input.coordinationRunId, input.targetAgentId);
      if (
        !isRow(current) || nullableText(current, "provider_session_ref") === null ||
        nullableText(current, "adapter_id") === null ||
        identity.providerSessionRef !== nullableText(current, "provider_session_ref") ||
        identity.provider !== nullableText(current, "adapter_id") ||
        identity.providerSessionGeneration !== integer(current, "provider_session_generation")
      ) throw new TypeError("Herdr agent provider session identity is stale or unbound");
      return;
    }
    let itemId: unknown;
    let revision: unknown;
    if (intent.kind === "attention.project") {
      itemId = intent.itemId;
      revision = intent.revision;
    } else if (intent.kind === "notification.show") {
      itemId = intent.attentionItemId;
      revision = intent.attentionRevision;
    } else if (intent.kind === "target.focus" && isRecord(intent.target) && intent.target.kind === "console-item" && intent.target.view === "attention") {
      itemId = intent.target.itemId;
      revision = intent.target.revision;
    } else {
      return;
    }
    const item = this.#database.prepare(`
      SELECT attention.project_session_id, attention.coordination_run_id, attention.revision, session.project_id
        FROM attention_items attention
        JOIN project_sessions session ON session.project_session_id=attention.project_session_id
       WHERE attention.item_id=?
    `).get(itemId);
    if (
      !isRow(item) || text(item, "project_id") !== input.projectId ||
      text(item, "project_session_id") !== input.projectSessionId ||
      integer(item, "revision") !== revision ||
      (nullableText(item, "coordination_run_id") !== null && nullableText(item, "coordination_run_id") !== input.coordinationRunId)
    ) throw new TypeError("Herdr attention projection binding is stale or outside the coordination run");
  }

  #prepareAction(input: Readonly<{
    actionId: ProviderActionId;
    runId: CoordinationRunId;
    projectId: ProjectId;
    projectSessionId: ProjectSessionId;
    targetAgentId: AgentId | null;
    operation: string;
    payload: JsonValue;
  }>, ticket: ProviderActionTicket): HerdrActionRecord {
    const intentDigest = digestJson(input.payload);
    const existing = this.#database.prepare(`
      SELECT run_id FROM provider_actions WHERE adapter_id=? AND action_id=?
    `).get(ADAPTER_ID, input.actionId);
    if (isRow(existing)) {
      const current = this.#readAction(input.actionId);
      if (
        current === null || text(existing, "run_id") !== input.runId ||
        current.intentDigest !== intentDigest
      ) {
        throw new TypeError("Herdr stable action identity conflicts with an existing action");
      }
      return current;
    }
    if (input.targetAgentId !== null && !isRow(this.#database.prepare("SELECT 1 FROM agents WHERE run_id=? AND agent_id=?").get(input.runId, input.targetAgentId))) {
      throw new TypeError("Herdr target agent is outside the coordination run");
    }
    const generationRow = input.targetAgentId === null ? null : this.#database.prepare(`
      SELECT provider_session_generation FROM provider_state WHERE run_id=? AND agent_id=?
    `).get(input.runId, input.targetAgentId);
    const providerGeneration = isRow(generationRow) ? integer(generationRow, "provider_session_generation") : null;
    this.#providerActionAdmission.admitUnroutedInCurrentTransaction(ticket, {
      runId: input.runId,
      actionId: input.actionId,
      adapterId: ADAPTER_ID,
      operation: `herdr:${input.operation}`,
      targetAgentId: input.targetAgentId,
      providerSessionGeneration: providerGeneration,
      identityHash: digestJson({
        runId: input.runId,
        projectSessionId: input.projectSessionId,
        targetAgentId: input.targetAgentId,
      }),
      payloadHash: intentDigest,
      payloadJson: canonicalJson(input.payload),
      status: "prepared",
      historyJson: '["prepared"]',
      executionCount: 0,
      updatedAt: this.#clock(),
    }, "herdr", () => {
      const membership = this.#database.prepare(`
        INSERT INTO project_session_memberships(
          project_session_id, coordination_run_id, member_kind, member_id, member_adapter_id,
          required, state, revision, abandoned_reason, created_at, updated_at
        ) VALUES (?, ?, 'provider-action', ?, ?, 1, 'active', 1, NULL, ?, ?)
      `).run(input.projectSessionId, input.runId, input.actionId, ADAPTER_ID, this.#clock(), this.#clock());
      touchProjectSessionMembershipRevision(
        this.#database,
        input.projectSessionId,
        this.#clock(),
        membership.changes,
      );
    });
    return requiredAction(this.#readAction(input.actionId), input.actionId);
  }

  #preflightAction(input: Readonly<{
    actionId: ProviderActionId;
    runId: CoordinationRunId;
    projectId: ProjectId;
    projectSessionId: ProjectSessionId;
    targetAgentId: AgentId | null;
    operation: string;
    payload: JsonValue;
  }>): ProviderActionTicket {
    const project = row(this.#database.prepare(`
      SELECT project_id FROM project_sessions WHERE project_session_id=?
    `).get(input.projectSessionId), "Herdr project");
    if (text(project, "project_id") !== input.projectId) {
      throw new TypeError("Herdr project binding is stale");
    }
    const availability = this.#database.prepare(`
      SELECT discovered_contract_json FROM integration_availability
       WHERE integration_id=?
    `).get(ADAPTER_ID);
    if (!isRow(availability)) {
      throw new TypeError("Herdr integration identity is not authenticated");
    }
    let discoveredContract: unknown;
    try {
      discoveredContract = JSON.parse(text(availability, "discovered_contract_json"));
    } catch {
      throw new TypeError("Herdr integration identity contract is invalid");
    }
    if (
      !isRow(discoveredContract) ||
      discoveredContract.schemaVersion !== 1 ||
      discoveredContract.operationFamily !== ADAPTER_ID
    ) {
      throw new TypeError("Herdr integration identity contract is invalid");
    }
    return this.#providerActionAdmission.preflight({
      actionRef: { adapterId: ADAPTER_ID, actionId: input.actionId },
      scope: { kind: "run-action", runId: input.runId },
      principal: {
        kind: "integration",
        integrationId: ADAPTER_ID,
        projectId: input.projectId,
      },
      canonicalInput: {
        schemaVersion: 1,
        operation: input.operation,
        projectSessionId: input.projectSessionId,
        targetAgentId: input.targetAgentId,
        intent: input.payload,
      },
    });
  }

  #releaseProviderActionPreflightAfterRollback(ticket: ProviderActionTicket, failure: unknown): void {
    if (ticket.disposition !== "resolving" || ticket.scope.kind !== "run-action") return;
    if (!isDeterministicClosedPreflightFailure(failure)) return;
    const actionExists = this.#database.prepare(`
      SELECT 1 FROM provider_actions WHERE run_id=? AND adapter_id=? AND action_id=?
    `).get(ticket.scope.runId, ticket.actionRef.adapterId, ticket.actionRef.actionId) !== undefined;
    if (actionExists) return;
    try {
      this.#providerActionAdmission.release(ticket, failure);
    } catch {
      // The outer preparation failure remains authoritative if release races.
    }
  }

  #readAction(actionId: ProviderActionId): HerdrActionRecord | null {
    const value = this.#database.prepare(`
      SELECT run_id, action_id, payload_hash, status, result_json, journal_revision
        FROM provider_actions WHERE adapter_id=? AND action_id=?
    `).get(ADAPTER_ID, actionId);
    if (!isRow(value)) return null;
    assertProviderActionOwner(this.#database, {
      runId: text(value, "run_id"),
      adapterId: ADAPTER_ID,
      actionId,
    }, "herdr");
    const status = text(value, "status");
    const base = {
      actionId: parseIdentifier<"ProviderActionId">(text(value, "action_id"), "herdrAction.actionId"),
      revision: integer(value, "journal_revision"),
      intentDigest: parseSha256Digest(text(value, "payload_hash"), "herdrAction.intentDigest"),
    };
    if (status === "prepared" || status === "dispatched") return { ...base, status };
    if (status === "ambiguous") {
      const result = parseStoredJson(value, "result_json");
      if (!isRecord(result) || typeof result.ambiguityReason !== "string") throw new TypeError("stored Herdr ambiguity evidence is invalid");
      const ambiguityReason = boundedSafeText(result.ambiguityReason, "stored Herdr ambiguity reason", 1024);
      assertNoCredentialLikeValue(ambiguityReason, "stored Herdr ambiguity reason");
      return { ...base, status, ambiguityReason };
    }
    if (status === "terminal") return { ...base, status, receipt: parseReceipt(parseStoredJson(value, "result_json")) };
    throw new TypeError("stored Herdr action has an invalid lifecycle state");
  }

  #actionPayload(actionId: ProviderActionId): JsonValue {
    const value = row(this.#database.prepare(`
      SELECT payload_json FROM provider_actions WHERE adapter_id=? AND action_id=?
    `).get(ADAPTER_ID, actionId), "Herdr action payload");
    return parseJsonValue(JSON.parse(text(value, "payload_json")), "herdrAction.payload");
  }
}

function parseSteerReference(value: FabricSteerReference): FabricSteerReference {
  if (!isRecord(value)) throw new TypeError("Herdr steer reference must be an object");
  const expected = value.kind === "task"
    ? ["coordinationRunId", "expectedRevision", "kind", "projectId", "projectSessionId", "taskId"]
    : ["coordinationRunId", "expectedRevision", "kind", "messageId", "projectId", "projectSessionId", "taskId"];
  if ((value.kind !== "task" && value.kind !== "message") || !exactKeys(value, expected)) throw new TypeError("Herdr steer reference is not closed");
  if (!Number.isSafeInteger(value.expectedRevision) || value.expectedRevision < 0) throw new TypeError("Herdr steer reference revision is invalid");
  const base = {
    projectId: parseIdentifier<"ProjectId">(value.projectId, "herdrSteer.projectId"),
    projectSessionId: parseIdentifier<"ProjectSessionId">(value.projectSessionId, "herdrSteer.projectSessionId"),
    coordinationRunId: parseIdentifier<"CoordinationRunId">(value.coordinationRunId, "herdrSteer.coordinationRunId"),
    taskId: parseIdentifier<"TaskId">(value.taskId, "herdrSteer.taskId"),
    expectedRevision: value.expectedRevision,
  };
  return value.kind === "task"
    ? { kind: "task", ...base }
    : { kind: "message", ...base, messageId: parseIdentifier<"MessageId">(value.messageId, "herdrSteer.messageId") };
}

function assertClosedDirectSteerIntent(intent: DirectSteerIntent): void {
  if (!isRecord(intent) || !exactKeys(intent, ["kind", "paneRef", "prompt", "reference", "targetAgentId", "validatedReferenceDigest"]) || intent.kind !== "steer.inject-fire-and-forget") {
    throw new TypeError("Herdr direct-steer intent is not closed");
  }
  parseIdentifier<"AgentId">(intent.targetAgentId, "herdrSteer.targetAgentId");
  parseSha256Digest(intent.validatedReferenceDigest, "herdrSteer.validatedReferenceDigest");
  if (!/^[A-Za-z0-9][A-Za-z0-9:._-]{0,127}$/u.test(intent.paneRef)) throw new TypeError("Herdr direct-steer pane reference is invalid");
  boundedSafeText(intent.prompt, "Herdr direct-steer prompt", 4096);
  parseSteerReference(intent.reference);
}

function assertClosedAppliedIntent(
  value: JsonValue,
  binding: Readonly<{
    projectId: ProjectId;
    projectSessionId: ProjectSessionId;
    coordinationRunId: CoordinationRunId;
    targetAgentId: AgentId | null;
  }>,
): void {
  if (!isRecord(value) || typeof value.kind !== "string") {
    throw new TypeError("Herdr action intent must be a closed object");
  }
  switch (value.kind) {
    case "console.ensure-pane": {
      requireExactIntent(value, ["kind", "profileId", "projectId", "projectSessionId"]);
      requireProjectBinding(value, binding);
      if (value.profileId !== "agent-fabric-console" || binding.targetAgentId !== null) {
        throw new TypeError("Herdr Console intent has an invalid closed binding");
      }
      return;
    }
    case "agent.ensure-pane": {
      requireExactIntent(value, ["identity", "kind", "paneClass", "placement", "surface"]);
      if (!isRecord(value.identity)) throw new TypeError("Herdr agent identity must be a closed object");
      requireExactIntent(value.identity, [
        "agentId", "coordinationRunId", "modelFamily", "projectId", "projectSessionId",
        "provider", "providerSessionGeneration", "providerSessionRef",
      ]);
      requireProjectBinding(value.identity, binding);
      if (
        value.identity.coordinationRunId !== binding.coordinationRunId ||
        value.identity.agentId !== binding.targetAgentId || binding.targetAgentId === null
      ) throw new TypeError("Herdr agent identity is outside its closed Fabric binding");
      parseIdentifier<"AgentId">(value.identity.agentId, "herdr.intent.identity.agentId");
      boundedSafeText(value.identity.provider, "Herdr identity provider", 128);
      boundedSafeText(value.identity.modelFamily, "Herdr identity model family", 128);
      boundedSafeText(value.identity.providerSessionRef, "Herdr identity provider session", 512);
      if (!Number.isSafeInteger(value.identity.providerSessionGeneration) || Number(value.identity.providerSessionGeneration) < 1) {
        throw new TypeError("Herdr identity provider generation is invalid");
      }
      if (
        !["chair", "paired-primary", "selected-long-running-worker"].includes(String(value.paneClass)) ||
        !["provider-tui", "observer"].includes(String(value.surface)) ||
        !["beside-chair", "workspace-default"].includes(String(value.placement))
      ) throw new TypeError("Herdr agent pane intent has an invalid closed variant");
      return;
    }
    case "panes.arrange": {
      requireExactIntent(value, ["kind", "layout", "paneRefs"]);
      if (
        !Array.isArray(value.paneRefs) || value.paneRefs.length < 1 || value.paneRefs.length > 16 ||
        !value.paneRefs.every(validPaneReference) || new Set(value.paneRefs).size !== value.paneRefs.length ||
        (value.layout !== "side-by-side" && value.layout !== "workspace-default") || binding.targetAgentId !== null
      ) throw new TypeError("Herdr pane arrangement intent has an invalid closed shape");
      return;
    }
    case "agent.project-metadata": {
      requireExactIntent(value, ["agentId", "kind", "metadata", "paneRef"]);
      if (value.agentId !== binding.targetAgentId || binding.targetAgentId === null || !validPaneReference(value.paneRef) || !isRecord(value.metadata)) {
        throw new TypeError("Herdr agent metadata intent has an invalid closed binding");
      }
      requireExactIntent(value.metadata, ["contextPressure", "lifecycle", "modelFamily", "provider", "role", "taskLabel"]);
      parseIdentifier<"AgentId">(value.agentId, "herdr.intent.agentId");
      boundedSafeText(value.metadata.provider, "Herdr metadata provider", 128);
      boundedSafeText(value.metadata.modelFamily, "Herdr metadata model family", 128);
      boundedSafeText(value.metadata.lifecycle, "Herdr metadata lifecycle", 128);
      boundedTextAllowEmpty(value.metadata.taskLabel, "Herdr metadata task label", 512);
      if (
        !["chair", "lead", "worker", "reviewer"].includes(String(value.metadata.role)) ||
        !["low", "medium", "high", "unknown"].includes(String(value.metadata.contextPressure))
      ) throw new TypeError("Herdr metadata intent has an invalid closed variant");
      return;
    }
    case "attention.project": {
      requireExactIntent(value, ["itemId", "kind", "label", "projectId", "projectSessionId", "revision", "title"]);
      requireProjectBinding(value, binding);
      boundedSafeText(value.itemId, "Herdr attention item", 128);
      boundedSafeText(value.title, "Herdr attention title", 512);
      nonNegativeRevision(value.revision, "Herdr attention revision");
      if (!["Decision", "Approval", "Blocked", "FYI"].includes(String(value.label)) || binding.targetAgentId !== null) {
        throw new TypeError("Herdr attention intent has an invalid closed variant");
      }
      return;
    }
    case "target.focus": {
      requireExactIntent(value, ["kind", "target"]);
      if (!isRecord(value.target) || typeof value.target.kind !== "string") throw new TypeError("Herdr focus target must be closed");
      if (value.target.kind === "agent-pane") {
        requireExactIntent(value.target, ["agentId", "kind", "paneRef"]);
        if (value.target.agentId !== binding.targetAgentId || binding.targetAgentId === null || !validPaneReference(value.target.paneRef)) {
          throw new TypeError("Herdr agent focus target has an invalid closed binding");
        }
        parseIdentifier<"AgentId">(value.target.agentId, "herdr.intent.target.agentId");
        return;
      }
      if (value.target.kind === "console-item") {
        requireExactIntent(value.target, ["itemId", "kind", "revision", "view"]);
        boundedSafeText(value.target.itemId, "Herdr Console focus item", 128);
        nonNegativeRevision(value.target.revision, "Herdr Console focus revision");
        if (!CONSOLE_VIEWS.has(String(value.target.view)) || binding.targetAgentId !== null) {
          throw new TypeError("Herdr Console focus target has an invalid closed binding");
        }
        return;
      }
      throw new TypeError("Herdr focus target is outside the closed family");
    }
    case "agent.wake": {
      requireExactIntent(value, ["agentId", "kind", "paneRef"]);
      if (value.agentId !== binding.targetAgentId || binding.targetAgentId === null || !validPaneReference(value.paneRef)) {
        throw new TypeError("Herdr wake intent has an invalid closed binding");
      }
      parseIdentifier<"AgentId">(value.agentId, "herdr.intent.agentId");
      return;
    }
    case "notification.show": {
      requireExactIntent(value, ["attentionItemId", "attentionRevision", "body", "focusTarget", "kind", "title"]);
      boundedSafeText(value.attentionItemId, "Herdr notification item", 128);
      boundedSafeText(value.title, "Herdr notification title", 256);
      boundedSafeText(value.body, "Herdr notification body", 1024);
      nonNegativeRevision(value.attentionRevision, "Herdr notification revision");
      if (binding.targetAgentId !== null) throw new TypeError("Herdr notification intent has an invalid closed binding");
      if (value.focusTarget === null) return;
      if (!isRecord(value.focusTarget)) throw new TypeError("Herdr notification focus target must be closed");
      requireExactIntent(value.focusTarget, ["itemId", "kind", "revision", "view"]);
      boundedSafeText(value.focusTarget.itemId, "Herdr notification focus item", 128);
      nonNegativeRevision(value.focusTarget.revision, "Herdr notification focus revision");
      if (value.focusTarget.kind !== "console-item" || !CONSOLE_VIEWS.has(String(value.focusTarget.view))) {
        throw new TypeError("Herdr notification focus target is outside the closed family");
      }
      return;
    }
    default:
      throw new TypeError("Herdr action kind is outside the closed integration family");
  }
}

const CONSOLE_VIEWS = new Set(["attention", "project", "runs", "work", "agents", "evidence", "activity", "system"]);

function requireExactIntent(value: Record<string, unknown>, keys: readonly string[]): void {
  if (!exactKeys(value, keys)) throw new TypeError("Herdr action intent is not closed");
}

function requireProjectBinding(
  value: Record<string, unknown>,
  binding: Readonly<{ projectId: ProjectId; projectSessionId: ProjectSessionId }>,
): void {
  const projectId = parseIdentifier<"ProjectId">(value.projectId, "herdr.intent.projectId");
  const projectSessionId = parseIdentifier<"ProjectSessionId">(value.projectSessionId, "herdr.intent.projectSessionId");
  if (projectId !== binding.projectId || projectSessionId !== binding.projectSessionId) {
    throw new TypeError("Herdr intent names another project session");
  }
}

function validPaneReference(value: unknown): value is string {
  return typeof value === "string" && /^[A-Za-z0-9][A-Za-z0-9:._-]{0,127}$/u.test(value) && !SECRET_PATTERN.test(value);
}

function nonNegativeRevision(value: unknown, label: string): void {
  if (!Number.isSafeInteger(value) || Number(value) < 0) throw new TypeError(`${label} is invalid`);
}

function boundedTextAllowEmpty(value: unknown, label: string, maximumBytes: number): string {
  if (typeof value !== "string" || Buffer.byteLength(value, "utf8") > maximumBytes || /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f\u009b]/u.test(value)) {
    throw new TypeError(`${label} is unsafe or exceeds its bound`);
  }
  return value;
}

function parseReceipt(value: unknown): HerdrEffectReceipt {
  if (!isRecord(value)) throw new TypeError("Herdr effect receipt must be an object");
  if (value.status === "dispatched-unconfirmed") {
    if (!exactKeys(value, ["canCloseBarrier", "canSatisfyExpectedResult", "deliveryEvidence", "operation", "referenceValidation", "status"]) ||
        value.operation !== "steer.inject-fire-and-forget" || value.referenceValidation !== "verified" ||
        value.deliveryEvidence !== "none" || value.canSatisfyExpectedResult !== false || value.canCloseBarrier !== false) {
      throw new TypeError("Herdr direct-steer receipt is invalid");
    }
    return value as HerdrEffectReceipt;
  }
  const keys = ["operation", "status"];
  if (value.paneRef !== undefined) keys.push("paneRef");
  if (value.detail !== undefined) keys.push("detail");
  if (
    !exactKeys(value, keys) || value.status !== "applied" || typeof value.operation !== "string" ||
    !HERDR_APPLIED_OPERATIONS.has(value.operation as HerdrAppliedOperation)
  ) throw new TypeError("Herdr effect receipt is invalid");
  const detail = value.detail === undefined ? undefined : parseJsonValue(value.detail, "herdrReceipt.detail");
  const parsed = {
    status: "applied" as const,
    operation: value.operation as HerdrAppliedOperation,
    ...(value.paneRef === undefined ? {} : { paneRef: boundedSafeText(value.paneRef, "Herdr receipt pane", 128) }),
    ...(detail === undefined ? {} : { detail }),
  };
  assertNoCredentialLikeValue(parsed, "Herdr receipt");
  return parsed;
}

function parseEvidence(value: unknown): HerdrActionEvidence {
  if (!isRecord(value) || typeof value.status !== "string") throw new TypeError("Herdr effect evidence is invalid");
  if (value.status === "absent" || value.status === "unknown") {
    if (!exactKeys(value, ["status"])) throw new TypeError("Herdr effect evidence is not closed");
    return { status: value.status };
  }
  if (value.status === "observed" && exactKeys(value, ["receipt", "status"])) {
    return { status: "observed", receipt: parseReceipt(value.receipt) };
  }
  throw new TypeError("Herdr effect evidence is invalid");
}

function parseStoredJson(value: Record<string, unknown>, field: string): unknown {
  const stored = text(value, field);
  if (Buffer.byteLength(stored, "utf8") > 64 * 1024) throw new TypeError(`stored ${field} exceeds its bound`);
  return JSON.parse(stored);
}

function requiredAction(value: HerdrActionRecord | null, actionId: ProviderActionId): HerdrActionRecord {
  if (value === null) throw new TypeError(`Herdr action ${actionId} is unavailable`);
  return value;
}

function exactRevision(record: HerdrActionRecord, expectedRevision: number): HerdrActionRecord {
  if (!Number.isSafeInteger(expectedRevision) || expectedRevision < 1 || record.revision !== expectedRevision) {
    throw new TypeError("Herdr action revision is stale");
  }
  return record;
}

function safeActionId(value: ProviderActionId): ProviderActionId {
  const actionId = parseIdentifier<"ProviderActionId">(value, "herdrAction.actionId");
  assertNoCredentialLikeValue(actionId, "Herdr action ID");
  return actionId;
}

function rejection(code: "unknown-reference" | "stale-reference" | "scope-mismatch", reason: string): FabricSteerReferenceValidation {
  return { status: "rejected", code, reason };
}

function digestJson(value: JsonValue): Sha256Digest {
  return parseSha256Digest(`sha256:${createHash("sha256").update(JSON.stringify(canonicalise(value))).digest("hex")}`, "herdr.digest");
}

function canonicalise(value: JsonValue): JsonValue {
  if (Array.isArray(value)) return value.map(canonicalise);
  if (value !== null && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0).map(([key, entry]) => [key, canonicalise(entry)]));
  }
  return value;
}

function boundedSafeText(value: unknown, label: string, maximumBytes: number): string {
  if (typeof value !== "string") throw new TypeError(`${label} must be text`);
  const bytes = Buffer.byteLength(value, "utf8");
  if (bytes < 1 || bytes > maximumBytes || /[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f\u009b]/u.test(value)) {
    throw new TypeError(`${label} is unsafe or exceeds its bound`);
  }
  return value;
}

function assertNoCredentialLikeValue(value: unknown, label: string): void {
  if (SECRET_PATTERN.test(JSON.stringify(value))) throw new TypeError(`${label} contains credential-like data`);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function exactKeys(value: Record<string, unknown>, expected: readonly string[]): boolean {
  const actual = Object.keys(value).sort();
  const sorted = [...expected].sort();
  return actual.length === sorted.length && actual.every((key, index) => key === sorted[index]);
}
