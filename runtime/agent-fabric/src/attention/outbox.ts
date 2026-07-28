import type Database from "better-sqlite3";

import {
  ProjectFabricCoreError,
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

export type AttentionProducerContext = Readonly<{
  producerId: string;
  projectId: string;
  projectSessionId: string;
  coordinationRunId?: string | null;
  principalGeneration: number;
}>;

export type NotificationWorkerContext = Readonly<{
  workerInstanceId: string;
  integrationId: string;
}>;

export type AttentionItemRecord = Readonly<{
  itemId: string;
  projectSessionId: string;
  coordinationRunId: string | null;
  kind: string;
  severity: string;
  revision: number;
  state: "open" | "acknowledged" | "resolved" | "cancelled";
  dedupeKey: string;
  payload: unknown;
  createdAt: string;
  updatedAt: string;
}>;

export type IntegrationAvailability = "available" | "unavailable" | "stale";
export type NotificationDeliveryState =
  | "pending"
  | "claimed"
  | "sent"
  | "failed"
  | "deduplicated"
  | "ambiguous";

export type NotificationDeliveryRecord = Readonly<{
  notificationId: string;
  itemId: string;
  itemRevision: number;
  targetIntegration: string;
  dedupeKey: string;
  state: NotificationDeliveryState;
  claimGeneration: number;
  claimDeadline: string | null;
  effectIdentityHash: string | null;
  availability: IntegrationAvailability;
  updatedAt: string;
}>;

export type ClaimedNotificationDelivery = NotificationDeliveryRecord & Readonly<{
  state: "claimed";
  claimDeadline: string;
  effectIdentityHash: string;
}>;

export type PendingNotificationWork = Readonly<{
  delivery: NotificationDeliveryRecord & Readonly<{ state: "pending" }>;
  attention: AttentionItemRecord;
}>;

type AttentionUpsertRequest = Readonly<{
  dedupeKey: string;
  kind: string;
  severity: string;
  payload: unknown;
}>;

type AttentionSettleRequest = Readonly<{
  itemId: string;
  expectedRevision: number;
  state: "resolved" | "cancelled";
  reason: string;
}>;

const NOTIFICATION_ACTIVE_SESSION_STATES = [
  "awaiting_launch",
  "launching",
  "active",
  "quiescing",
  "awaiting_acceptance",
  "launch_ambiguous",
  "reconciling",
  "visibility_degraded",
  "recovery_required",
  "quarantined",
] as const;

type NotificationEnqueueRequest = Readonly<{
  itemId: string;
  expectedItemRevision: number;
  targetIntegration: string;
}>;

type IntegrationAvailabilityRequest = Readonly<{
  state: IntegrationAvailability;
  discoveredContract: unknown;
}>;

type NotificationClaimRequest = Readonly<{
  notificationId: string;
  expectedItemRevision: number;
  expectedClaimGeneration: number;
  claimDeadline: string;
}>;

type NotificationOutcomeRequest = Readonly<{
  notificationId: string;
  claimGeneration: number;
  outcome: "sent" | "failed" | "deduplicated" | "ambiguous";
  effectIdentityHash: string;
  detail: unknown;
}>;

type NotificationRetryRequest = Readonly<{
  notificationId: string;
  expectedClaimGeneration: number;
  reason: string;
}>;

type NotificationAmbiguousReconciliationRequest = Readonly<{
  notificationId: string;
  claimGeneration: number;
  outcome: "sent" | "failed" | "deduplicated";
  effectIdentityHash: string;
  evidence: unknown;
}>;

export class NotificationOutbox {
  readonly #database: Database.Database;
  readonly #clock: () => number;
  readonly #fault: (label: string) => void;

  constructor(options: CoreServiceOptions) {
    this.#database = options.database;
    this.#clock = options.clock ?? Date.now;
    this.#fault = options.fault ?? (() => undefined);
  }

  upsertAttention(
    context: AttentionProducerContext,
    request: AttentionUpsertRequest,
  ): AttentionItemRecord {
    const execute = this.#database.transaction((): AttentionItemRecord => {
      this.#assertProducerContext(context);
      const dedupeKey = this.#requiredText(request.dedupeKey, "attention dedupe key");
      const kind = this.#requiredText(request.kind, "attention kind");
      const severity = this.#requiredText(request.severity, "attention severity");
      const payloadJson = canonicalJson(request.payload);
      const existing = this.#database.prepare(`
        SELECT * FROM attention_items WHERE project_session_id=? AND dedupe_key=?
      `).get(context.projectSessionId, dedupeKey);
      const runId = context.coordinationRunId ?? null;
      if (isRow(existing)) {
        if (nullableText(existing, "coordination_run_id") !== runId) {
          throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "attention identity cannot move between runs");
        }
        if (
          text(existing, "kind") === kind &&
          text(existing, "severity") === severity &&
          text(existing, "payload_json") === payloadJson
        ) {
          return this.#attention(existing);
        }
        this.#database.prepare(`
          UPDATE attention_items
             SET kind=?, severity=?, payload_json=?, revision=revision+1, updated_at=?
           WHERE item_id=?
        `).run(kind, severity, payloadJson, this.#clock(), text(existing, "item_id"));
        this.#fault("attention:upsert:after-item");
        this.#database.prepare(`
          UPDATE notification_deliveries
             SET state='deduplicated', updated_at=?
           WHERE item_id=? AND item_revision<? AND state='pending'
        `).run(this.#clock(), text(existing, "item_id"), integer(existing, "revision") + 1);
        this.#fault("attention:upsert:after-supersede");
        return this.getAttention(text(existing, "item_id"));
      }
      const itemId = `attention_${sha256(`${context.projectSessionId}\0${dedupeKey}`).slice(0, 24)}`;
      const now = this.#clock();
      this.#database.prepare(`
        INSERT INTO attention_items(
          item_id, project_session_id, coordination_run_id, kind, severity,
          revision, state, dedupe_key, payload_json, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, 1, 'open', ?, ?, ?, ?)
      `).run(
        itemId,
        context.projectSessionId,
        runId,
        kind,
        severity,
        dedupeKey,
        payloadJson,
        now,
        now,
      );
      this.#fault("attention:upsert:after-item");
      return this.getAttention(itemId);
    });
    return execute();
  }

  settleAttention(
    context: AttentionProducerContext,
    request: AttentionSettleRequest,
  ): AttentionItemRecord {
    const execute = this.#database.transaction((): AttentionItemRecord => {
      this.#assertProducerContext(context);
      const item = this.#attentionRow(request.itemId);
      this.#assertItemContext(context, item);
      if (integer(item, "revision") !== request.expectedRevision) {
        throw new ProjectFabricCoreError("STALE_REVISION", "attention revision changed before settlement");
      }
      if (text(item, "state") !== "open" && text(item, "state") !== "acknowledged") {
        throw new ProjectFabricCoreError("CONFLICT", "only open Attention may be settled");
      }
      this.#requiredText(request.reason, "attention settlement reason");
      const now = this.#clock();
      this.#database.prepare(`
        UPDATE notification_deliveries
           SET state='deduplicated', updated_at=?
         WHERE item_id=? AND state='pending'
      `).run(now, request.itemId);
      this.#fault("attention:settle:after-deliveries");
      const updated = this.#database.prepare(`
        UPDATE attention_items
           SET state=?, revision=revision+1, updated_at=?
         WHERE item_id=? AND revision=? AND state IN ('open','acknowledged')
      `).run(request.state, now, request.itemId, request.expectedRevision);
      if (updated.changes !== 1) {
        throw new ProjectFabricCoreError("STALE_REVISION", "attention changed during settlement");
      }
      this.#fault("attention:settle:after-item");
      return this.getAttention(request.itemId);
    });
    return execute();
  }

  enqueue(
    context: AttentionProducerContext,
    request: NotificationEnqueueRequest,
  ): NotificationDeliveryRecord {
    const execute = this.#database.transaction((): NotificationDeliveryRecord => {
      this.#assertProducerContext(context);
      const item = this.#attentionRow(request.itemId);
      this.#assertItemContext(context, item);
      if (integer(item, "revision") !== request.expectedItemRevision) {
        throw new ProjectFabricCoreError("STALE_REVISION", "attention revision changed before notification enqueue");
      }
      if (text(item, "state") !== "open") {
        throw new ProjectFabricCoreError("CONFLICT", "notification enqueue requires open Attention");
      }
      const integration = this.#requiredText(request.targetIntegration, "target integration");
      const existing = this.#database.prepare(`
        SELECT notification_id FROM notification_deliveries
         WHERE item_id=? AND item_revision=? AND target_integration=?
      `).get(request.itemId, request.expectedItemRevision, integration);
      if (isRow(existing)) return this.get(text(existing, "notification_id"));
      const identity = `${request.itemId}\0${String(request.expectedItemRevision)}\0${integration}`;
      const notificationId = `notification_${sha256(identity).slice(0, 24)}`;
      const dedupeKey = `notification:${sha256(identity)}`;
      this.#database.prepare(`
        INSERT INTO notification_deliveries(
          notification_id, item_id, item_revision, target_integration,
          dedupe_key, state, claim_generation, updated_at
        ) VALUES (?, ?, ?, ?, ?, 'pending', 0, ?)
      `).run(
        notificationId,
        request.itemId,
        request.expectedItemRevision,
        integration,
        dedupeKey,
        this.#clock(),
      );
      this.#fault("attention:enqueue:after-delivery");
      return this.get(notificationId);
    });
    return execute();
  }

  setIntegrationAvailability(
    context: NotificationWorkerContext,
    request: IntegrationAvailabilityRequest,
  ): void {
    this.#assertWorkerContext(context);
    const contractJson = canonicalJson(request.discoveredContract);
    const execute = this.#database.transaction(() => {
      this.#database.prepare(`
        INSERT INTO integration_availability(integration_id, state, discovered_contract_json, checked_at)
        VALUES (?, ?, ?, ?)
        ON CONFLICT(integration_id) DO UPDATE SET
          state=excluded.state,
          discovered_contract_json=excluded.discovered_contract_json,
          checked_at=excluded.checked_at
        WHERE integration_availability.state IS NOT excluded.state
           OR integration_availability.discovered_contract_json IS NOT excluded.discovered_contract_json
      `).run(context.integrationId, request.state, contractJson, this.#clock());
      this.#fault("attention:availability:after-update");
    });
    execute();
  }

  claim(
    context: NotificationWorkerContext,
    request: NotificationClaimRequest,
  ): ClaimedNotificationDelivery {
    const execute = this.#database.transaction((): ClaimedNotificationDelivery => {
      this.#assertWorkerContext(context);
      const delivery = this.#deliveryRow(request.notificationId);
      this.#assertTargetIntegration(context, delivery);
      if (integer(delivery, "item_revision") !== request.expectedItemRevision) {
        throw new ProjectFabricCoreError("STALE_REVISION", "notification item revision changed");
      }
      const deadline = timestampToMillis(request.claimDeadline);
      const expectedEffectIdentityHash = `sha256:${sha256(canonicalJson({
        notificationId: request.notificationId,
        itemRevision: request.expectedItemRevision,
        claimGeneration: request.expectedClaimGeneration + 1,
        integrationId: context.integrationId,
        workerInstanceId: context.workerInstanceId,
      }))}`;
      const state = text(delivery, "state");
      if (
        state === "claimed" &&
        integer(delivery, "claim_generation") === request.expectedClaimGeneration + 1 &&
        integer(delivery, "claim_deadline") === deadline &&
        nullableText(delivery, "effect_identity_hash") === expectedEffectIdentityHash
      ) {
        return this.#claimed(this.get(request.notificationId));
      }
      this.#assertAvailable(context.integrationId);
      const item = this.#attentionRow(text(delivery, "item_id"));
      if (integer(item, "revision") !== request.expectedItemRevision) {
        throw new ProjectFabricCoreError("STALE_REVISION", "notification no longer targets the current attention revision");
      }
      if (text(item, "state") !== "open") {
        throw new ProjectFabricCoreError("CONFLICT", "notification claim requires open Attention");
      }
      if (deadline <= this.#clock()) {
        throw new ProjectFabricCoreError("DEADLINE_EXCEEDED", "notification claim deadline is expired");
      }
      if (state !== "pending") {
        throw new ProjectFabricCoreError("CONFLICT", "only a pending notification may be claimed");
      }
      if (integer(delivery, "claim_generation") !== request.expectedClaimGeneration) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "notification claim generation changed");
      }
      const claimGeneration = request.expectedClaimGeneration + 1;
      this.#database.prepare(`
        UPDATE notification_deliveries
           SET state='claimed', claim_generation=?, claim_deadline=?,
               effect_identity_hash=?, updated_at=?
         WHERE notification_id=? AND state='pending' AND claim_generation=?
      `).run(
        claimGeneration,
        deadline,
        expectedEffectIdentityHash,
        this.#clock(),
        request.notificationId,
        request.expectedClaimGeneration,
      );
      this.#fault("attention:claim:after-delivery");
      this.#database.prepare(`
        INSERT INTO notification_attempts(notification_id, attempt, state, detail_json, created_at)
        VALUES (?, ?, 'claimed', ?, ?)
      `).run(
        request.notificationId,
        claimGeneration,
        canonicalJson({
          workerInstanceId: context.workerInstanceId,
          integrationId: context.integrationId,
          itemRevision: request.expectedItemRevision,
          claimDeadline: request.claimDeadline,
          effectIdentityHash: expectedEffectIdentityHash,
        }),
        this.#clock(),
      );
      this.#fault("attention:claim:after-attempt");
      return this.#claimed(this.get(request.notificationId));
    });
    return execute();
  }

  recordOutcome(
    context: NotificationWorkerContext,
    request: NotificationOutcomeRequest,
  ): NotificationDeliveryRecord {
    const execute = this.#database.transaction((): NotificationDeliveryRecord => {
      this.#assertWorkerContext(context);
      const delivery = this.#deliveryRow(request.notificationId);
      this.#assertTargetIntegration(context, delivery);
      const detailJson = canonicalJson({
        outcome: request.outcome,
        effectIdentityHash: request.effectIdentityHash,
        detail: request.detail,
      });
      const attempt = this.#database.prepare(`
        SELECT state, detail_json FROM notification_attempts
         WHERE notification_id=? AND attempt=?
      `).get(request.notificationId, request.claimGeneration);
      if (
        text(delivery, "state") === request.outcome &&
        integer(delivery, "claim_generation") === request.claimGeneration &&
        nullableText(delivery, "effect_identity_hash") === request.effectIdentityHash &&
        isRow(attempt) &&
        text(attempt, "state") === request.outcome &&
        text(attempt, "detail_json") === detailJson
      ) {
        return this.get(request.notificationId);
      }
      if (
        text(delivery, "state") !== "claimed" ||
        integer(delivery, "claim_generation") !== request.claimGeneration ||
        nullableText(delivery, "effect_identity_hash") !== request.effectIdentityHash
      ) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "notification effect claim changed");
      }
      if (!isRow(attempt) || text(attempt, "state") !== "claimed") {
        throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "notification claim attempt is missing or terminal");
      }
      this.#database.prepare(`
        UPDATE notification_deliveries
           SET state=?, claim_deadline=NULL, updated_at=?
         WHERE notification_id=? AND state='claimed' AND claim_generation=?
      `).run(request.outcome, this.#clock(), request.notificationId, request.claimGeneration);
      this.#fault("attention:outcome:after-delivery");
      const update = this.#database.prepare(`
        UPDATE notification_attempts SET state=?, detail_json=?
         WHERE notification_id=? AND attempt=? AND state='claimed'
      `).run(
        request.outcome,
        detailJson,
        request.notificationId,
        request.claimGeneration,
      );
      if (update.changes !== 1) {
        throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "notification outcome journal did not advance once");
      }
      this.#fault("attention:outcome:after-attempt");
      return this.get(request.notificationId);
    });
    return execute();
  }

  retryFailed(
    context: NotificationWorkerContext,
    request: NotificationRetryRequest,
  ): NotificationDeliveryRecord {
    const execute = this.#database.transaction((): NotificationDeliveryRecord => {
      this.#assertWorkerContext(context);
      const delivery = this.#deliveryRow(request.notificationId);
      this.#assertTargetIntegration(context, delivery);
      const item = this.#attentionRow(text(delivery, "item_id"));
      if (
        text(item, "state") !== "open" ||
        integer(item, "revision") !== integer(delivery, "item_revision")
      ) {
        throw new ProjectFabricCoreError("CONFLICT", "notification retry requires current open Attention");
      }
      const reason = this.#requiredText(request.reason, "notification retry reason");
      if (integer(delivery, "claim_generation") !== request.expectedClaimGeneration) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "notification retry generation changed");
      }
      const attempt = row(this.#database.prepare(`
        SELECT state, detail_json FROM notification_attempts
         WHERE notification_id=? AND attempt=?
      `).get(request.notificationId, request.expectedClaimGeneration), "notification attempt");
      const previousDetail = JSON.parse(text(attempt, "detail_json")) as unknown;
      const retry = { workerInstanceId: context.workerInstanceId, reason };
      if (text(delivery, "state") === "pending") {
        if (!isRow(previousDetail) || canonicalJson(previousDetail.retry) !== canonicalJson(retry)) {
          throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "notification retry input changed on replay");
        }
        return this.get(request.notificationId);
      }
      if (text(delivery, "state") !== "failed" || text(attempt, "state") !== "failed") {
        throw new ProjectFabricCoreError("CONFLICT", "only a proved failed notification may be retried");
      }
      const replayDetail = isRow(previousDetail)
        ? { ...previousDetail, retry }
        : { outcome: "failed", detail: previousDetail, retry };
      this.#database.prepare(`
        UPDATE notification_deliveries
           SET state='pending', claim_deadline=NULL, effect_identity_hash=NULL, updated_at=?
         WHERE notification_id=? AND state='failed' AND claim_generation=?
      `).run(this.#clock(), request.notificationId, request.expectedClaimGeneration);
      this.#database.prepare(`
        UPDATE notification_attempts SET detail_json=?
         WHERE notification_id=? AND attempt=? AND state='failed'
      `).run(
        canonicalJson(replayDetail),
        request.notificationId,
        request.expectedClaimGeneration,
      );
      this.#fault("attention:retry:after-reset");
      return this.get(request.notificationId);
    });
    return execute();
  }

  reconcileAmbiguous(
    context: NotificationWorkerContext,
    request: NotificationAmbiguousReconciliationRequest,
  ): NotificationDeliveryRecord {
    const execute = this.#database.transaction((): NotificationDeliveryRecord => {
      this.#assertWorkerContext(context);
      const delivery = this.#deliveryRow(request.notificationId);
      this.#assertTargetIntegration(context, delivery);
      if (
        integer(delivery, "claim_generation") !== request.claimGeneration ||
        nullableText(delivery, "effect_identity_hash") !== request.effectIdentityHash
      ) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "ambiguous notification effect identity changed");
      }
      const evidence = JSON.parse(canonicalJson(request.evidence)) as unknown;
      if (!isRow(evidence) || Object.keys(evidence).length === 0) {
        throw new ProjectFabricCoreError("PROTOCOL_INVALID", "notification reconciliation evidence is required");
      }
      const reconciliation = {
        workerInstanceId: context.workerInstanceId,
        outcome: request.outcome,
        effectIdentityHash: request.effectIdentityHash,
        evidence,
      };
      const attempt = row(this.#database.prepare(`
        SELECT state, detail_json FROM notification_attempts
         WHERE notification_id=? AND attempt=?
      `).get(request.notificationId, request.claimGeneration), "notification attempt");
      const priorDetail = JSON.parse(text(attempt, "detail_json")) as unknown;
      if (text(delivery, "state") === request.outcome && text(attempt, "state") === request.outcome) {
        if (
          !isRow(priorDetail) ||
          !isRow(priorDetail.reconciliation) ||
          canonicalJson(priorDetail.reconciliation) !== canonicalJson(reconciliation)
        ) {
          throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "notification reconciliation changed on replay");
        }
        return this.get(request.notificationId);
      }
      if (text(delivery, "state") !== "ambiguous" || text(attempt, "state") !== "ambiguous") {
        throw new ProjectFabricCoreError("CONFLICT", "only an ambiguous notification effect may be reconciled");
      }
      this.#database.prepare(`
        UPDATE notification_deliveries SET state=?, updated_at=?
         WHERE notification_id=? AND state='ambiguous' AND claim_generation=?
      `).run(request.outcome, this.#clock(), request.notificationId, request.claimGeneration);
      this.#fault("attention:reconcile:after-delivery");
      this.#database.prepare(`
        UPDATE notification_attempts SET state=?, detail_json=?
         WHERE notification_id=? AND attempt=? AND state='ambiguous'
      `).run(
        request.outcome,
        canonicalJson({ prior: priorDetail, reconciliation }),
        request.notificationId,
        request.claimGeneration,
      );
      this.#fault("attention:reconcile:after-attempt");
      return this.get(request.notificationId);
    });
    return execute();
  }

  recover(): Readonly<{ ambiguousClaims: number }> {
    const execute = this.#database.transaction(() => {
      const now = this.#clock();
      const expired = this.#database.prepare(`
        SELECT notification_id, claim_generation, effect_identity_hash
          FROM notification_deliveries
         WHERE state='claimed' AND claim_deadline IS NOT NULL AND claim_deadline<=?
      `).all(now).filter(isRow);
      for (const delivery of expired) {
        const notificationId = text(delivery, "notification_id");
        const generation = integer(delivery, "claim_generation");
        this.#database.prepare(`
          UPDATE notification_deliveries
             SET state='ambiguous', claim_deadline=NULL, updated_at=?
           WHERE notification_id=? AND state='claimed' AND claim_generation=?
        `).run(now, notificationId, generation);
        const attempt = row(this.#database.prepare(`
          SELECT detail_json FROM notification_attempts
           WHERE notification_id=? AND attempt=?
        `).get(notificationId, generation), "notification attempt");
        const original = JSON.parse(text(attempt, "detail_json")) as unknown;
        this.#database.prepare(`
          UPDATE notification_attempts SET state='ambiguous', detail_json=?
           WHERE notification_id=? AND attempt=? AND state='claimed'
        `).run(
          canonicalJson({
            original,
            recovery: {
              outcome: "ambiguous",
              recoveredAt: new Date(now).toISOString(),
              effectIdentityHash: nullableText(delivery, "effect_identity_hash"),
            },
          }),
          notificationId,
          generation,
        );
      }
      this.#fault("attention:recover:after-ambiguity");
      return { ambiguousClaims: expired.length };
    });
    return execute();
  }

  pendingPage(
    context: NotificationWorkerContext,
    request: Readonly<{ limit: number }>,
  ): readonly PendingNotificationWork[] {
    this.#assertWorkerContext(context);
    if (!Number.isSafeInteger(request.limit) || request.limit < 1 || request.limit > 100) {
      throw new ProjectFabricCoreError("PROTOCOL_INVALID", "notification page limit must be between 1 and 100");
    }
    const rows = this.#database.prepare(`
      SELECT d.notification_id
        FROM notification_deliveries d
        JOIN attention_items i ON i.item_id=d.item_id
        JOIN project_sessions s ON s.project_session_id=i.project_session_id
       WHERE d.target_integration=?
         AND d.state='pending'
         AND i.state='open'
         AND i.revision=d.item_revision
         AND s.state IN (${NOTIFICATION_ACTIVE_SESSION_STATES.map(() => "?").join(",")})
       ORDER BY d.updated_at, d.notification_id
       LIMIT ?
    `).all(context.integrationId, ...NOTIFICATION_ACTIVE_SESSION_STATES, request.limit).filter(isRow);
    return rows.map((value): PendingNotificationWork => {
      const delivery = this.get(text(value, "notification_id"));
      if (delivery.state !== "pending") {
        throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "pending notification page changed while reading");
      }
      return {
        delivery: { ...delivery, state: "pending" },
        attention: this.getAttention(delivery.itemId),
      };
    });
  }

  getAttention(itemId: string): AttentionItemRecord {
    return this.#attention(this.#attentionRow(itemId));
  }

  get(notificationId: string): NotificationDeliveryRecord {
    const delivery = row(this.#database.prepare(`
      SELECT d.*, a.state AS availability_state
        FROM notification_deliveries d
        LEFT JOIN integration_availability a ON a.integration_id=d.target_integration
       WHERE d.notification_id=?
    `).get(notificationId), "notification delivery");
    const claimDeadline = delivery.claim_deadline === null
      ? null
      : new Date(integer(delivery, "claim_deadline")).toISOString();
    return {
      notificationId: text(delivery, "notification_id"),
      itemId: text(delivery, "item_id"),
      itemRevision: integer(delivery, "item_revision"),
      targetIntegration: text(delivery, "target_integration"),
      dedupeKey: text(delivery, "dedupe_key"),
      state: text(delivery, "state") as NotificationDeliveryState,
      claimGeneration: integer(delivery, "claim_generation"),
      claimDeadline,
      effectIdentityHash: nullableText(delivery, "effect_identity_hash"),
      availability: delivery.availability_state === null
        ? "unavailable"
        : text(delivery, "availability_state") as IntegrationAvailability,
      updatedAt: new Date(integer(delivery, "updated_at")).toISOString(),
    };
  }

  exactFocusAction(notificationId: string): Readonly<{
    integrationId: string;
    action: "focus-attention-item";
    itemId: string;
    itemRevision: number;
    projectSessionId: string;
  }> | null {
    const value = row(this.#database.prepare(`
      SELECT d.item_id, d.item_revision, d.target_integration,
             i.project_session_id, a.state AS availability_state,
             a.discovered_contract_json
        FROM notification_deliveries d
        JOIN attention_items i ON i.item_id=d.item_id
        LEFT JOIN integration_availability a ON a.integration_id=d.target_integration
       WHERE d.notification_id=?
    `).get(notificationId), "notification delivery");
    if (value.availability_state !== "available" || typeof value.discovered_contract_json !== "string") {
      return null;
    }
    const contract = JSON.parse(value.discovered_contract_json) as unknown;
    if (!isRow(contract) || !isRow(contract.exactAttentionFocus)) return null;
    if (
      contract.exactAttentionFocus.supported !== true ||
      contract.exactAttentionFocus.contractTested !== true
    ) return null;
    return {
      integrationId: text(value, "target_integration"),
      action: "focus-attention-item",
      itemId: text(value, "item_id"),
      itemRevision: integer(value, "item_revision"),
      projectSessionId: text(value, "project_session_id"),
    };
  }

  #assertProducerContext(context: AttentionProducerContext): void {
    this.#requiredText(context.producerId, "attention producer ID");
    if (!Number.isSafeInteger(context.principalGeneration) || context.principalGeneration < 1) {
      throw new ProjectFabricCoreError("STALE_GENERATION", "attention producer generation is invalid");
    }
    const session = row(this.#database.prepare(`
      SELECT project_id FROM project_sessions WHERE project_session_id=?
    `).get(context.projectSessionId), "project session");
    if (text(session, "project_id") !== context.projectId) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "attention producer is bound to another project");
    }
    if (context.coordinationRunId !== undefined && context.coordinationRunId !== null) {
      const run = this.#database.prepare(`
        SELECT 1 FROM runs WHERE project_session_id=? AND run_id=?
      `).get(context.projectSessionId, context.coordinationRunId);
      if (!isRow(run)) throw new ProjectFabricCoreError("WRONG_PROJECT", "attention run is outside the project session");
    }
  }

  #assertWorkerContext(context: NotificationWorkerContext): void {
    this.#requiredText(context.workerInstanceId, "notification worker instance ID");
    this.#requiredText(context.integrationId, "notification integration ID");
  }

  #assertItemContext(context: AttentionProducerContext, item: Row): void {
    if (
      text(item, "project_session_id") !== context.projectSessionId ||
      nullableText(item, "coordination_run_id") !== (context.coordinationRunId ?? null)
    ) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "attention item is outside the producer context");
    }
  }

  #assertTargetIntegration(context: NotificationWorkerContext, delivery: Row): void {
    if (text(delivery, "target_integration") !== context.integrationId) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "notification is assigned to another integration");
    }
  }

  #assertAvailable(integrationId: string): void {
    const availability = this.#database.prepare(`
      SELECT state FROM integration_availability WHERE integration_id=?
    `).get(integrationId);
    if (!isRow(availability) || text(availability, "state") !== "available") {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "notification integration is not available");
    }
  }

  #attentionRow(itemId: string): Row {
    return row(this.#database.prepare(`
      SELECT * FROM attention_items WHERE item_id=?
    `).get(itemId), "attention item");
  }

  #deliveryRow(notificationId: string): Row {
    return row(this.#database.prepare(`
      SELECT * FROM notification_deliveries WHERE notification_id=?
    `).get(notificationId), "notification delivery");
  }

  #attention(value: Row): AttentionItemRecord {
    return {
      itemId: text(value, "item_id"),
      projectSessionId: text(value, "project_session_id"),
      coordinationRunId: nullableText(value, "coordination_run_id"),
      kind: text(value, "kind"),
      severity: text(value, "severity"),
      revision: integer(value, "revision"),
      state: text(value, "state") as AttentionItemRecord["state"],
      dedupeKey: text(value, "dedupe_key"),
      payload: JSON.parse(text(value, "payload_json")) as unknown,
      createdAt: new Date(integer(value, "created_at")).toISOString(),
      updatedAt: new Date(integer(value, "updated_at")).toISOString(),
    };
  }

  #claimed(value: NotificationDeliveryRecord): ClaimedNotificationDelivery {
    if (
      value.state !== "claimed" ||
      value.claimDeadline === null ||
      value.effectIdentityHash === null
    ) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "notification claim projection is incomplete");
    }
    return {
      ...value,
      state: "claimed",
      claimDeadline: value.claimDeadline,
      effectIdentityHash: value.effectIdentityHash,
    };
  }

  #requiredText(value: string, label: string): string {
    if (typeof value !== "string" || value.trim().length === 0) {
      throw new ProjectFabricCoreError("PROTOCOL_INVALID", `${label} is required`);
    }
    return value;
  }
}
