import { mkdirSync, mkdtempSync, realpathSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import type Database from "better-sqlite3";
import type {
  ResultDeliveryClaimRequest,
  ResultDeliveryConsumeRequest,
  ResultDeliveryProviderAcceptRequest,
  TaskCompleteWithReply,
  TaskRequest,
} from "@local/agent-fabric-protocol";
import { afterEach, describe, expect, it } from "vitest";

import {
  AtomicDeliveryStore,
  resultDeliveryProviderActionBinding,
  type ResultDeliveryIntegrationContext,
} from "../../../src/results/store.ts";
import { sha256 } from "../../../src/persistence/row-codec.ts";
import {
  chairContext,
  openSystemDatabase,
  workerContext,
  workerTwoContext,
} from "./restart-recovery-fixtures.ts";
import { admitProviderActionFixture } from "../../support/provider-action-fixture.ts";

const databases: Database.Database[] = [];
const roots: string[] = [];
const ARTIFACT_BYTES = "Bounded answer artifact.\n";
const ARTIFACT_DIGEST = `sha256:${sha256(ARTIFACT_BYTES)}`;
const UNRELATED_ARTIFACT_BYTES = "Unrelated artifact.\n";
const UNRELATED_ARTIFACT_DIGEST = `sha256:${sha256(UNRELATED_ARTIFACT_BYTES)}`;
afterEach(() => {
  for (const database of databases.splice(0)) database.close();
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

function open(): Database.Database {
  const database = openSystemDatabase();
  const root = realpathSync(mkdtempSync(join(tmpdir(), "result-artifact-root-")));
  roots.push(root);
  mkdirSync(join(root, ".agent-run", "run_01", "artifacts"), { recursive: true });
  writeFileSync(join(root, ".agent-run", "run_01", "artifacts", "answer.md"), ARTIFACT_BYTES);
  writeFileSync(join(root, ".agent-run", "run_01", "artifacts", "unrelated.md"), UNRELATED_ARTIFACT_BYTES);
  database.prepare("UPDATE projects SET canonical_root=? WHERE project_id='project_01'").run(root);
  database.prepare("UPDATE runs SET workspace_root=? WHERE run_id='run_01'").run(root);
  databases.push(database);
  return database;
}

function request(overrides: Readonly<Record<string, unknown>> = {}): TaskRequest {
  return {
    commandId: "request_command",
    projectSessionId: "session_01",
    coordinationRunId: "run_01",
    task: {
      taskId: "task_answer",
      taskRevision: 1,
      objective: "Return a bounded answer.",
      baseRevision: "base-answer",
      expectedArtifactPaths: ["artifacts/answer.md"],
    },
    request: {
      requestRevision: 1,
      messageId: "message_request",
      conversationId: "conversation_answer",
      targetAgentId: "worker_01",
      targetProviderSessionRef: "provider-worker",
      requiresAck: true,
      dedupeKey: "answer-request",
      responseDeadline: "2099-01-01T00:00:00.000Z",
      callbackId: "callback_answer",
      callbackGeneration: 1,
      dependentBarrierId: "barrier_answer",
    },
    ...overrides,
  } as unknown as TaskRequest;
}

function completion(): TaskCompleteWithReply {
  return {
    commandId: "complete_command",
    taskId: "task_answer",
    expectedTaskRevision: 1,
    ownerLeaseId: "task-owner:run_01:task_answer:1",
    ownerLeaseGeneration: 1,
    requestMessageId: "message_request",
    expectedRequestRevision: 1,
    callbackId: "callback_answer",
    callbackGeneration: 1,
    reply: {
      messageId: "message_reply",
      conversationId: "conversation_answer",
      replyToMessageId: "message_request",
      body: "Bounded answer.",
      artifactRefs: [{ path: "artifacts/answer.md", digest: ARTIFACT_DIGEST }],
    },
    terminalResult: {
      status: "complete",
      summary: "Answer complete.",
      completedAt: "2026-07-11T00:00:01.000Z",
    },
  } as unknown as TaskCompleteWithReply;
}

function requestFor(taskId: string, messageId: string, callbackId: string, conversationId: string): TaskRequest {
  const value = request();
  return {
    ...value,
    commandId: `request_${taskId}`,
    task: { ...value.task, taskId },
    request: { ...value.request, messageId, callbackId, conversationId, dedupeKey: `request_${taskId}` },
  } as TaskRequest;
}

function completionFor(overrides: Readonly<{
  taskId?: string;
  ownerLeaseId?: string;
  requestMessageId?: string;
  callbackId?: string;
  reply?: Readonly<Record<string, unknown>>;
}> = {}): TaskCompleteWithReply {
  const value = completion();
  return {
    ...value,
    ...overrides,
    reply: { ...value.reply, ...overrides.reply },
  } as TaskCompleteWithReply;
}

function requestWithDeadline(responseDeadline: string): TaskRequest {
  const value = request();
  return {
    ...value,
    request: { ...value.request, responseDeadline },
  } as TaskRequest;
}

const integrationContext: ResultDeliveryIntegrationContext = {
  integrationId: "integration_provider",
  projectId: "project_01",
  projectSessionId: "session_01",
  coordinationRunId: "run_01",
  principalGeneration: 1,
};

describe("atomic request, result, and callback delivery", () => {
  it("does not complete task B with task A's active owner lease", () => {
    const database = open();
    const store = new AtomicDeliveryStore({ database, clock: () => 1_000 });
    store.request(chairContext, request());
    store.request(chairContext, requestFor("task_other", "message_other", "callback_other", "conversation_other"));

    expect(() => store.completeWithReply(workerContext, completionFor({
      taskId: "task_other",
      requestMessageId: "message_other",
      callbackId: "callback_other",
      ownerLeaseId: "task-owner:run_01:task_answer:1",
      reply: {
        messageId: "message_other_reply",
        conversationId: "conversation_other",
        replyToMessageId: "message_other",
      },
    }))).toThrowError("task owner lease changed");
    expect(database.prepare("SELECT task_id, state FROM tasks WHERE task_id IN ('task_answer', 'task_other') ORDER BY task_id").all()).toEqual([
      { task_id: "task_answer", state: "active" },
      { task_id: "task_other", state: "active" },
    ]);
    expect(database.prepare("SELECT task_id, status FROM task_owner_leases WHERE task_id IN ('task_answer', 'task_other') ORDER BY task_id").all()).toEqual([
      { task_id: "task_answer", status: "active" },
      { task_id: "task_other", status: "active" },
    ]);
    expect(database.prepare("SELECT COUNT(*) AS count FROM task_results").get()).toEqual({ count: 0 });
    expect(database.prepare("SELECT COUNT(*) AS count FROM messages WHERE kind='response'").get()).toEqual({ count: 0 });
  });

  it("binds the reply conversation to the pending request conversation", () => {
    const database = open();
    const store = new AtomicDeliveryStore({ database, clock: () => 1_000 });
    store.request(chairContext, request());

    expect(() => store.completeWithReply(workerContext, completionFor({
      reply: { conversationId: "conversation_forged" },
    }))).toThrowError("task request ownership or revision changed");
    expect(database.prepare("SELECT state FROM tasks WHERE task_id='task_answer'").get()).toEqual({ state: "active" });
    expect(database.prepare("SELECT state FROM task_requests WHERE request_id='message_request'").get()).toEqual({ state: "pending" });
    expect(database.prepare("SELECT COUNT(*) AS count FROM messages WHERE kind='response'").get()).toEqual({ count: 0 });
  });

  it.each([
    ["empty", []],
    ["unrelated", [{ path: "artifacts/unrelated.md", digest: UNRELATED_ARTIFACT_DIGEST }]],
  ] as const)("requires completion artifact refs to exactly match obligations: %s", (_label, artifactRefs) => {
    const database = open();
    const store = new AtomicDeliveryStore({ database, clock: () => 1_000 });
    store.request(chairContext, request());

    expect(() => store.completeWithReply(workerContext, completionFor({
      reply: { artifactRefs: artifactRefs as TaskCompleteWithReply["reply"]["artifactRefs"] },
    }))).toThrowError("completion artifacts must exactly match task obligations");
    expect(database.prepare("SELECT state FROM tasks WHERE task_id='task_answer'").get()).toEqual({ state: "active" });
    expect(database.prepare("SELECT COUNT(*) AS count FROM artifacts").get()).toEqual({ count: 0 });
  });

  it.each([
    ["lease", { ownerLeaseId: "task-owner:run_01:task_answer:99" }, workerContext, "STALE_LEASE_GENERATION"],
    ["callback", { callbackId: "callback_forged" }, workerContext, "STALE_REVISION"],
    ["task", { taskId: "task_forged" }, workerContext, "NOT_FOUND"],
    ["owner", {}, workerTwoContext, "STALE_REVISION"],
    ["conversation", { reply: { conversationId: "conversation_forged" } }, workerContext, "STALE_REVISION"],
  ] as const)("rejects an existing request forged by %s without partial effects", (_label, overrides, context, code) => {
    const database = open();
    const store = new AtomicDeliveryStore({ database, clock: () => 1_000 });
    store.request(chairContext, request());

    expect(() => store.completeWithReply(context, completionFor(overrides))).toThrowError(
      expect.objectContaining({ code }),
    );
    expect(database.prepare("SELECT state FROM tasks WHERE task_id='task_answer'").get()).toEqual({ state: "active" });
    expect(database.prepare("SELECT status FROM task_owner_leases WHERE task_id='task_answer'").get()).toEqual({ status: "active" });
    expect(database.prepare("SELECT state FROM task_requests WHERE request_id='message_request'").get()).toEqual({ state: "pending" });
    expect(database.prepare("SELECT COUNT(*) AS count FROM task_results").get()).toEqual({ count: 0 });
    expect(database.prepare("SELECT COUNT(*) AS count FROM messages WHERE kind='response'").get()).toEqual({ count: 0 });
  });

  it("rejects a task-result artifact through a symlinked ancestor before registration", () => {
    const database = open();
    const root = mkdtempSync(join(tmpdir(), "result-artifact-authority-"));
    const outside = mkdtempSync(join(tmpdir(), "result-artifact-outside-"));
    roots.push(root, outside);
    const canonicalRoot = realpathSync(root);
    const runDirectory = join(canonicalRoot, ".agent-run", "run_01");
    mkdirSync(runDirectory, { recursive: true });
    rmSync(join(runDirectory, "artifacts"), { recursive: true, force: true });
    symlinkSync(outside, join(runDirectory, "artifacts"));
    database.prepare("UPDATE projects SET canonical_root=? WHERE project_id='project_01'").run(canonicalRoot);
    database.prepare("UPDATE runs SET workspace_root=? WHERE run_id='run_01'").run(canonicalRoot);
    const store = new AtomicDeliveryStore({ database, clock: () => 1_000 });
    store.request(chairContext, request());

    expect(() => store.completeWithReply(workerContext, completion())).toThrowError(
      "artifact source resolves through a symlinked path",
    );
    expect(database.prepare("SELECT COUNT(*) AS count FROM artifacts").get()).toEqual({ count: 0 });
  });

  it("rejects a completion artifact whose digest does not match workspace bytes", () => {
    const database = open();
    const store = new AtomicDeliveryStore({ database, clock: () => 1_000 });
    store.request(chairContext, request());

    expect(() => store.completeWithReply(workerContext, completionFor({
      reply: {
        artifactRefs: [{ path: "artifacts/answer.md", digest: `sha256:${"0".repeat(64)}` }],
      },
    }))).toThrowError("artifact source digest does not match bytes");
    expect(database.prepare("SELECT state FROM tasks WHERE task_id='task_answer'").get()).toEqual({ state: "active" });
    expect(database.prepare("SELECT COUNT(*) AS count FROM artifacts").get()).toEqual({ count: 0 });
    expect(database.prepare("SELECT COUNT(*) AS count FROM messages WHERE kind='response'").get()).toEqual({ count: 0 });
  });

  it.each([
    "results:request:after-task",
    "results:request:after-message",
    "results:request:after-mailbox",
    "results:request:after-request",
  ])("exposes none of the request composite after crash at %s", (failpoint) => {
    const database = open();
    const store = new AtomicDeliveryStore({
      database,
      clock: () => 1_000,
      fault: (label) => {
        if (label === failpoint) throw new Error("crash");
      },
    });

    expect(() => store.request(chairContext, request())).toThrow("crash");
    expect(database.prepare("SELECT count(*) AS count FROM tasks WHERE task_id='task_answer'").get()).toEqual({ count: 0 });
    expect(database.prepare("SELECT count(*) AS count FROM messages WHERE message_id='message_request'").get()).toEqual({ count: 0 });
    expect(database.prepare("SELECT count(*) AS count FROM task_requests").get()).toEqual({ count: 0 });
    expect(database.prepare("SELECT count(*) AS count FROM task_request_barriers").get()).toEqual({ count: 0 });
  });

  it.each([
    "results:complete:after-reply",
    "results:complete:after-task-result",
    "results:complete:after-delivery",
    "results:complete:after-terminal-task",
  ])("exposes none of the reply/result composite after crash at %s", (failpoint) => {
    const database = open();
    new AtomicDeliveryStore({ database, clock: () => 1_000 }).request(chairContext, request());
    const crashing = new AtomicDeliveryStore({
      database,
      clock: () => 2_000,
      fault: (label) => {
        if (label === failpoint) throw new Error("crash");
      },
    });

    expect(() => crashing.completeWithReply(workerContext, completion())).toThrow("crash");
    expect(database.prepare("SELECT count(*) AS count FROM messages WHERE message_id='message_reply'").get()).toEqual({ count: 0 });
    expect(database.prepare("SELECT count(*) AS count FROM task_results").get()).toEqual({ count: 0 });
    expect(database.prepare("SELECT count(*) AS count FROM result_deliveries").get()).toEqual({ count: 0 });
    expect(database.prepare("SELECT state, revision FROM tasks WHERE task_id='task_answer'").get())
      .toEqual({ state: "active", revision: 1 });
  });

  it("commits task/request/recipients/barrier then reply/result/artifacts/pending delivery exactly once", () => {
    const database = open();
    const store = new AtomicDeliveryStore({ database, clock: () => 1_000 });
    const committed = store.request(chairContext, request());
    expect(committed).toEqual({
      taskRevision: 1,
      requestRevision: 1,
      callbackId: "callback_answer",
      callbackGeneration: 1,
    });
    expect(store.request(chairContext, request())).toEqual(committed);
    expect(database.prepare("SELECT state, owner_agent_id FROM tasks WHERE task_id='task_answer'").get())
      .toEqual({ state: "active", owner_agent_id: "worker_01" });
    expect(database.prepare("SELECT state FROM task_request_barriers WHERE request_id='message_request'").get())
      .toEqual({ state: "blocked" });

    const completed = store.completeWithReply(workerContext, completion());
    expect(completed).toMatchObject({
      taskRevision: 2,
      replyRevision: 1,
      resultDelivery: {
        state: "pending",
        callbackId: "callback_answer",
        targetAgentId: "chair_01",
        targetProviderSessionRef: "provider-chair",
        required: true,
      },
    });
    expect(store.completeWithReply(workerContext, completion())).toEqual(completed);
    expect(database.prepare("SELECT state, revision FROM tasks WHERE task_id='task_answer'").get())
      .toEqual({ state: "complete", revision: 2 });
    expect(database.prepare("SELECT count(*) AS count FROM artifacts WHERE task_id='task_answer'").get())
      .toEqual({ count: 1 });
    expect(database.prepare(`
      SELECT member_kind, state FROM project_session_memberships
       WHERE member_id IN ('message_reply', (SELECT artifact_id FROM artifacts WHERE task_id='task_answer'))
       ORDER BY member_kind
    `).all()).toEqual([
      { member_kind: "artifact-obligation", state: "reconciled" },
      { member_kind: "required-message", state: "active" },
    ]);
    expect(database.prepare("SELECT state FROM task_request_barriers WHERE request_id='message_request'").get())
      .toEqual({ state: "blocked" });

    const resultDeliveryId = completed.resultDelivery.resultDeliveryId;
    const claimed = store.claim(chairContext, {
      commandId: "claim_command",
      resultDeliveryId,
      expectedRevision: 1,
      expectedClaimGeneration: 0,
      claimantAgentId: "chair_01",
      claimDeadline: "2099-01-01T00:00:01.000Z",
    } as unknown as ResultDeliveryClaimRequest);
    expect(claimed).toMatchObject({ state: "claimed", revision: 2, claimGeneration: 1 });

    admitProviderActionFixture(database, {
      runId: "run_01",
      actionId: "provider_action_wrong",
      adapterId: "adapter",
      operation: "inject",
      targetAgentId: "chair_01",
      providerSessionGeneration: 1,
      turnLeaseGeneration: 1,
      identityHash: "identity-wrong",
      payloadHash: "payload-wrong",
      payloadJson: "{}",
      status: "accepted",
      historyJson: "[]",
      executionCount: 1,
      idempotencyProven: true,
      updatedAt: 1,
    });
    expect(() => store.providerAccept(integrationContext, {
      commandId: "accept_wrong_command",
      resultDeliveryId,
      expectedRevision: 2,
      claimGeneration: 1,
      providerAdapterId: "adapter",
      providerActionId: "provider_action_wrong",
    } as unknown as ResultDeliveryProviderAcceptRequest)).toThrow(/exact result callback/u);

    const providerPayload = JSON.stringify({
      fabricResultDelivery: resultDeliveryProviderActionBinding(claimed),
    });
    admitProviderActionFixture(database, {
      runId: "run_01",
      actionId: "provider_action_result",
      adapterId: "adapter-secondary",
      operation: "inject",
      targetAgentId: "chair_01",
      providerSessionGeneration: 1,
      turnLeaseGeneration: 1,
      identityHash: "identity-sibling",
      payloadHash: "payload-sibling",
      payloadJson: "{}",
      status: "accepted",
      historyJson: "[]",
      executionCount: 1,
      idempotencyProven: true,
      updatedAt: 1,
    });
    admitProviderActionFixture(database, {
      runId: "run_01",
      actionId: "provider_action_result",
      adapterId: "adapter",
      operation: "inject",
      targetAgentId: "chair_01",
      providerSessionGeneration: 1,
      turnLeaseGeneration: 1,
      identityHash: "identity",
      payloadHash: "payload",
      payloadJson: providerPayload,
      status: "accepted",
      historyJson: "[]",
      executionCount: 1,
      idempotencyProven: true,
      updatedAt: 1,
    });
    const accepted = store.providerAccept(integrationContext, {
      commandId: "accept_command",
      resultDeliveryId,
      expectedRevision: 2,
      claimGeneration: 1,
      providerAdapterId: "adapter",
      providerActionId: "provider_action_result",
    } as unknown as ResultDeliveryProviderAcceptRequest);
    expect(accepted).toMatchObject({ state: "provider-accepted", revision: 3 });
    const consumed = store.consume(chairContext, {
      commandId: "consume_command",
      resultDeliveryId,
      expectedRevision: 3,
      claimGeneration: 1,
      callbackId: "callback_answer",
      payloadDigest: accepted.payloadDigest,
    } as unknown as ResultDeliveryConsumeRequest);
    expect(consumed).toMatchObject({ state: "consumed", revision: 4 });
    expect(database.prepare("SELECT state FROM task_request_barriers WHERE request_id='message_request'").get())
      .toEqual({ state: "released" });
  });

  it("fences a claimed callback at its response deadline and emits one deduplicated native alert", () => {
    let now = Date.parse("2026-07-11T00:00:00.000Z");
    const database = open();
    database.prepare(`
      INSERT INTO daemon_runtime_epochs(
        instance_generation, instance_id, state, started_at, heartbeat_at
      ) VALUES (7, 'daemon-deadline-7', 'running', ?, ?)
    `).run(now, now);
    const store = new AtomicDeliveryStore({ database, clock: () => now });
    store.request(chairContext, requestWithDeadline("2026-07-11T00:00:01.000Z"));
    const completed = store.completeWithReply(workerContext, completion());
    const claimed = store.claim(chairContext, {
      commandId: "deadline_claim_command",
      resultDeliveryId: completed.resultDelivery.resultDeliveryId,
      expectedRevision: 1,
      expectedClaimGeneration: 0,
      claimantAgentId: "chair_01",
      claimDeadline: "2026-07-11T00:10:00.000Z",
    } as unknown as ResultDeliveryClaimRequest);
    expect(claimed).toMatchObject({ state: "claimed", claimGeneration: 1, revision: 2 });

    now = Date.parse("2026-07-11T00:00:02.000Z");
    const first = store.sweepDeadlines({ daemonInstanceGeneration: 7, passGeneration: 1 });
    expect(first).toEqual({
      daemonInstanceGeneration: 7,
      passGeneration: 1,
      overdueDeliveries: 1,
      overdueRequests: 0,
      attentionItems: 1,
      notificationsEnqueued: 1,
    });
    expect(store.get(completed.resultDelivery.resultDeliveryId)).toMatchObject({
      state: "overdue",
      revision: 3,
      claimGeneration: 2,
    });
    expect(database.prepare(`
      SELECT claimed_by, claim_deadline FROM result_deliveries
       WHERE result_delivery_id=?
    `).get(completed.resultDelivery.resultDeliveryId)).toEqual({
      claimed_by: null,
      claim_deadline: null,
    });
    expect(database.prepare("SELECT state FROM task_request_barriers WHERE request_id='message_request'").get())
      .toEqual({ state: "blocked" });
    expect(database.prepare(`
      SELECT kind, severity, state,
             json_extract(payload_json, '$.priority') AS priority
        FROM attention_items
       WHERE dedupe_key='result-overdue:callback_answer'
    `).get()).toEqual({
      kind: "blocked",
      severity: "critical",
      state: "open",
      priority: "critical-path",
    });
    expect(database.prepare(`
      SELECT target_integration, state FROM notification_deliveries
    `).all()).toEqual([{ target_integration: "native-desktop", state: "pending" }]);

    expect(store.sweepDeadlines({ daemonInstanceGeneration: 7, passGeneration: 1 })).toEqual(first);
    expect(store.sweepDeadlines({ daemonInstanceGeneration: 7, passGeneration: 2 })).toEqual({
      daemonInstanceGeneration: 7,
      passGeneration: 2,
      overdueDeliveries: 0,
      overdueRequests: 0,
      attentionItems: 0,
      notificationsEnqueued: 0,
    });
    expect(database.prepare("SELECT COUNT(*) AS count FROM attention_items").get()).toEqual({ count: 1 });
    expect(database.prepare("SELECT COUNT(*) AS count FROM notification_deliveries").get()).toEqual({ count: 1 });
  });

  it("marks an unanswered request overdue live and a late result cannot duplicate its alert or release its barrier", () => {
    let now = Date.parse("2026-07-11T00:00:00.000Z");
    const database = open();
    database.prepare(`
      INSERT INTO daemon_runtime_epochs(
        instance_generation, instance_id, state, started_at, heartbeat_at
      ) VALUES (8, 'daemon-deadline-8', 'running', ?, ?)
    `).run(now, now);
    const store = new AtomicDeliveryStore({ database, clock: () => now });
    store.request(chairContext, requestWithDeadline("2026-07-11T00:00:01.000Z"));
    now = Date.parse("2026-07-11T00:00:02.000Z");

    expect(store.sweepDeadlines({ daemonInstanceGeneration: 8, passGeneration: 1 })).toMatchObject({
      overdueDeliveries: 0,
      overdueRequests: 1,
      attentionItems: 1,
      notificationsEnqueued: 1,
    });
    expect(database.prepare("SELECT state FROM task_requests WHERE request_id='message_request'").get())
      .toEqual({ state: "overdue" });
    const late = store.completeWithReply(workerContext, completion());
    expect(late.resultDelivery).toMatchObject({ state: "overdue" });
    expect(store.sweepDeadlines({ daemonInstanceGeneration: 8, passGeneration: 2 })).toMatchObject({
      overdueDeliveries: 0,
      overdueRequests: 0,
      attentionItems: 0,
      notificationsEnqueued: 0,
    });
    expect(database.prepare("SELECT state FROM task_request_barriers WHERE request_id='message_request'").get())
      .toEqual({ state: "blocked" });
    expect(database.prepare("SELECT COUNT(*) AS count FROM attention_items").get()).toEqual({ count: 1 });
    expect(database.prepare("SELECT COUNT(*) AS count FROM notification_deliveries").get()).toEqual({ count: 1 });
  });

  it("alerts when a late reply creates an overdue delivery before the first live sweep", () => {
    let now = Date.parse("2026-07-11T00:00:00.000Z");
    const database = open();
    database.prepare(`
      INSERT INTO daemon_runtime_epochs(
        instance_generation, instance_id, state, started_at, heartbeat_at
      ) VALUES (9, 'daemon-deadline-9', 'running', ?, ?)
    `).run(now, now);
    const store = new AtomicDeliveryStore({ database, clock: () => now });
    store.request(chairContext, requestWithDeadline("2026-07-11T00:00:01.000Z"));
    now = Date.parse("2026-07-11T00:00:02.000Z");
    const late = store.completeWithReply(workerContext, completion());
    expect(late.resultDelivery).toMatchObject({ state: "overdue" });

    expect(store.sweepDeadlines({ daemonInstanceGeneration: 9, passGeneration: 1 })).toMatchObject({
      overdueDeliveries: 0,
      overdueRequests: 0,
      attentionItems: 1,
      notificationsEnqueued: 1,
    });
    expect(database.prepare("SELECT COUNT(*) AS count FROM attention_items").get()).toEqual({ count: 1 });
    expect(database.prepare("SELECT COUNT(*) AS count FROM notification_deliveries").get()).toEqual({ count: 1 });
    expect(database.prepare("SELECT state FROM task_request_barriers WHERE request_id='message_request'").get())
      .toEqual({ state: "blocked" });
  });
});
