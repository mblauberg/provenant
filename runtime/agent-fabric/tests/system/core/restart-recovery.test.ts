import type Database from "better-sqlite3";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type {
  ResultDeliveryAbandonRequest,
  ResultDeliveryClaimRequest,
  ResultDeliveryProviderAcceptRequest,
  ResultDeliveryReassignRequest,
  ResultDeliveryRetryRequest,
  TaskCompleteWithReply,
  TaskRequest,
} from "@local/agent-fabric-protocol";
import { afterEach, describe, expect, it } from "vitest";

import {
  AtomicDeliveryStore,
  resultDeliveryProviderActionBinding,
  type ResultDeliveryIntegrationContext,
} from "../../../src/results/store.ts";
import {
  chairContext,
  openSystemDatabase,
  reopenSystemDatabase,
  workerContext,
  workerTwoContext,
} from "./restart-recovery-fixtures.ts";
import { admitProviderActionFixture } from "../../support/provider-action-fixture.ts";

const databases: Database.Database[] = [];
const directories: string[] = [];

afterEach(() => {
  for (const database of databases.splice(0)) {
    if (database.open) database.close();
  }
  for (const directory of directories.splice(0)) rmSync(directory, { recursive: true, force: true });
});

const initialNow = Date.parse("2026-07-11T00:00:00.000Z");

function request(responseDeadline: string): TaskRequest {
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
      responseDeadline,
      callbackId: "callback_answer",
      callbackGeneration: 1,
      dependentBarrierId: "barrier_answer",
    },
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
      artifactRefs: [{ path: "artifacts/answer.md", digest: `sha256:${"b".repeat(64)}` }],
    },
    terminalResult: {
      status: "complete",
      summary: "Answer complete.",
      completedAt: "2026-07-11T00:00:01.000Z",
    },
  } as unknown as TaskCompleteWithReply;
}

function completeDelivery(database: Database.Database, clock: () => number, responseDeadline: string) {
  const store = new AtomicDeliveryStore({ database, clock });
  store.request(chairContext, request(responseDeadline));
  const result = store.completeWithReply(workerContext, completion());
  return { store, resultDeliveryId: result.resultDelivery.resultDeliveryId };
}

const integrationContext: ResultDeliveryIntegrationContext = {
  integrationId: "integration_provider",
  projectId: "project_01",
  projectSessionId: "session_01",
  coordinationRunId: "run_01",
  principalGeneration: 1,
};

describe("result-delivery restart recovery", () => {
  it("returns only an expired claim under a higher generation after a real database reopen", () => {
    const directory = mkdtempSync(join(tmpdir(), "fabric-results-restart-"));
    directories.push(directory);
    const filename = join(directory, "fabric.sqlite3");
    let now = initialNow;
    let database = openSystemDatabase(filename);
    databases.push(database);
    const { store, resultDeliveryId } = completeDelivery(
      database,
      () => now,
      "2026-07-11T00:05:00.000Z",
    );
    store.claim(chairContext, {
      commandId: "claim_command",
      resultDeliveryId,
      expectedRevision: 1,
      expectedClaimGeneration: 0,
      claimantAgentId: "chair_01",
      claimDeadline: "2026-07-11T00:01:00.000Z",
    } as unknown as ResultDeliveryClaimRequest);

    database.close();
    now = Date.parse("2026-07-11T00:02:00.000Z");
    database = reopenSystemDatabase(filename);
    databases.push(database);
    const restarted = new AtomicDeliveryStore({ database, clock: () => now });

    expect(restarted.recover()).toEqual({
      returnedClaims: 1,
      overdueDeliveries: 0,
      overdueRequests: 0,
    });
    expect(restarted.get(resultDeliveryId)).toMatchObject({
      state: "pending",
      revision: 3,
      claimGeneration: 2,
    });
    expect(database.prepare("SELECT count(*) AS count FROM provider_actions").get()).toEqual({ count: 0 });
    expect(database.prepare("SELECT state FROM task_request_barriers WHERE request_id='message_request'").get())
      .toEqual({ state: "blocked" });
    expect(restarted.recover()).toEqual({ returnedClaims: 0, overdueDeliveries: 0, overdueRequests: 0 });
  });

  it("preserves provider-accepted delivery across restart even after the response deadline", () => {
    let now = initialNow;
    const database = openSystemDatabase();
    databases.push(database);
    const { store, resultDeliveryId } = completeDelivery(
      database,
      () => now,
      "2026-07-11T00:05:00.000Z",
    );
    const claimed = store.claim(chairContext, {
      commandId: "claim_command",
      resultDeliveryId,
      expectedRevision: 1,
      expectedClaimGeneration: 0,
      claimantAgentId: "chair_01",
      claimDeadline: "2026-07-11T00:10:00.000Z",
    } as unknown as ResultDeliveryClaimRequest);
    const providerPayload = JSON.stringify({
      fabricResultDelivery: resultDeliveryProviderActionBinding(claimed),
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
    store.providerAccept(integrationContext, {
      commandId: "accept_command",
      resultDeliveryId,
      expectedRevision: 2,
      claimGeneration: 1,
      providerAdapterId: "adapter",
      providerActionId: "provider_action_result",
    } as unknown as ResultDeliveryProviderAcceptRequest);

    now = Date.parse("2026-07-11T00:06:00.000Z");
    const restarted = new AtomicDeliveryStore({ database, clock: () => now });
    expect(restarted.recover()).toEqual({ returnedClaims: 0, overdueDeliveries: 0, overdueRequests: 0 });
    expect(restarted.get(resultDeliveryId)).toMatchObject({
      state: "provider-accepted",
      revision: 3,
      claimGeneration: 1,
    });
    expect(database.prepare("SELECT count(*) AS count FROM provider_actions").get()).toEqual({ count: 1 });
  });

  it("keeps overdue barriers blocked through explicit retry/reassignment and releases only on abandon", () => {
    let now = initialNow;
    const database = openSystemDatabase();
    databases.push(database);
    const { store, resultDeliveryId } = completeDelivery(
      database,
      () => now,
      "2026-07-11T00:01:00.000Z",
    );
    now = Date.parse("2026-07-11T00:02:00.000Z");
    expect(store.recover()).toEqual({ returnedClaims: 0, overdueDeliveries: 1, overdueRequests: 0 });

    const retryRequest = {
      commandId: "retry_command",
      resultDeliveryId,
      expectedRevision: 2,
      sameCallbackId: "callback_answer",
      reason: "Retry the exact callback after inspection.",
    } as unknown as ResultDeliveryRetryRequest;
    const retried = store.retry(chairContext, retryRequest);
    expect(retried).toMatchObject({ state: "pending", revision: 3, callbackId: "callback_answer" });
    expect(store.retry(chairContext, retryRequest)).toEqual(retried);
    expect(database.prepare("SELECT state FROM task_request_barriers WHERE request_id='message_request'").get())
      .toEqual({ state: "blocked" });

    expect(store.recover()).toEqual({ returnedClaims: 0, overdueDeliveries: 1, overdueRequests: 0 });
    const reassigned = store.reassign(chairContext, {
      commandId: "reassign_command",
      resultDeliveryId,
      expectedRevision: 4,
      targetAgentId: "worker_02",
      targetProviderSessionRef: "provider-worker-2",
      reason: "Move the exact callback to the alternate lead.",
    } as unknown as ResultDeliveryReassignRequest);
    expect(reassigned).toMatchObject({
      state: "pending",
      revision: 5,
      assignmentGeneration: 2,
      targetAgentId: "worker_02",
      callbackId: "callback_answer",
    });
    expect(database.prepare("SELECT count(*) AS count FROM provider_actions").get()).toEqual({ count: 0 });
    expect(database.prepare("SELECT state FROM task_request_barriers WHERE request_id='message_request'").get())
      .toEqual({ state: "blocked" });

    expect(store.recover()).toEqual({ returnedClaims: 0, overdueDeliveries: 1, overdueRequests: 0 });
    const abandoned = store.abandon(workerTwoContext, {
      commandId: "abandon_command",
      resultDeliveryId,
      expectedRevision: 6,
      reason: "Human explicitly abandoned the dependent obligation.",
    } as unknown as ResultDeliveryAbandonRequest);
    expect(abandoned).toMatchObject({ state: "abandoned", revision: 7 });
    expect(database.prepare("SELECT state FROM task_request_barriers WHERE request_id='message_request'").get())
      .toEqual({ state: "abandoned" });
    expect(store.recover()).toEqual({ returnedClaims: 0, overdueDeliveries: 0, overdueRequests: 0 });
  });
});
