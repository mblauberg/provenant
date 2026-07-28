import type Database from "better-sqlite3";
import { v7 as uuidv7 } from "uuid";

import type { CommandJournal } from "../application/command-journal.js";
import type { MessageInput } from "../domain/types.js";
import { MESSAGE_POLICY } from "../domain/types.js";
import { FabricError } from "../errors.js";
import {
  assertRunAcceptingWork,
  assertTaskOperationAdmitted,
} from "../operator/task-run-admission.js";
import type { ProjectSessionMembershipStore } from "./membership-store.js";
import { canonicalJson, isRow, sha256, type Row } from "../persistence/row-codec.js";

type MailboxCommandJournalPort = Pick<CommandJournal, "execute">;

type MailboxMembershipPort = Pick<
  ProjectSessionMembershipStore,
  "bindRequired" | "reconcileRequiredMessageIfSettled"
>;

export type MailboxCustodyHost = Readonly<{
  assertChair(runId: string, actorAgentId: string): void;
  event(runId: string, type: string, actorAgentId: string | null, payload: unknown): void;
}>;

export type MailboxCustodyServiceOptions = Readonly<{
  database: Database.Database;
  clock: () => number;
  commandJournal: MailboxCommandJournalPort;
  memberships: MailboxMembershipPort;
  host: MailboxCustodyHost;
}>;

export type MailboxDelivery = Readonly<{
  deliveryId: string;
  messageId: string;
  sequence: number;
  body: string;
  attempt: number;
  senderId: string;
  kind: MessageInput["kind"];
  requiresAck: boolean;
}>;

function stringField(row: Row, field: string): string {
  const value = row[field];
  if (typeof value !== "string") {
    throw new Error(`database field ${field} is not a string`);
  }
  return value;
}

function numberField(row: Row, field: string): number {
  const value = row[field];
  if (typeof value !== "number") {
    throw new Error(`database field ${field} is not a number`);
  }
  return value;
}

function messageKindField(row: Row, field: string): MessageInput["kind"] {
  const value = stringField(row, field);
  if (
    value === "request" ||
    value === "response" ||
    value === "event" ||
    value === "steer" ||
    value === "cancel" ||
    value === "escalate" ||
    value === "ack"
  ) return value;
  throw new Error(`database field ${field} is not a message kind`);
}

function rowOrNotFound(value: unknown, label: string): Row {
  if (!isRow(value)) {
    throw new FabricError("NOT_FOUND", `${label} was not found`);
  }
  return value;
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function audienceHash(input: MessageInput): string {
  const audience =
    input.audience.kind === "agents"
      ? { kind: input.audience.kind, agentIds: [...new Set(input.audience.agentIds)].sort() }
      : input.audience.kind === "team"
        ? { kind: input.audience.kind, teamId: input.audience.teamId }
        : { kind: input.audience.kind, taskId: input.audience.taskId };
  return sha256(
    canonicalJson({
      audience,
      context: input.context ?? { kind: "direct" },
      kind: input.kind,
      body: input.body,
      requiresAck: input.requiresAck,
      conversationId: input.conversationId ?? null,
      replyToMessageId: input.replyToMessageId ?? null,
      taskRevision: input.taskRevision ?? null,
      hopCount: input.hopCount ?? 0,
      expiresAt: input.expiresAt ?? null,
    }),
  );
}

/**
 * Byte-moved from `Fabric`'s mailbox custody block (issue #371): message admission,
 * audience and relationship checks, discussion-group creation, ordered delivery claims,
 * acknowledgement/abandonment, required-message membership settlement, and the contiguous
 * mailbox watermark. Preserves the original transaction boundaries, mutation ordering,
 * error codes/messages, event timing, dedupe identity, and visibility-timeout semantics.
 * The narrow host handle retains only the two Fabric-owned effects used by this family.
 */
export class MailboxCustodyService {
  readonly #database: Database.Database;
  readonly #clock: () => number;
  readonly #commandJournal: MailboxCommandJournalPort;
  readonly #memberships: MailboxMembershipPort;
  readonly #host: MailboxCustodyHost;

  constructor(options: MailboxCustodyServiceOptions) {
    this.#database = options.database;
    this.#clock = options.clock;
    this.#commandJournal = options.commandJournal;
    this.#memberships = options.memberships;
    this.#host = options.host;
  }

  sendMessage(runId: string, senderId: string, input: MessageInput): { messageId: string } {
    if (Buffer.byteLength(input.body, "utf8") > MESSAGE_POLICY.maximumInlineBytes) {
      throw new FabricError("CAPABILITY_FORBIDDEN", "inline message exceeds 4096 bytes");
    }
    const hopCount = input.hopCount ?? 0;
    if (!Number.isInteger(hopCount) || hopCount < 0 || hopCount > MESSAGE_POLICY.maximumHops) {
      throw new FabricError("MESSAGE_HOP_LIMIT_EXCEEDED", `message exceeds the ${MESSAGE_POLICY.maximumHops}-hop limit`);
    }
    const expiresAt = input.expiresAt === undefined ? null : Date.parse(input.expiresAt);
    if (expiresAt !== null && (!Number.isFinite(expiresAt) || expiresAt <= this.#clock())) {
      throw new FabricError("CAPABILITY_FORBIDDEN", "message expiry must be a future ISO timestamp");
    }
    const payloadHash = audienceHash(input);
    const existing = this.#database
      .prepare("SELECT message_id, payload_hash FROM messages WHERE run_id = ? AND sender_id = ? AND dedupe_key = ?")
      .get(runId, senderId, input.dedupeKey);
    if (isRow(existing)) {
      if (stringField(existing, "payload_hash") !== payloadHash) {
        throw new FabricError("DEDUPE_CONFLICT", "dedupe key was reused with a changed payload or audience");
      }
      return { messageId: stringField(existing, "message_id") };
    }
    assertRunAcceptingWork(this.#database, runId);
    if (input.audience.kind === "task") assertTaskOperationAdmitted(this.#database, runId, input.audience.taskId);
    const messageId = uuidv7();
    const conversationId = input.conversationId ?? messageId;
    this.#database.transaction(() => {
      const recipients = this.#resolveAudienceRecipients(runId, senderId, input.audience);
      if (recipients.length === 0) {
        throw new FabricError("NOT_FOUND", "message has no recipients");
      }
      for (const recipientId of recipients) {
        rowOrNotFound(
          this.#database.prepare("SELECT 1 FROM agents WHERE run_id = ? AND agent_id = ?").get(runId, recipientId),
          `recipient ${recipientId}`,
        );
        if (input.audience.kind === "agents") {
          this.#assertMessageRelationship(runId, senderId, recipientId, input.context);
        }
        if (input.requiresAck) {
          const unresolved = numberField(rowOrNotFound(this.#database.prepare("SELECT COUNT(*) AS count FROM deliveries d JOIN messages m ON m.message_id = d.message_id WHERE d.run_id = ? AND d.recipient_id = ? AND m.requires_ack = 1 AND d.state NOT IN ('acknowledged', 'abandoned', 'expired')").get(runId, recipientId), "unacknowledged delivery count"), "count");
          if (unresolved >= MESSAGE_POLICY.maximumUnacknowledgedPerAgent) throw new FabricError("MESSAGE_QUOTA_EXCEEDED", `recipient has ${MESSAGE_POLICY.maximumUnacknowledgedPerAgent} unresolved acknowledged-required messages`);
        }
      }
      if (input.replyToMessageId !== undefined) {
        const reply = rowOrNotFound(this.#database.prepare("SELECT conversation_id FROM messages WHERE run_id = ? AND message_id = ?").get(runId, input.replyToMessageId), "reply message");
        if (stringField(reply, "conversation_id") !== conversationId) throw new FabricError("MESSAGE_RELATIONSHIP_FORBIDDEN", "reply message belongs to another conversation");
      }
      this.#database
        .prepare(
          "INSERT INTO messages(message_id, run_id, sender_id, dedupe_key, payload_hash, audience_json, kind, body, requires_ack, conversation_id, reply_to_message_id, task_revision, hop_count, expires_at, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .run(
          messageId,
          runId,
          senderId,
          input.dedupeKey,
          payloadHash,
          canonicalJson(input.audience),
          input.kind,
          input.body,
          input.requiresAck ? 1 : 0,
          conversationId,
          input.replyToMessageId ?? null,
          input.taskRevision ?? null,
          hopCount,
          expiresAt,
          this.#clock(),
        );
      this.#database
        .prepare("INSERT INTO message_contexts(message_id, context_json) VALUES (?, ?)")
        .run(messageId, canonicalJson(input.context ?? { kind: "direct" }));
      for (const recipientId of recipients) {
        const state = rowOrNotFound(
          this.#database
            .prepare("SELECT next_sequence FROM mailbox_state WHERE run_id = ? AND recipient_id = ?")
            .get(runId, recipientId),
          "mailbox",
        );
        const sequence = numberField(state, "next_sequence");
        this.#database
          .prepare(
            "INSERT INTO deliveries(delivery_id, message_id, run_id, recipient_id, mailbox_sequence, state) VALUES (?, ?, ?, ?, ?, 'ready')",
          )
          .run(uuidv7(), messageId, runId, recipientId, sequence);
        this.#database
          .prepare("UPDATE mailbox_state SET next_sequence = next_sequence + 1 WHERE run_id = ? AND recipient_id = ?")
          .run(runId, recipientId);
      }
      if (input.requiresAck) {
        this.#memberships.bindRequired(runId, [{ kind: "required-message", memberId: messageId }]);
      }
      this.#host.event(runId, "message-persisted", senderId, { messageId, recipients });
    })();
    return { messageId };
  }

  #resolveAudienceRecipients(
    runId: string,
    senderId: string,
    audience: MessageInput["audience"],
  ): string[] {
    if (audience.kind === "agents") {
      return [...new Set(audience.agentIds)].sort();
    }
    if (audience.kind === "team") {
      rowOrNotFound(
        this.#database.prepare("SELECT 1 FROM teams WHERE run_id = ? AND team_id = ?").get(runId, audience.teamId),
        `team audience ${audience.teamId}`,
      );
      const senderMembership = this.#database
        .prepare("SELECT 1 FROM team_members WHERE run_id = ? AND team_id = ? AND agent_id = ?")
        .get(runId, audience.teamId, senderId);
      if (!isRow(senderMembership)) {
        throw new FabricError("MESSAGE_RELATIONSHIP_FORBIDDEN", "sender is not a member of the named team");
      }
      return this.#database
        .prepare("SELECT agent_id FROM team_members WHERE run_id = ? AND team_id = ? ORDER BY agent_id")
        .all(runId, audience.teamId)
        .map((value) => stringField(rowOrNotFound(value, "team audience member"), "agent_id"));
    }
    rowOrNotFound(
      this.#database.prepare("SELECT 1 FROM tasks WHERE run_id = ? AND task_id = ?").get(runId, audience.taskId),
      `task audience ${audience.taskId}`,
    );
    if (!this.#taskIncludesAgent(runId, audience.taskId, senderId)) {
      throw new FabricError("MESSAGE_RELATIONSHIP_FORBIDDEN", "sender is not a participant in the named task");
    }
    return this.#database
      .prepare(
        "SELECT agent_id FROM (SELECT owner_agent_id AS agent_id FROM tasks WHERE run_id = ? AND task_id = ? AND owner_agent_id IS NOT NULL UNION SELECT agent_id FROM task_participants WHERE run_id = ? AND task_id = ?) ORDER BY agent_id",
      )
      .all(runId, audience.taskId, runId, audience.taskId)
      .map((value) => stringField(rowOrNotFound(value, "task audience member"), "agent_id"))
      // Deliberate asymmetry (issue #336): task audiences exclude the sender so no
      // self-echo sits pending ack; team audiences intentionally keep the sender.
      .filter((agentId) => agentId !== senderId);
  }

  createDiscussionGroup(
    runId: string,
    actorAgentId: string,
    input: { groupId: string; memberAgentIds: string[]; teamId?: string; commandId: string },
  ): { groupId: string; memberAgentIds: string[] } {
    const parse = (value: unknown): value is { groupId: string; memberAgentIds: string[] } =>
      isRow(value) && typeof value.groupId === "string" && isStringArray(value.memberAgentIds);
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, parse, () => {
      this.#host.assertChair(runId, actorAgentId);
      const members = [...new Set(input.memberAgentIds)].sort();
      if (members.length < 2) throw new FabricError("MESSAGE_RELATIONSHIP_FORBIDDEN", "discussion group requires two members");
      for (const agentId of members) {
        rowOrNotFound(
          this.#database.prepare("SELECT 1 FROM agents WHERE run_id = ? AND agent_id = ?").get(runId, agentId),
          `discussion member ${agentId}`,
        );
      }
      this.#database
        .prepare("INSERT INTO discussion_groups(run_id, group_id, team_id, created_by) VALUES (?, ?, ?, ?)")
        .run(runId, input.groupId, input.teamId ?? null, actorAgentId);
      for (const agentId of members) {
        this.#database
          .prepare("INSERT INTO discussion_group_members(run_id, group_id, agent_id) VALUES (?, ?, ?)")
          .run(runId, input.groupId, agentId);
      }
      return { groupId: input.groupId, memberAgentIds: members };
    });
  }

  #assertMessageRelationship(
    runId: string,
    senderId: string,
    recipientId: string,
    context: MessageInput["context"],
  ): void {
    if (senderId === recipientId) return;
    if (context?.kind === "task") {
      if (this.#taskIncludesAgent(runId, context.taskId, senderId) && this.#taskIncludesAgent(runId, context.taskId, recipientId)) return;
      throw new FabricError("MESSAGE_RELATIONSHIP_FORBIDDEN", "sender and recipient do not share the named task");
    }
    if (context?.kind === "discussion-group") {
      const count = this.#database
        .prepare(
          "SELECT COUNT(*) AS count FROM discussion_group_members WHERE run_id = ? AND group_id = ? AND agent_id IN (?, ?)",
        )
        .get(runId, context.groupId, senderId, recipientId);
      if (numberField(rowOrNotFound(count, "discussion membership"), "count") === 2) return;
      throw new FabricError("MESSAGE_RELATIONSHIP_FORBIDDEN", "sender and recipient do not share the named discussion group");
    }
    if (context?.kind === "task-dependency") {
      const edge = this.#database
        .prepare(
          "SELECT 1 FROM task_dependencies WHERE run_id = ? AND ((task_id = ? AND dependency_task_id = ?) OR (task_id = ? AND dependency_task_id = ?))",
        )
        .get(runId, context.fromTaskId, context.toTaskId, context.toTaskId, context.fromTaskId);
      if (
        isRow(edge) &&
        this.#taskIncludesAgent(runId, context.fromTaskId, senderId) &&
        this.#taskIncludesAgent(runId, context.toTaskId, recipientId)
      ) return;
      throw new FabricError("MESSAGE_RELATIONSHIP_FORBIDDEN", "sender and recipient do not own the named dependency endpoints");
    }
    if (this.#agentsHaveAnyRelationship(runId, senderId, recipientId)) return;
    throw new FabricError("MESSAGE_RELATIONSHIP_FORBIDDEN", "sender and recipient have no authorised task, dependency or group relationship");
  }

  #taskIncludesAgent(runId: string, taskId: string, agentId: string): boolean {
    return isRow(
      this.#database
        .prepare(
          "SELECT 1 FROM tasks t WHERE t.run_id = ? AND t.task_id = ? AND (t.owner_agent_id = ? OR EXISTS (SELECT 1 FROM task_participants p WHERE p.run_id = t.run_id AND p.task_id = t.task_id AND p.agent_id = ?))",
        )
        .get(runId, taskId, agentId, agentId),
    );
  }

  #agentsHaveAnyRelationship(runId: string, left: string, right: string): boolean {
    const sharedTask = this.#database
      .prepare(
        "SELECT 1 FROM tasks t WHERE t.run_id = ? AND (t.owner_agent_id = ? OR EXISTS (SELECT 1 FROM task_participants p WHERE p.run_id = t.run_id AND p.task_id = t.task_id AND p.agent_id = ?)) AND (t.owner_agent_id = ? OR EXISTS (SELECT 1 FROM task_participants p WHERE p.run_id = t.run_id AND p.task_id = t.task_id AND p.agent_id = ?)) LIMIT 1",
      )
      .get(runId, left, left, right, right);
    if (isRow(sharedTask)) return true;
    const sharedGroup = this.#database
      .prepare(
        "SELECT 1 FROM discussion_group_members l JOIN discussion_group_members r ON r.run_id = l.run_id AND r.group_id = l.group_id WHERE l.run_id = ? AND l.agent_id = ? AND r.agent_id = ? LIMIT 1",
      )
      .get(runId, left, right);
    if (isRow(sharedGroup)) return true;
    const dependency = this.#database
      .prepare(
        "SELECT 1 FROM task_dependencies d JOIN tasks a ON a.run_id = d.run_id AND a.task_id = d.task_id JOIN tasks b ON b.run_id = d.run_id AND b.task_id = d.dependency_task_id WHERE d.run_id = ? AND ((a.owner_agent_id = ? OR EXISTS (SELECT 1 FROM task_participants p WHERE p.run_id = a.run_id AND p.task_id = a.task_id AND p.agent_id = ?)) AND (b.owner_agent_id = ? OR EXISTS (SELECT 1 FROM task_participants p WHERE p.run_id = b.run_id AND p.task_id = b.task_id AND p.agent_id = ?)) OR (a.owner_agent_id = ? OR EXISTS (SELECT 1 FROM task_participants p WHERE p.run_id = a.run_id AND p.task_id = a.task_id AND p.agent_id = ?)) AND (b.owner_agent_id = ? OR EXISTS (SELECT 1 FROM task_participants p WHERE p.run_id = b.run_id AND p.task_id = b.task_id AND p.agent_id = ?))) LIMIT 1",
      )
      .get(runId, left, left, right, right, right, right, left, left);
    return isRow(dependency);
  }

  receiveMessages(
    runId: string,
    recipientId: string,
    input: { limit: number; visibilityTimeoutMs: number },
  ): MailboxDelivery[] {
    const now = this.#clock();
    return this.#database.transaction(() => {
      const freeze = this.#database.prepare(`
        SELECT reason FROM delivery_freezes WHERE run_id=? AND agent_id=?
      `).get(runId, recipientId);
      if (isRow(freeze)) {
        throw new FabricError("CONTEXT_UNRECONCILED", "message delivery is frozen until lifecycle reconciliation");
      }
      const expiringRequiredMessageIds = this.#database.prepare(`
        SELECT DISTINCT delivery.message_id
          FROM deliveries delivery JOIN messages message USING(message_id)
         WHERE delivery.run_id=? AND delivery.recipient_id=?
           AND delivery.state IN ('ready','claimed')
           AND message.requires_ack=1 AND message.expires_at IS NOT NULL
           AND message.expires_at<=?
      `).all(runId, recipientId, now).map((value) =>
        stringField(rowOrNotFound(value, "expiring required message"), "message_id")
      );
      const expired = this.#database.prepare("UPDATE deliveries SET state = 'expired', claim_deadline = NULL, resolution_reason = 'message-expired-by-policy', resolved_at = ? WHERE run_id = ? AND recipient_id = ? AND state IN ('ready', 'claimed') AND message_id IN (SELECT message_id FROM messages WHERE run_id = ? AND expires_at IS NOT NULL AND expires_at <= ?)").run(now, runId, recipientId, runId, now);
      if (expired.changes > 0) {
        this.#advanceMailboxWatermark(runId, recipientId);
        for (const messageId of expiringRequiredMessageIds) {
          this.#memberships.reconcileRequiredMessageIfSettled(runId, messageId);
        }
      }
      this.#database
        .prepare(
          "UPDATE deliveries SET state = 'ready', claim_deadline = NULL WHERE run_id = ? AND recipient_id = ? AND state = 'claimed' AND claim_deadline <= ?",
        )
        .run(runId, recipientId, now);
      const rows = this.#database
        .prepare(
          "SELECT d.delivery_id, d.message_id, d.mailbox_sequence, d.attempt_count, m.body, m.sender_id, m.kind, m.requires_ack FROM deliveries d JOIN messages m ON m.message_id = d.message_id WHERE d.run_id = ? AND d.recipient_id = ? AND d.state = 'ready' ORDER BY d.mailbox_sequence LIMIT ?",
        )
        .all(runId, recipientId, Math.max(0, input.limit));
      return rows.map((value) => {
        const row = rowOrNotFound(value, "delivery");
        const attempt = numberField(row, "attempt_count") + 1;
        this.#database
          .prepare(
            "UPDATE deliveries SET state = 'claimed', attempt_count = ?, claim_deadline = ? WHERE delivery_id = ?",
          )
          .run(attempt, now + input.visibilityTimeoutMs, stringField(row, "delivery_id"));
        return {
          deliveryId: stringField(row, "delivery_id"),
          messageId: stringField(row, "message_id"),
          sequence: numberField(row, "mailbox_sequence"),
          body: stringField(row, "body"),
          attempt,
          senderId: stringField(row, "sender_id"),
          kind: messageKindField(row, "kind"),
          requiresAck: numberField(row, "requires_ack") === 1,
        };
      });
    })();
  }

  acknowledgeDelivery(runId: string, recipientId: string, deliveryId: string): void {
    this.#database.transaction(() => {
      const delivery = rowOrNotFound(
        this.#database
          .prepare("SELECT mailbox_sequence, state, message_id FROM deliveries WHERE delivery_id = ? AND run_id = ? AND recipient_id = ?")
          .get(deliveryId, runId, recipientId),
        "delivery",
      );
      if (stringField(delivery, "state") !== "acknowledged") {
        this.#database
          .prepare("UPDATE deliveries SET state = 'acknowledged', acknowledged_at = ?, claim_deadline = NULL WHERE delivery_id = ?")
          .run(this.#clock(), deliveryId);
      }
      this.#advanceMailboxWatermark(runId, recipientId);
      this.#memberships.reconcileRequiredMessageIfSettled(
        runId,
        stringField(delivery, "message_id"),
      );
      this.#host.event(runId, "delivery-acknowledged", recipientId, {
        deliveryId,
        sequence: numberField(delivery, "mailbox_sequence"),
      });
    })();
  }

  abandonDelivery(
    runId: string,
    actorAgentId: string,
    input: { deliveryId: string; reason: string; commandId: string },
  ): { deliveryId: string; status: "abandoned"; reason: string } {
    if (input.reason.trim().length === 0) {
      throw new FabricError("DELIVERY_REASON_REQUIRED", "abandoning a delivery requires a reason");
    }
    return this.#commandJournal.execute(runId, actorAgentId, input.commandId, input, (value): value is { deliveryId: string; status: "abandoned"; reason: string } =>
      isRow(value) && value.deliveryId === input.deliveryId && value.status === "abandoned" && typeof value.reason === "string", () => {
      this.#host.assertChair(runId, actorAgentId);
      const delivery = rowOrNotFound(
        this.#database
          .prepare("SELECT recipient_id, state, message_id FROM deliveries WHERE run_id = ? AND delivery_id = ?")
          .get(runId, input.deliveryId),
        "delivery",
      );
      const state = stringField(delivery, "state");
      if (state === "acknowledged" || state === "expired") {
        throw new FabricError("DELIVERY_ALREADY_RESOLVED", `delivery is already ${state}`);
      }
      this.#database
        .prepare(
          "UPDATE deliveries SET state = 'abandoned', claim_deadline = NULL, resolution_reason = ?, resolved_at = ? WHERE run_id = ? AND delivery_id = ?",
        )
        .run(input.reason.trim(), this.#clock(), runId, input.deliveryId);
      const recipientId = stringField(delivery, "recipient_id");
      this.#advanceMailboxWatermark(runId, recipientId);
      this.#memberships.reconcileRequiredMessageIfSettled(
        runId,
        stringField(delivery, "message_id"),
      );
      const result = { deliveryId: input.deliveryId, status: "abandoned" as const, reason: input.reason.trim() };
      this.#host.event(runId, "delivery-abandoned", actorAgentId, { ...result, recipientId });
      return result;
    });
  }

  #advanceMailboxWatermark(runId: string, recipientId: string): void {
    const state = rowOrNotFound(
      this.#database
        .prepare("SELECT contiguous_watermark FROM mailbox_state WHERE run_id = ? AND recipient_id = ?")
        .get(runId, recipientId),
      "mailbox",
    );
    let watermark = numberField(state, "contiguous_watermark");
    while (true) {
      const next = this.#database
        .prepare("SELECT state FROM deliveries WHERE run_id = ? AND recipient_id = ? AND mailbox_sequence = ?")
        .get(runId, recipientId, watermark + 1);
      if (!isRow(next) || !["acknowledged", "abandoned", "expired"].includes(stringField(next, "state"))) break;
      watermark += 1;
    }
    this.#database
      .prepare("UPDATE mailbox_state SET contiguous_watermark = ? WHERE run_id = ? AND recipient_id = ?")
      .run(watermark, runId, recipientId);
  }

  getMailboxState(runId: string, recipientId: string): {
    contiguousWatermark: number;
    acknowledgedAboveWatermark: number[];
  } {
    const state = rowOrNotFound(
      this.#database
        .prepare("SELECT contiguous_watermark FROM mailbox_state WHERE run_id = ? AND recipient_id = ?")
        .get(runId, recipientId),
      "mailbox",
    );
    const watermark = numberField(state, "contiguous_watermark");
    const above = this.#database
      .prepare(
        "SELECT mailbox_sequence FROM deliveries WHERE run_id = ? AND recipient_id = ? AND state = 'acknowledged' AND mailbox_sequence > ? ORDER BY mailbox_sequence",
      )
      .all(runId, recipientId, watermark)
      .map((value) => numberField(rowOrNotFound(value, "delivery"), "mailbox_sequence"));
    return { contiguousWatermark: watermark, acknowledgedAboveWatermark: above };
  }
}
