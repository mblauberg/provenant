import {
  FABRIC_OPERATIONS,
  parseResultDelivery,
  parseTaskCompleteWithReply,
  parseTaskRequest,
  type ResultDelivery,
  type ResultDeliveryAbandonRequest,
  type ResultDeliveryClaimRequest,
  type ResultDeliveryConsumeRequest,
  type ResultDeliveryProviderAcceptRequest,
  type ResultDeliveryReassignRequest,
  type ResultDeliveryRetryRequest,
  type TaskCompleteWithReply,
  type TaskCompletionCommit,
  type TaskRequest,
  type TaskRequestCommit,
} from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ArtifactRegistry } from "../artifacts/registry.js";
import {
  ProjectFabricCoreError,
  type AuthenticatedAgentContext,
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
import {
  ProjectSessionMembershipStore,
  touchProjectSessionMembershipRevision,
} from "../project-session/membership-store.js";
import { assertRunAcceptingWork, assertTaskOperationAdmitted } from "../operator/task-run-admission.js";
import { NotificationOutbox } from "../attention/outbox.js";
import { assertScopedOperationAllowed } from "../gates/store.js";

export type ResultDeliveryIntegrationContext = Readonly<{
  integrationId: string;
  projectId: string;
  projectSessionId: string;
  coordinationRunId: string;
  principalGeneration: number;
}>;

type DeliveryContext = AuthenticatedAgentContext | ResultDeliveryIntegrationContext;

export type DeliveryRecoveryResult = Readonly<{
  returnedClaims: number;
  overdueDeliveries: number;
  overdueRequests: number;
}>;

export type ResultDeadlinePassInput = Readonly<{
  daemonInstanceGeneration: number;
  passGeneration: number;
}>;

export type ResultDeadlinePassResult = Readonly<{
  daemonInstanceGeneration: number;
  passGeneration: number;
  overdueDeliveries: number;
  overdueRequests: number;
  attentionItems: number;
  notificationsEnqueued: number;
}>;

export type ResultDeliveryProviderActionBinding = Readonly<{
  kind: "result-delivery";
  resultDeliveryId: string;
  callbackId: string;
  claimGeneration: number;
  assignmentGeneration: number;
  targetAgentId: string;
  targetProviderSessionRef: string;
  payloadDigest: string;
  requestRevision: number;
  replyRevision: number;
  taskRevision: number;
}>;

export function resultDeliveryProviderActionBinding(
  delivery: ResultDelivery,
): ResultDeliveryProviderActionBinding {
  if (delivery.state !== "claimed" && delivery.state !== "provider-accepted") {
    throw new ProjectFabricCoreError("CONFLICT", "provider action binding requires a claimed result delivery");
  }
  return {
    kind: "result-delivery",
    resultDeliveryId: delivery.resultDeliveryId,
    callbackId: delivery.callbackId,
    claimGeneration: delivery.claimGeneration,
    assignmentGeneration: delivery.assignmentGeneration,
    targetAgentId: delivery.targetAgentId,
    targetProviderSessionRef: delivery.targetProviderSessionRef,
    payloadDigest: delivery.payloadDigest,
    requestRevision: delivery.requestRevision,
    replyRevision: delivery.replyRevision,
    taskRevision: delivery.taskRevision,
  };
}

export class AtomicDeliveryStore {
  readonly #database: Database.Database;
  readonly #clock: () => number;
  readonly #fault: (label: string) => void;
  readonly #artifactRegistry: ArtifactRegistry;
  readonly #memberships: ProjectSessionMembershipStore;
  readonly #notifications: NotificationOutbox;

  constructor(options: CoreServiceOptions & {
    artifactRegistry?: ArtifactRegistry;
    memberships?: ProjectSessionMembershipStore;
    notifications?: NotificationOutbox;
  }) {
    this.#database = options.database;
    this.#clock = options.clock ?? Date.now;
    this.#fault = options.fault ?? (() => undefined);
    this.#artifactRegistry = options.artifactRegistry ?? new ArtifactRegistry(this.#database, this.#clock);
    this.#memberships = options.memberships ?? new ProjectSessionMembershipStore({
      database: this.#database,
      clock: this.#clock,
    });
    this.#notifications = options.notifications ?? new NotificationOutbox({
      database: this.#database,
      clock: this.#clock,
    });
  }

  request(context: AuthenticatedAgentContext, value: TaskRequest): TaskRequestCommit {
    const request = parseTaskRequest(value);
    this.#assertAgentContext(context, request.projectSessionId, request.coordinationRunId);
    return this.#executeAgentCommand(context, request.commandId, request, () => {
      assertRunAcceptingWork(this.#database, context.coordinationRunId);
      assertScopedOperationAllowed(
        this.#database,
        context.coordinationRunId,
        FABRIC_OPERATIONS.taskRequest,
        { kind: "run" },
      );
      if (request.task.taskRevision !== 1 || request.request.requestRevision !== 1) {
        throw new ProjectFabricCoreError("PROTOCOL_INVALID", "new task and request revisions must start at one");
      }
      if (this.#database.prepare(`
        SELECT 1 FROM tasks WHERE run_id=? AND task_id=?
      `).get(context.coordinationRunId, request.task.taskId) !== undefined) {
        throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "task ID already exists outside this command");
      }
      const target = this.#agent(context.coordinationRunId, request.request.targetAgentId);
      const targetProvider = nullableText(target, "provider_session_ref");
      if (targetProvider !== request.request.targetProviderSessionRef) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "target provider session changed before request commit");
      }
      const requester = this.#agent(context.coordinationRunId, context.agentId);
      const authorityId = text(requester, "authority_id");
      const now = this.#clock();
      this.#database.prepare(`
        INSERT INTO tasks(
          run_id, task_id, authority_id, objective, base_revision, state,
          owner_agent_id, revision, owner_lease_generation, created_by
        ) VALUES (?, ?, ?, ?, ?, 'active', ?, 1, 1, ?)
      `).run(
        context.coordinationRunId,
        request.task.taskId,
        authorityId,
        request.task.objective,
        request.task.baseRevision,
        request.request.targetAgentId,
        context.agentId,
      );
      for (const path of request.task.expectedArtifactPaths) {
        this.#database.prepare(`
          INSERT INTO task_expected_artifacts(run_id, task_id, relative_path) VALUES (?, ?, ?)
        `).run(context.coordinationRunId, request.task.taskId, path);
      }
      this.#database.prepare(`
        INSERT INTO task_owner_leases(
          project_session_id, run_id, task_id, lease_id, holder_agent_id,
          generation, status, updated_at
        ) VALUES (?, ?, ?, ?, ?, 1, 'active', ?)
      `).run(
        context.projectSessionId,
        context.coordinationRunId,
        request.task.taskId,
        `task-owner:${context.coordinationRunId}:${request.task.taskId}:1`,
        request.request.targetAgentId,
        now,
      );
      this.#fault("results:request:after-task");

      const payloadHash = sha256(canonicalJson(request));
      this.#database.prepare(`
        INSERT INTO messages(
          message_id, run_id, sender_id, dedupe_key, payload_hash, audience_json,
          kind, body, requires_ack, conversation_id, reply_to_message_id,
          task_revision, hop_count, expires_at, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, 'request', ?, 1, ?, NULL, ?, 0, ?, ?)
      `).run(
        request.request.messageId,
        context.coordinationRunId,
        context.agentId,
        request.request.dedupeKey,
        payloadHash,
        canonicalJson({ kind: "agent", agentId: request.request.targetAgentId }),
        request.task.objective,
        request.request.conversationId,
        request.task.taskRevision,
        timestampToMillis(request.request.responseDeadline),
        now,
      );
      this.#database.prepare(`
        INSERT INTO message_contexts(message_id, context_json) VALUES (?, ?)
      `).run(request.request.messageId, canonicalJson(request));
      this.#fault("results:request:after-message");

      const deliveryId = this.#deliverMessage(
        context.coordinationRunId,
        request.request.messageId,
        request.request.targetAgentId,
      );
      this.#fault("results:request:after-mailbox");
      this.#database.prepare(`
        INSERT INTO task_requests(
          request_id, project_session_id, run_id, task_id, requester_agent_id,
          request_revision, conversation_id, request_message_id, target_agent_id,
          target_provider_session, expected_artifacts_json, acknowledgement_required,
          dedupe_key, response_deadline, callback_id, callback_generation,
          dependent_barrier_id, state, payload_digest, created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, 1, ?, ?, ?, 1, ?, 'pending', ?, ?, ?)
      `).run(
        request.request.messageId,
        context.projectSessionId,
        context.coordinationRunId,
        request.task.taskId,
        context.agentId,
        request.request.conversationId,
        request.request.messageId,
        request.request.targetAgentId,
        request.request.targetProviderSessionRef,
        canonicalJson(request.task.expectedArtifactPaths),
        request.request.dedupeKey,
        timestampToMillis(request.request.responseDeadline),
        request.request.callbackId,
        request.request.dependentBarrierId,
        `sha256:${payloadHash}`,
        now,
        now,
      );
      this.#database.prepare(`
        INSERT INTO task_request_recipients(request_id, delivery_id) VALUES (?, ?)
      `).run(request.request.messageId, deliveryId);
      this.#database.prepare(`
        INSERT INTO task_request_barriers(request_id, barrier_id, state)
        VALUES (?, ?, 'blocked')
      `).run(request.request.messageId, request.request.dependentBarrierId);
      this.#bindMemberships(context, request);
      this.#fault("results:request:after-request");
      return {
        taskRevision: 1,
        requestRevision: 1,
        callbackId: request.request.callbackId,
        callbackGeneration: request.request.callbackGeneration,
      };
    });
  }

  completeWithReply(
    context: AuthenticatedAgentContext,
    value: TaskCompleteWithReply,
  ): TaskCompletionCommit {
    const request = parseTaskCompleteWithReply(value);
    this.#assertAgentContext(context, context.projectSessionId, context.coordinationRunId);
    return this.#executeAgentCommand(context, request.commandId, request, () => {
      assertScopedOperationAllowed(
        this.#database,
        context.coordinationRunId,
        FABRIC_OPERATIONS.taskCompleteWithReply,
        { kind: "task", taskId: request.taskId },
      );
      assertTaskOperationAdmitted(this.#database, context.coordinationRunId, request.taskId);
      const pending = row(this.#database.prepare(`
        SELECT * FROM task_requests WHERE request_message_id=?
      `).get(request.requestMessageId), "task request");
      this.#assertCompletionOwnership(context, request, pending);
      const now = this.#clock();
      const replyPayloadHash = sha256(canonicalJson(request.reply));
      this.#database.prepare(`
        INSERT INTO messages(
          message_id, run_id, sender_id, dedupe_key, payload_hash, audience_json,
          kind, body, requires_ack, conversation_id, reply_to_message_id,
          task_revision, hop_count, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, 'response', ?, 1, ?, ?, ?, 0, ?)
      `).run(
        request.reply.messageId,
        context.coordinationRunId,
        context.agentId,
        `reply:${request.requestMessageId}:${request.reply.messageId}`,
        replyPayloadHash,
        canonicalJson({ kind: "agent", agentId: text(pending, "requester_agent_id") }),
        request.reply.body,
        request.reply.conversationId,
        request.reply.replyToMessageId,
        request.expectedTaskRevision + 1,
        now,
      );
      this.#database.prepare(`
        INSERT INTO message_contexts(message_id, context_json) VALUES (?, ?)
      `).run(request.reply.messageId, canonicalJson(request.reply));
      this.#deliverMessage(
        context.coordinationRunId,
        request.reply.messageId,
        text(pending, "requester_agent_id"),
      );
      this.#fault("results:complete:after-reply");

      const resultId = `result_${sha256(text(pending, "request_id")).slice(0, 24)}`;
      const payloadDigest = `sha256:${sha256(canonicalJson({
        reply: request.reply,
        terminalResult: request.terminalResult,
      }))}`;
      const artifactIds: string[] = [];
      for (const artifact of request.reply.artifactRefs) {
        const registered = this.#artifactRegistry.registerAgentEvidence({
          runId: context.coordinationRunId,
          agentId: context.agentId,
          taskId: request.taskId,
          requestedSourceKind: "run-file",
          evidenceKind: "artifact",
          relativePath: artifact.path,
          digest: artifact.digest,
          verifyBytes: false,
        });
        artifactIds.push(registered.evidenceId);
      }
      this.#database.prepare(`
        INSERT INTO task_results(
          result_id, request_id, project_session_id, run_id, task_id,
          task_revision, reply_message_id, reply_revision, payload_digest,
          artifacts_json, terminal_state, summary, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?, ?, 'complete', ?, ?)
      `).run(
        resultId,
        text(pending, "request_id"),
        context.projectSessionId,
        context.coordinationRunId,
        request.taskId,
        request.expectedTaskRevision + 1,
        request.reply.messageId,
        payloadDigest,
        canonicalJson(request.reply.artifactRefs),
        request.terminalResult.summary,
        timestampToMillis(request.terminalResult.completedAt),
      );
      this.#bindCompletionMemberships(context, request.reply.messageId, artifactIds);
      this.#fault("results:complete:after-task-result");

      const requester = this.#agent(context.coordinationRunId, text(pending, "requester_agent_id"));
      const requesterProvider = nullableText(requester, "provider_session_ref");
      if (requesterProvider === null) {
        throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "requester provider session is unavailable");
      }
      const responseDeadline = integer(pending, "response_deadline");
      const overdue = responseDeadline <= now || text(pending, "state") === "overdue";
      const deliveryId = `result_delivery_${sha256(text(pending, "callback_id")).slice(0, 24)}`;
      this.#database.prepare(`
        INSERT INTO result_deliveries(
          result_delivery_id, callback_id, request_id, result_id,
          project_session_id, run_id, task_id, requester_agent_id,
          target_provider_session, state, required, revision, claim_generation,
          assignment_generation, response_deadline, request_revision,
          reply_revision, task_revision, payload_digest, overdue_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, 1, 0, 1, ?, ?, 1, ?, ?, ?, ?)
      `).run(
        deliveryId,
        text(pending, "callback_id"),
        text(pending, "request_id"),
        resultId,
        context.projectSessionId,
        context.coordinationRunId,
        request.taskId,
        text(pending, "requester_agent_id"),
        requesterProvider,
        overdue ? "overdue" : "pending",
        responseDeadline,
        integer(pending, "request_revision"),
        request.expectedTaskRevision + 1,
        payloadDigest,
        overdue ? now : null,
        now,
      );
      this.#fault("results:complete:after-delivery");
      this.#database.prepare(`
        UPDATE tasks SET state='complete', revision=revision+1
         WHERE run_id=? AND task_id=? AND revision=? AND owner_agent_id=?
      `).run(context.coordinationRunId, request.taskId, request.expectedTaskRevision, context.agentId);
      this.#database.prepare(`
        UPDATE task_owner_leases SET status='released', updated_at=?
         WHERE lease_id=? AND generation=? AND status='active'
      `).run(now, request.ownerLeaseId, request.ownerLeaseGeneration);
      this.#database.prepare(`
        UPDATE task_requests SET state='answered', updated_at=? WHERE request_id=?
      `).run(now, text(pending, "request_id"));
      this.#memberships.reconcile(context.coordinationRunId, [
        { kind: "task", memberId: request.taskId },
      ]);
      this.#fault("results:complete:after-terminal-task");
      return {
        taskRevision: request.expectedTaskRevision + 1,
        replyRevision: 1,
        resultDelivery: this.get(deliveryId),
      };
    });
  }

  claim(context: DeliveryContext, request: ResultDeliveryClaimRequest): ResultDelivery {
    return this.#transition(context, request.commandId, "claim", request, () => {
      const delivery = this.#deliveryRow(request.resultDeliveryId);
      this.#assertDeliveryContext(context, delivery);
      this.#assertRevision(delivery, request.expectedRevision);
      if (text(delivery, "state") !== "pending") {
        throw new ProjectFabricCoreError("CONFLICT", "only a pending result may be claimed");
      }
      if (
        integer(delivery, "claim_generation") !== request.expectedClaimGeneration ||
        text(delivery, "requester_agent_id") !== request.claimantAgentId
      ) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "result claim generation or claimant changed");
      }
      const deadline = timestampToMillis(request.claimDeadline);
      if (deadline <= this.#clock()) throw new ProjectFabricCoreError("DEADLINE_EXCEEDED", "claim deadline is expired");
      this.#database.prepare(`
        UPDATE result_deliveries
           SET state='claimed', revision=revision+1,
               claim_generation=claim_generation+1, claimed_by=?, claim_deadline=?, updated_at=?
         WHERE result_delivery_id=? AND revision=? AND state='pending'
      `).run(
        request.claimantAgentId,
        deadline,
        this.#clock(),
        request.resultDeliveryId,
        request.expectedRevision,
      );
      return this.get(request.resultDeliveryId);
    });
  }

  providerAccept(
    context: ResultDeliveryIntegrationContext,
    request: ResultDeliveryProviderAcceptRequest,
  ): ResultDelivery {
    return this.#transition(context, request.commandId, "provider-accept", request, () => {
      const delivery = this.#deliveryRow(request.resultDeliveryId);
      this.#assertDeliveryContext(context, delivery);
      this.#assertRevision(delivery, request.expectedRevision);
      if (
        text(delivery, "state") !== "claimed" ||
        integer(delivery, "claim_generation") !== request.claimGeneration
      ) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "result claim is not current");
      }
      const action = row(this.#database.prepare(`
        SELECT * FROM provider_actions WHERE run_id=? AND adapter_id=? AND action_id=?
      `).get(
        context.coordinationRunId,
        request.providerAdapterId,
        request.providerActionId,
      ), "provider action");
      if (text(action, "status") !== "accepted" && text(action, "status") !== "terminal") {
        throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "provider action has not accepted the callback");
      }
      const currentTarget = this.#agent(
        context.coordinationRunId,
        text(delivery, "requester_agent_id"),
      );
      if (
        nullableText(action, "target_agent_id") !== text(delivery, "requester_agent_id") ||
        nullableText(currentTarget, "provider_session_ref") !== text(delivery, "target_provider_session")
      ) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "provider action target changed before callback acceptance");
      }
      const providerPayload = JSON.parse(text(action, "payload_json")) as unknown;
      const binding = isRow(providerPayload) ? providerPayload.fabricResultDelivery : undefined;
      const projected = this.get(request.resultDeliveryId);
      if (
        !isRow(binding) ||
        canonicalJson(binding) !== canonicalJson(resultDeliveryProviderActionBinding(projected))
      ) {
        throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "provider action is not bound to the exact result callback");
      }
      this.#database.prepare(`
        UPDATE result_deliveries
           SET state='provider-accepted', revision=revision+1,
               provider_adapter_id=?, provider_action_id=?, provider_accepted_at=?, updated_at=?
         WHERE result_delivery_id=? AND revision=? AND state='claimed'
      `).run(
        request.providerAdapterId,
        request.providerActionId,
        this.#clock(),
        this.#clock(),
        request.resultDeliveryId,
        request.expectedRevision,
      );
      return this.get(request.resultDeliveryId);
    });
  }

  consume(context: DeliveryContext, request: ResultDeliveryConsumeRequest): ResultDelivery {
    return this.#transition(context, request.commandId, "consume", request, () => {
      const delivery = this.#deliveryRow(request.resultDeliveryId);
      this.#assertDeliveryContext(context, delivery);
      this.#assertRevision(delivery, request.expectedRevision);
      if (
        text(delivery, "state") !== "provider-accepted" ||
        integer(delivery, "claim_generation") !== request.claimGeneration ||
        text(delivery, "callback_id") !== request.callbackId ||
        text(delivery, "payload_digest") !== request.payloadDigest
      ) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "callback consumption binding changed");
      }
      this.#database.prepare(`
        UPDATE result_deliveries
           SET state='consumed', revision=revision+1, consumed_at=?, updated_at=?
         WHERE result_delivery_id=? AND revision=? AND state='provider-accepted'
      `).run(this.#clock(), this.#clock(), request.resultDeliveryId, request.expectedRevision);
      this.#releaseBarrier(text(delivery, "request_id"), "released");
      return this.get(request.resultDeliveryId);
    });
  }

  retry(context: AuthenticatedAgentContext, request: ResultDeliveryRetryRequest): ResultDelivery {
    return this.#transition(context, request.commandId, "retry", request, () => {
      const delivery = this.#deliveryRow(request.resultDeliveryId);
      this.#assertDeliveryContext(context, delivery);
      this.#assertRevision(delivery, request.expectedRevision);
      if (text(delivery, "state") !== "overdue" || text(delivery, "callback_id") !== request.sameCallbackId) {
        throw new ProjectFabricCoreError("CONFLICT", "retry must retain the overdue callback identity");
      }
      if (request.reason.trim().length === 0) throw new ProjectFabricCoreError("DELIVERY_REASON_REQUIRED", "retry reason is required");
      this.#database.prepare(`
        UPDATE result_deliveries
           SET state='pending', revision=revision+1, overdue_at=NULL,
               retry_of_callback_id=callback_id, updated_at=?
         WHERE result_delivery_id=? AND revision=? AND state='overdue'
      `).run(this.#clock(), request.resultDeliveryId, request.expectedRevision);
      return this.get(request.resultDeliveryId);
    });
  }

  reassign(context: AuthenticatedAgentContext, request: ResultDeliveryReassignRequest): ResultDelivery {
    return this.#transition(context, request.commandId, "reassign", request, () => {
      const delivery = this.#deliveryRow(request.resultDeliveryId);
      this.#assertDeliveryContext(context, delivery);
      this.#assertRevision(delivery, request.expectedRevision);
      if (text(delivery, "state") !== "overdue") {
        throw new ProjectFabricCoreError("CONFLICT", "only an overdue result may be reassigned");
      }
      const target = this.#agent(context.coordinationRunId, request.targetAgentId);
      if (nullableText(target, "provider_session_ref") !== request.targetProviderSessionRef) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "reassignment provider session changed");
      }
      if (request.reason.trim().length === 0) throw new ProjectFabricCoreError("DELIVERY_REASON_REQUIRED", "reassignment reason is required");
      this.#database.prepare(`
        UPDATE result_deliveries
           SET state='pending', revision=revision+1, assignment_generation=assignment_generation+1,
               requester_agent_id=?, target_provider_session=?, overdue_at=NULL,
               reassignment_of_callback_id=callback_id, updated_at=?
         WHERE result_delivery_id=? AND revision=? AND state='overdue'
      `).run(
        request.targetAgentId,
        request.targetProviderSessionRef,
        this.#clock(),
        request.resultDeliveryId,
        request.expectedRevision,
      );
      return this.get(request.resultDeliveryId);
    });
  }

  abandon(context: AuthenticatedAgentContext, request: ResultDeliveryAbandonRequest): ResultDelivery {
    return this.#transition(context, request.commandId, "abandon", request, () => {
      const delivery = this.#deliveryRow(request.resultDeliveryId);
      this.#assertDeliveryContext(context, delivery);
      this.#assertRevision(delivery, request.expectedRevision);
      if (!["pending", "claimed", "provider-accepted", "overdue"].includes(text(delivery, "state"))) {
        throw new ProjectFabricCoreError("DELIVERY_ALREADY_RESOLVED", "result delivery is already terminal");
      }
      if (request.reason.trim().length === 0) throw new ProjectFabricCoreError("DELIVERY_REASON_REQUIRED", "abandon reason is required");
      this.#database.prepare(`
        UPDATE result_deliveries
           SET state='abandoned', revision=revision+1, abandoned_reason=?,
               abandoned_at=?, updated_at=?
         WHERE result_delivery_id=? AND revision=?
      `).run(
        request.reason,
        this.#clock(),
        this.#clock(),
        request.resultDeliveryId,
        request.expectedRevision,
      );
      this.#releaseBarrier(text(delivery, "request_id"), "abandoned");
      return this.get(request.resultDeliveryId);
    });
  }

  sweepDeadlines(input: ResultDeadlinePassInput): ResultDeadlinePassResult {
    if (
      !Number.isSafeInteger(input.daemonInstanceGeneration) ||
      input.daemonInstanceGeneration < 1 ||
      !Number.isSafeInteger(input.passGeneration) ||
      input.passGeneration < 1
    ) {
      throw new ProjectFabricCoreError("PROTOCOL_INVALID", "deadline sweep generations must be positive integers");
    }
    return this.#database.transaction(() => {
      const epochValue = this.#database.prepare(`
        SELECT instance_generation, state FROM daemon_runtime_epochs
         ORDER BY instance_generation DESC LIMIT 1
      `).get();
      if (
        !isRow(epochValue) ||
        integer(epochValue, "instance_generation") !== input.daemonInstanceGeneration ||
        text(epochValue, "state") !== "running"
      ) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "deadline sweep daemon generation is not current");
      }
      const previous = this.#database.prepare(`
        SELECT daemon_instance_generation, pass_generation, result_json
          FROM result_deadline_sweep_state WHERE singleton=1
      `).get();
      if (isRow(previous)) {
        const daemonGeneration = integer(previous, "daemon_instance_generation");
        const passGeneration = integer(previous, "pass_generation");
        if (
          daemonGeneration === input.daemonInstanceGeneration &&
          passGeneration === input.passGeneration
        ) {
          return this.#parseDeadlinePassResult(JSON.parse(text(previous, "result_json")));
        }
        if (
          input.daemonInstanceGeneration < daemonGeneration ||
          (input.daemonInstanceGeneration === daemonGeneration && input.passGeneration !== passGeneration + 1) ||
          (input.daemonInstanceGeneration > daemonGeneration && input.passGeneration !== 1)
        ) {
          throw new ProjectFabricCoreError("STALE_GENERATION", "deadline sweep pass generation is stale or discontinuous");
        }
      } else if (input.passGeneration !== 1) {
        throw new ProjectFabricCoreError("STALE_GENERATION", "first deadline sweep pass must start at generation one");
      }

      const counts = this.#applyDeadlineTransitions(
        this.#clock(),
        input.daemonInstanceGeneration,
      );
      const result = { ...input, ...counts };
      this.#database.prepare(`
        INSERT INTO result_deadline_sweep_state(
          singleton, daemon_instance_generation, pass_generation, result_json, completed_at
        ) VALUES (1, ?, ?, ?, ?)
        ON CONFLICT(singleton) DO UPDATE SET
          daemon_instance_generation=excluded.daemon_instance_generation,
          pass_generation=excluded.pass_generation,
          result_json=excluded.result_json,
          completed_at=excluded.completed_at
      `).run(
        input.daemonInstanceGeneration,
        input.passGeneration,
        canonicalJson(result),
        this.#clock(),
      );
      this.#fault("results:deadlines:after-pass");
      return result;
    })();
  }

  recover(): DeliveryRecoveryResult {
    const execute = this.#database.transaction((): DeliveryRecoveryResult => {
      const now = this.#clock();
      const latestEpoch = this.#database.prepare(`
        SELECT instance_generation FROM daemon_runtime_epochs
         ORDER BY instance_generation DESC LIMIT 1
      `).get();
      const deadlineCounts = this.#applyDeadlineTransitions(
        now,
        isRow(latestEpoch) ? integer(latestEpoch, "instance_generation") : 1,
      );
      const expiredClaims = this.#database.prepare(`
        SELECT delivery.result_delivery_id FROM result_deliveries delivery
         WHERE delivery.state='claimed' AND delivery.claim_deadline IS NOT NULL
           AND delivery.claim_deadline<=?
           AND NOT EXISTS (
             SELECT 1 FROM lifecycle_custody_adoption_deliveries ownership
            WHERE ownership.delivery_id=delivery.result_delivery_id
              AND ownership.active_owner=1
           )
      `).all(now).filter(isRow);
      let returnedClaims = 0;
      for (const delivery of expiredClaims) {
        this.#database.prepare(`
          UPDATE result_deliveries
             SET state='pending', revision=revision+1, claim_generation=claim_generation+1,
                 claimed_by=NULL, claim_deadline=NULL,
                 overdue_at=NULL, updated_at=?
           WHERE result_delivery_id=? AND state='claimed'
             AND NOT EXISTS (
               SELECT 1 FROM lifecycle_custody_adoption_deliveries ownership
              WHERE ownership.delivery_id=result_deliveries.result_delivery_id
                AND ownership.active_owner=1
             )
        `).run(
          now,
          text(delivery, "result_delivery_id"),
        );
        returnedClaims += 1;
      }
      this.#fault("results:recover:after-deadlines");
      return {
        returnedClaims,
        overdueDeliveries: deadlineCounts.overdueDeliveries,
        overdueRequests: deadlineCounts.overdueRequests,
      };
    });
    return execute();
  }

  #applyDeadlineTransitions(
    now: number,
    producerGeneration: number,
  ): Omit<ResultDeadlinePassResult, "daemonInstanceGeneration" | "passGeneration"> {
    const touchedRequestIds = new Set<string>();
    const dueDeliveries = this.#database.prepare(`
      SELECT delivery.result_delivery_id,delivery.request_id,delivery.state,delivery.claim_generation
        FROM result_deliveries delivery
       WHERE delivery.state IN ('pending','claimed') AND delivery.response_deadline<=?
         AND NOT EXISTS (
           SELECT 1 FROM lifecycle_custody_adoption_deliveries ownership
          WHERE ownership.delivery_id=delivery.result_delivery_id
            AND ownership.active_owner=1
         )
       ORDER BY delivery.result_delivery_id
    `).all(now).filter(isRow);
    let overdueDeliveries = 0;
    for (const delivery of dueDeliveries) {
      const state = text(delivery, "state");
      const changed = state === "claimed"
        ? this.#database.prepare(`
            UPDATE result_deliveries
               SET state='overdue', revision=revision+1,
                   claim_generation=claim_generation+1,
                   claimed_by=NULL, claim_deadline=NULL,
                   overdue_at=?, updated_at=?
             WHERE result_delivery_id=? AND state='claimed'
               AND claim_generation=? AND response_deadline<=?
               AND NOT EXISTS (
                 SELECT 1 FROM lifecycle_custody_adoption_deliveries ownership
                WHERE ownership.delivery_id=result_deliveries.result_delivery_id
                  AND ownership.active_owner=1
               )
          `).run(
            now,
            now,
            text(delivery, "result_delivery_id"),
            integer(delivery, "claim_generation"),
            now,
          )
        : this.#database.prepare(`
            UPDATE result_deliveries
               SET state='overdue', revision=revision+1, overdue_at=?, updated_at=?
             WHERE result_delivery_id=? AND state='pending' AND response_deadline<=?
               AND NOT EXISTS (
                 SELECT 1 FROM lifecycle_custody_adoption_deliveries ownership
                WHERE ownership.delivery_id=result_deliveries.result_delivery_id
                  AND ownership.active_owner=1
               )
          `).run(now, now, text(delivery, "result_delivery_id"), now);
      if (changed.changes === 1) {
        overdueDeliveries += 1;
        touchedRequestIds.add(text(delivery, "request_id"));
      }
    }

    const dueRequests = this.#database.prepare(`
      SELECT request_id FROM task_requests
       WHERE state='pending' AND response_deadline<=?
       ORDER BY request_id
    `).all(now).filter(isRow);
    let overdueRequests = 0;
    for (const request of dueRequests) {
      const requestId = text(request, "request_id");
      const changed = this.#database.prepare(`
        UPDATE task_requests SET state='overdue', updated_at=?
         WHERE request_id=? AND state='pending' AND response_deadline<=?
      `).run(now, requestId, now);
      if (changed.changes === 1) {
        overdueRequests += 1;
        touchedRequestIds.add(requestId);
      }
    }

    const unalertedOverdueRequests = this.#database.prepare(`
      SELECT task_request.request_id
        FROM task_requests task_request
       WHERE task_request.response_deadline<=?
         AND (
           task_request.state='overdue' OR EXISTS (
             SELECT 1 FROM result_deliveries delivery
              WHERE delivery.request_id=task_request.request_id
                AND delivery.state='overdue'
           )
         )
         AND NOT EXISTS (
           SELECT 1 FROM attention_items item
            WHERE item.project_session_id=task_request.project_session_id
              AND item.dedupe_key='result-overdue:' || task_request.callback_id
         )
       ORDER BY task_request.request_id
    `).all(now).filter(isRow);
    for (const request of unalertedOverdueRequests) {
      touchedRequestIds.add(text(request, "request_id"));
    }

    let attentionItems = 0;
    let notificationsEnqueued = 0;
    for (const requestId of [...touchedRequestIds].sort()) {
      const request = row(this.#database.prepare(`
        SELECT task_request.*, session.project_id
          FROM task_requests task_request
          JOIN project_sessions session
            ON session.project_session_id=task_request.project_session_id
         WHERE task_request.request_id=?
      `).get(requestId), "overdue task request");
      const dedupeKey = `result-overdue:${text(request, "callback_id")}`;
      const existingAttention = this.#database.prepare(`
        SELECT item_id FROM attention_items
         WHERE project_session_id=? AND dedupe_key=?
      `).get(text(request, "project_session_id"), dedupeKey);
      const item = this.#notifications.upsertAttention({
        producerId: "daemon-result-deadline",
        projectId: text(request, "project_id"),
        projectSessionId: text(request, "project_session_id"),
        coordinationRunId: text(request, "run_id"),
        principalGeneration: producerGeneration,
      }, {
        dedupeKey,
        kind: "blocked",
        severity: "critical",
        payload: {
          priority: "critical-path",
          title: "Required result overdue",
          summary: `Result callback ${text(request, "callback_id")} is overdue; its dependent barrier remains blocked.`,
          requestId,
          callbackId: text(request, "callback_id"),
          taskId: text(request, "task_id"),
          responseDeadline: new Date(integer(request, "response_deadline")).toISOString(),
          dependentBarrierId: text(request, "dependent_barrier_id"),
        },
      });
      if (!isRow(existingAttention)) attentionItems += 1;
      const existingNotification = this.#database.prepare(`
        SELECT notification_id FROM notification_deliveries
         WHERE item_id=? AND item_revision=? AND target_integration='native-desktop'
      `).get(item.itemId, item.revision);
      this.#notifications.enqueue({
        producerId: "daemon-result-deadline",
        projectId: text(request, "project_id"),
        projectSessionId: text(request, "project_session_id"),
        coordinationRunId: text(request, "run_id"),
        principalGeneration: producerGeneration,
      }, {
        itemId: item.itemId,
        expectedItemRevision: item.revision,
        targetIntegration: "native-desktop",
      });
      if (!isRow(existingNotification)) notificationsEnqueued += 1;
    }
    this.#fault("results:deadlines:after-transitions");
    return { overdueDeliveries, overdueRequests, attentionItems, notificationsEnqueued };
  }

  #parseDeadlinePassResult(value: unknown): ResultDeadlinePassResult {
    if (!isRow(value)) throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "stored deadline pass is invalid");
    for (const field of [
      "daemonInstanceGeneration",
      "passGeneration",
      "overdueDeliveries",
      "overdueRequests",
      "attentionItems",
      "notificationsEnqueued",
    ] as const) {
      if (!Number.isSafeInteger(value[field]) || (value[field] as number) < 0) {
        throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "stored deadline pass is invalid");
      }
    }
    return value as ResultDeadlinePassResult;
  }

  get(resultDeliveryId: string): ResultDelivery {
    const value = row(this.#database.prepare(`
      SELECT d.*, q.request_message_id, q.request_revision AS joined_request_revision,
             q.callback_generation, q.dependent_barrier_id,
             r.reply_message_id, r.reply_revision AS joined_reply_revision
        FROM result_deliveries d
        JOIN task_requests q ON q.request_id=d.request_id
        JOIN task_results r ON r.result_id=d.result_id
       WHERE d.result_delivery_id=?
    `).get(resultDeliveryId), "result delivery");
    const state = text(value, "state");
    const base = {
      resultDeliveryId: text(value, "result_delivery_id"),
      revision: integer(value, "revision"),
      projectSessionId: text(value, "project_session_id"),
      taskId: text(value, "task_id"),
      requestMessageId: text(value, "request_message_id"),
      requestRevision: integer(value, "joined_request_revision"),
      replyMessageId: text(value, "reply_message_id"),
      replyRevision: integer(value, "joined_reply_revision"),
      taskRevision: integer(value, "task_revision"),
      callbackId: text(value, "callback_id"),
      callbackGeneration: integer(value, "callback_generation"),
      assignmentGeneration: integer(value, "assignment_generation"),
      targetAgentId: text(value, "requester_agent_id"),
      targetProviderSessionRef: text(value, "target_provider_session"),
      payloadDigest: text(value, "payload_digest"),
      responseDeadline: new Date(integer(value, "response_deadline")).toISOString(),
      dependentBarrierId: text(value, "dependent_barrier_id"),
      required: integer(value, "required") === 1,
      claimGeneration: integer(value, "claim_generation"),
      state,
    };
    if (state === "claimed") {
      return parseResultDelivery({
        ...base,
        claimedByAgentId: text(value, "claimed_by"),
        claimDeadline: new Date(integer(value, "claim_deadline")).toISOString(),
      });
    }
    if (state === "provider-accepted") {
      return parseResultDelivery({
        ...base,
        claimedByAgentId: text(value, "claimed_by"),
        claimDeadline: new Date(integer(value, "claim_deadline")).toISOString(),
        providerAcceptedAt: new Date(integer(value, "provider_accepted_at")).toISOString(),
      });
    }
    if (state === "consumed") {
      return parseResultDelivery({ ...base, consumedAt: new Date(integer(value, "consumed_at")).toISOString() });
    }
    if (state === "overdue") {
      return parseResultDelivery({ ...base, overdueAt: new Date(integer(value, "overdue_at")).toISOString() });
    }
    if (state === "abandoned") {
      return parseResultDelivery({
        ...base,
        abandonedAt: new Date(integer(value, "abandoned_at")).toISOString(),
        reason: text(value, "abandoned_reason"),
      });
    }
    return parseResultDelivery(base);
  }

  #executeAgentCommand<Result>(
    context: AuthenticatedAgentContext,
    commandId: string,
    payload: unknown,
    mutate: () => Result,
  ): Result {
    const execute = this.#database.transaction((): Result => {
      const payloadHash = sha256(canonicalJson({ context, payload }));
      const existing = this.#database.prepare(`
        SELECT payload_hash, result_json FROM commands
         WHERE run_id=? AND actor_agent_id=? AND command_id=?
      `).get(context.coordinationRunId, context.agentId, commandId);
      if (isRow(existing)) {
        if (text(existing, "payload_hash") !== payloadHash) {
          throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "delivery command ID was reused with changed input");
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

  #transition<Result extends ResultDelivery>(
    context: DeliveryContext,
    commandId: string,
    transition: string,
    payload: unknown,
    mutate: () => Result,
  ): Result {
    const execute = this.#database.transaction((): Result => {
      const actor = "agentId" in context ? context.agentId : `integration:${context.integrationId}`;
      const deliveryId = isRow(payload) && typeof payload.resultDeliveryId === "string"
        ? payload.resultDeliveryId
        : (() => { throw new ProjectFabricCoreError("PROTOCOL_INVALID", "result delivery ID is required"); })();
      const identityHash = sha256(canonicalJson({ context, transition, payload }));
      const existing = this.#database.prepare(`
        SELECT identity_hash, detail_json FROM result_delivery_attempts
         WHERE result_delivery_id=? AND command_id=?
      `).get(deliveryId, commandId);
      if (isRow(existing)) {
        if (text(existing, "identity_hash") !== identityHash) {
          throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "delivery transition command changed on replay");
        }
        return parseResultDelivery(JSON.parse(text(existing, "detail_json"))) as Result;
      }
      const delivery = this.#deliveryRow(deliveryId);
      this.#assertLifecycleTransitionAllowed(deliveryId, transition);
      assertTaskOperationAdmitted(this.#database, text(delivery, "run_id"), text(delivery, "task_id"));
      const result = mutate();
      this.#database.prepare(`
        INSERT INTO result_delivery_attempts(
          result_delivery_id, command_id, claim_generation, transition,
          identity_hash, detail_json, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?)
      `).run(
        deliveryId,
        commandId,
        result.claimGeneration,
        `${transition}:${actor}`,
        identityHash,
        canonicalJson(result),
        this.#clock(),
      );
      return result;
    });
    return execute();
  }

  #assertCompletionOwnership(
    context: AuthenticatedAgentContext,
    request: TaskCompleteWithReply,
    pending: Row,
  ): void {
    if (
      text(pending, "project_session_id") !== context.projectSessionId ||
      text(pending, "run_id") !== context.coordinationRunId ||
      text(pending, "task_id") !== request.taskId ||
      text(pending, "target_agent_id") !== context.agentId ||
      integer(pending, "request_revision") !== request.expectedRequestRevision ||
      text(pending, "callback_id") !== request.callbackId ||
      integer(pending, "callback_generation") !== request.callbackGeneration ||
      !["pending", "overdue"].includes(text(pending, "state"))
    ) {
      throw new ProjectFabricCoreError("STALE_REVISION", "task request ownership or revision changed");
    }
    const task = row(this.#database.prepare(`
      SELECT state, revision, owner_agent_id FROM tasks WHERE run_id=? AND task_id=?
    `).get(context.coordinationRunId, request.taskId), "task");
    if (
      text(task, "state") !== "active" ||
      integer(task, "revision") !== request.expectedTaskRevision ||
      text(task, "owner_agent_id") !== context.agentId
    ) {
      throw new ProjectFabricCoreError("TASK_NOT_OWNER", "task completion owner or revision changed");
    }
    const lease = row(this.#database.prepare(`
      SELECT holder_agent_id, generation, status FROM task_owner_leases WHERE lease_id=?
    `).get(request.ownerLeaseId), "task owner lease");
    if (
      text(lease, "holder_agent_id") !== context.agentId ||
      integer(lease, "generation") !== request.ownerLeaseGeneration ||
      text(lease, "status") !== "active"
    ) {
      throw new ProjectFabricCoreError("STALE_LEASE_GENERATION", "task owner lease changed");
    }
  }

  #bindMemberships(context: AuthenticatedAgentContext, request: TaskRequest): void {
    this.#memberships.bindRequired(context.coordinationRunId, [
      { kind: "task", memberId: request.task.taskId },
      { kind: "required-message", memberId: request.request.messageId },
      { kind: "scoped-barrier", memberId: request.request.dependentBarrierId },
    ]);
  }

  #bindCompletionMemberships(
    context: AuthenticatedAgentContext,
    replyMessageId: string,
    artifactIds: readonly string[],
  ): void {
    this.#memberships.bindRequired(context.coordinationRunId, [
      { kind: "required-message", memberId: replyMessageId },
      ...artifactIds.map((memberId) => ({ kind: "artifact-obligation" as const, memberId })),
    ]);
    this.#memberships.reconcile(
      context.coordinationRunId,
      artifactIds.map((memberId) => ({ kind: "artifact-obligation" as const, memberId })),
    );
  }

  #deliverMessage(runId: string, messageId: string, recipientId: string): string {
    const state = this.#database.prepare(`
      SELECT next_sequence FROM mailbox_state WHERE run_id=? AND recipient_id=?
    `).get(runId, recipientId);
    const sequence = isRow(state) ? integer(state, "next_sequence") : 1;
    if (isRow(state)) {
      this.#database.prepare(`
        UPDATE mailbox_state SET next_sequence=next_sequence+1 WHERE run_id=? AND recipient_id=?
      `).run(runId, recipientId);
    } else {
      this.#database.prepare(`
        INSERT INTO mailbox_state(run_id, recipient_id, next_sequence, contiguous_watermark)
        VALUES (?, ?, 2, 0)
      `).run(runId, recipientId);
    }
    const deliveryId = `delivery_${sha256(`${messageId}\0${recipientId}`).slice(0, 24)}`;
    this.#database.prepare(`
      INSERT INTO deliveries(
        delivery_id, message_id, run_id, recipient_id, mailbox_sequence,
        state, attempt_count
      ) VALUES (?, ?, ?, ?, ?, 'ready', 0)
    `).run(deliveryId, messageId, runId, recipientId, sequence);
    return deliveryId;
  }

  #releaseBarrier(requestId: string, state: "released" | "abandoned"): void {
    const barrier = row(this.#database.prepare(`
      SELECT b.barrier_id, q.project_session_id, q.run_id
        FROM task_request_barriers b JOIN task_requests q USING(request_id)
       WHERE b.request_id=?
    `).get(requestId), "task request barrier");
    this.#database.prepare(`
      UPDATE task_request_barriers SET state=? WHERE request_id=? AND state='blocked'
    `).run(state, requestId);
    const membership = this.#database.prepare(`
      UPDATE project_session_memberships
         SET state=?, revision=revision+1, updated_at=?
       WHERE member_kind='scoped-barrier' AND member_id=? AND state='active'
         AND project_session_id=? AND coordination_run_id=?
    `).run(
      state === "released" ? "reconciled" : "abandoned",
      this.#clock(),
      text(barrier, "barrier_id"),
      text(barrier, "project_session_id"),
      text(barrier, "run_id"),
    );
    touchProjectSessionMembershipRevision(
      this.#database,
      text(barrier, "project_session_id"),
      this.#clock(),
      membership.changes,
    );
  }

  #assertAgentContext(context: AuthenticatedAgentContext, sessionId: string, runId: string): void {
    if (context.projectSessionId !== sessionId || context.coordinationRunId !== runId) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "agent context is bound to another request run");
    }
    this.#agent(runId, context.agentId);
  }

  #assertDeliveryContext(context: DeliveryContext, delivery: Row): void {
    if (
      text(delivery, "project_session_id") !== context.projectSessionId ||
      text(delivery, "run_id") !== context.coordinationRunId
    ) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "result delivery is outside the authenticated context");
    }
    if ("agentId" in context) {
      this.#agent(context.coordinationRunId, context.agentId);
      if (text(delivery, "requester_agent_id") !== context.agentId) {
        throw new ProjectFabricCoreError("TASK_NOT_OWNER", "agent does not own the callback delivery");
      }
    } else {
      const project = row(this.#database.prepare(`
        SELECT s.project_id FROM project_sessions s WHERE s.project_session_id=?
      `).get(context.projectSessionId), "project session");
      if (text(project, "project_id") !== context.projectId) {
        throw new ProjectFabricCoreError("WRONG_PROJECT", "integration is bound to another project");
      }
    }
  }

  #assertRevision(value: Row, expectedRevision: number): void {
    if (integer(value, "revision") !== expectedRevision) {
      throw new ProjectFabricCoreError("STALE_REVISION", "result delivery revision changed");
    }
  }

  #agent(runId: string, agentId: string): Row {
    return row(this.#database.prepare(`
      SELECT * FROM agents WHERE run_id=? AND agent_id=?
    `).get(runId, agentId), "agent");
  }

  #deliveryRow(resultDeliveryId: string): Row {
    return row(this.#database.prepare(`
      SELECT * FROM result_deliveries WHERE result_delivery_id=?
    `).get(resultDeliveryId), "result delivery");
  }

  #assertLifecycleTransitionAllowed(resultDeliveryId: string, transition: string): void {
    const owned = this.#database.prepare(`
      SELECT CASE WHEN EXISTS (
               SELECT 1 FROM lifecycle_receipt_custody_effects effect
                WHERE effect.run_id=ownership.run_id AND effect.agent_id=ownership.agent_id
                  AND effect.custody_id=ownership.custody_id
             ) THEN 1 ELSE 0 END AS prepared
        FROM lifecycle_custody_adoption_deliveries ownership
        JOIN lifecycle_rotation_custody_heads head
          ON head.run_id=ownership.run_id AND head.agent_id=ownership.agent_id
         AND head.custody_id=ownership.custody_id
       WHERE ownership.delivery_id=? AND ownership.active_owner=1 AND head.terminal=0
       LIMIT 1
    `).get(resultDeliveryId);
    if (isRow(owned) && (transition !== "provider-accept" || integer(owned, "prepared") === 1)) {
      throw new ProjectFabricCoreError(
        "RECOVERY_REQUIRED",
        "result delivery is owned by an active lifecycle rotation",
      );
    }
  }
}
