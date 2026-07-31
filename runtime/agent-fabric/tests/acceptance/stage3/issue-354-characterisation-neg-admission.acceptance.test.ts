import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { ProviderActionAdmissionCoordinator } from "../../../src/application/provider-action-admission.ts";
import type { ProviderActionInsert } from "../../../src/application/provider-action-admission.ts";
import { createLifecycleFixture } from "../../support/lifecycle-testkit.ts";
import { eventually } from "../../shared/deadline-wait.ts";
import {
  actionSnapshot,
  bindMinimalLifecycleOwner,
  bindProviderAgentOwner,
  closeFixture,
  corruptOwner,
  durableSnapshot,
  readFakeJournal,
  seedProviderAction,
} from "../../support/w354-characterisation-testkit.ts";

const cleanup: Array<() => Promise<void>> = [];

afterEach(async () => {
  await Promise.all(cleanup.splice(0).map((close) => close()));
});

function actionInput(actionId: string) {
  return {
    certifyingReview: null,
    adapterId: "fake-lifecycle",
    actionId,
    operation: "wakeup" as const,
    payload: {},
    commandId: `${actionId}:command`,
  };
}

async function preparedSpawnAtFault(label: string): Promise<{
  fixture: Awaited<ReturnType<typeof createLifecycleFixture>>;
  ref: { runId: string; adapterId: string; actionId: string };
  close: () => Promise<void>;
}> {
  const actionId = `issue-354:${label}`;
  let databasePath: string | undefined;
  const fixture = await createLifecycleFixture({
    fault: (faultLabel) => {
      if (faultLabel !== label) return;
      if (databasePath === undefined) throw new Error("fault fired before fixture database was available");
      const database = new Database(databasePath);
      bindProviderAgentOwner(database, { runId: "run-stage3", adapterId: "fake-lifecycle", actionId });
      database.close();
    },
  });
  databasePath = fixture.databasePath;
  const close = closeFixture(cleanup, fixture);
  const authority = await fixture.chair.delegateAuthority({
    parentAuthorityId: fixture.chairAuthorityId,
    authority: {
      ...fixture.rootAuthority,
      sourcePaths: ["src/leader"],
      actions: [...fixture.rootAuthority.actions],
      budget: { turns: 1 },
    },
    commandId: `${actionId}:authority`,
  });
  const request = {
    certifyingReview: null,
    adapterId: "fake-lifecycle",
    actionId,
    operation: "spawn" as const,
    authorityId: authority.authorityId,
    taskId: fixture.leaderTask.taskId,
    payload: {
      model: "fake-reviewer-v1",
      modelFamily: "fake",
      prompt: "characterise the deferred owner fence",
      cwd: "src/leader",
    },
    commandId: `${actionId}:dispatch`,
  };
  await expect(fixture.chair.dispatchProviderAction(request)).rejects.toMatchObject({
    name: "ProviderActionOwnerError",
    expectedOwner: "generic",
    actualOwner: "provider_agent",
  });
  return {
    fixture,
    ref: { runId: fixture.runId, adapterId: "fake-lifecycle", actionId },
    close,
  };
}

describe("S1 NEG: representative owner fences before provider effects", () => {
  // CITE: concurrent-reconcile revalidation is already the oracle in ambiguous-provider-action.acceptance.test.ts:2985.
  // CITE: the certifying deferred claim/completion arm is already exercised in ambiguous-provider-action.acceptance.test.ts:3048-3147.

  it.each([
    { id: "NEG-1 live dispatch", owner: "provider_agent" as const, bind: bindProviderAgentOwner, call: "dispatch" as const },
    { id: "NEG-3 live reconcile", owner: "lifecycle" as const, bind: bindMinimalLifecycleOwner, call: "reconcile" as const },
    { id: "NEG-7 live dispatch integrity failure", owner: "integrity_failed" as const, bind: corruptOwner, call: "dispatch" as const },
    { id: "NEG-8 live reconcile integrity failure", owner: "integrity_failed" as const, bind: corruptOwner, call: "reconcile" as const },
  ])("$id asserts before lookup or dispatch", async ({ owner, bind, call, id }) => {
    const fixture = await createLifecycleFixture();
    const close = closeFixture(cleanup, fixture);
    const actionId = `issue-354:${id}`;
    const database = new Database(fixture.databasePath);
    const ref = seedProviderAction(database, {
      runId: fixture.runId,
      actionId,
      status: call === "dispatch" ? "prepared" : "dispatched",
      historyJson: call === "dispatch" ? '["prepared"]' : '["prepared","dispatched"]',
      executionCount: call === "dispatch" ? 0 : 1,
    });
    bind(database, ref);
    database.close();

    const before = durableSnapshot(fixture.databasePath, fixture.runId, ref);
    if (call === "dispatch") {
      await expect(fixture.chair.dispatchProviderAction(actionInput(actionId))).rejects.toMatchObject({
        name: "ProviderActionOwnerError",
        expectedOwner: "generic",
        actualOwner: owner,
      });
    } else {
      await expect(fixture.chair.reconcileProviderAction({
        adapterId: ref.adapterId,
        actionId: ref.actionId,
        commandId: `${actionId}:reconcile`,
      })).rejects.toMatchObject({
        name: "ProviderActionOwnerError",
        expectedOwner: "generic",
        actualOwner: owner,
      });
    }
    const after = durableSnapshot(fixture.databasePath, fixture.runId, ref);
    expect(after).toEqual(before);
    expect(actionSnapshot(fixture.databasePath, ref)).toMatchObject({ effect_count: 0 });
    const journal = await readFakeJournal(fixture.providerJournalPath);
    expect((journal.actions as Record<string, unknown>)[actionId]).toBeUndefined();
    await close();
  });

  it("NEG-2 revalidates a now-foreign owner before dispatch replay acknowledgement", async () => {
    const fixture = await createLifecycleFixture();
    const close = closeFixture(cleanup, fixture);
    const request = {
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "issue-354:NEG-2",
      operation: "wakeup" as const,
      payload: {},
      commandId: "issue-354:NEG-2:dispatch",
    };
    await expect(fixture.chair.dispatchProviderAction(request)).resolves.toMatchObject({ status: "terminal", effectCount: 1 });
    const before = durableSnapshot(fixture.databasePath, fixture.runId, {
      runId: fixture.runId,
      adapterId: request.adapterId,
      actionId: request.actionId,
    });
    const journalBefore = await readFakeJournal(fixture.providerJournalPath);
    const database = new Database(fixture.databasePath);
    bindProviderAgentOwner(database, { runId: fixture.runId, adapterId: request.adapterId, actionId: request.actionId });
    database.close();

    await expect(fixture.chair.dispatchProviderAction(request)).rejects.toMatchObject({
      name: "ProviderActionOwnerError",
      expectedOwner: "generic",
      actualOwner: "provider_agent",
    });
    expect(durableSnapshot(fixture.databasePath, fixture.runId, {
      runId: fixture.runId,
      adapterId: request.adapterId,
      actionId: request.actionId,
    })).toEqual(before);
    expect(await readFakeJournal(fixture.providerJournalPath)).toEqual(journalBefore);
    await close();
  });

  it.each([
    { id: "NEG-5 deferred enqueue", label: "provider-action-owner:before-deferred-enqueue", status: "prepared", executionCount: 0 },
    { id: "NEG-6 deferred claim", label: "provider-action-owner:before-deferred-claim", status: "prepared", executionCount: 0 },
    { id: "NEG-6 deferred pre-completion", label: "provider-action-owner:before-deferred-completion", status: "dispatched", executionCount: 1 },
  ])("$id removes the owned operation after the owner fence", async ({ label, status, executionCount }) => {
    const { fixture, ref, close } = await preparedSpawnAtFault(label);
    await eventually(() => {
      expect(actionSnapshot(fixture.databasePath, ref)).toMatchObject({
        status,
        execution_count: executionCount,
        effect_count: 0,
      });
    });
    const journal = await readFakeJournal(fixture.providerJournalPath);
    expect((journal.actions as Record<string, unknown>)[ref.actionId]).toBeUndefined();
    await close();
  });
});

describe("S1 ADM: admission owner revalidation fence", () => {
  // CITE: admission crash cuts remain covered by the existing stage3 fault-parameterized tests.

  it.each([
    { id: "foreign provider-agent owner", corrupt: false },
    { id: "crossed integrity owner", corrupt: true },
  ])("ADM-1 rolls back the row, dependant and budget reservation for $id", ({ corrupt }) => {
    const fixturePromise = createLifecycleFixture();
    return fixturePromise.then((fixture) => {
      const close = closeFixture(cleanup, fixture);
      const actionId = `issue-354:ADM-1:${corrupt ? "integrity" : "foreign"}`;
      const database = new Database(fixture.databasePath);
      database.pragma("foreign_keys = OFF");
      const coordinator = new ProviderActionAdmissionCoordinator({
        database,
        clock: () => fixture.clock.now().getTime(),
      });
      const ticket = coordinator.preflightAgentAction({
        runId: fixture.runId,
        actorAgentId: "chair",
        actionRef: { adapterId: "fake-lifecycle", actionId },
        canonicalInput: { schemaVersion: 1, kind: "issue-354-admission", actionId },
      });
      const authorityId = database.prepare("SELECT authority_id FROM agents WHERE run_id=? AND agent_id='leader'").pluck().get(fixture.runId);
      if (typeof authorityId !== "string") throw new Error("admission budget authority is unavailable");
      const action: ProviderActionInsert = {
        runId: fixture.runId,
        actionId,
        adapterId: "fake-lifecycle",
        operation: "spawn",
        targetAgentId: null,
        providerSessionGeneration: null,
        turnLeaseGeneration: null,
        identityHash: "c".repeat(64),
        payloadHash: "d".repeat(64),
        payloadJson: '{"maxTurns":1,"modelFamily":"fake","prompt":"admission","taskId":"leader-task"}',
        status: "prepared",
        historyJson: '["prepared"]',
        executionCount: 0,
        taskId: fixture.leaderTask.taskId,
        budgetAuthorityId: authorityId,
        budgetReservationJson: '{"turns":1}',
        budgetState: "reserved",
        budgetStartedAt: fixture.clock.now().getTime(),
        updatedAt: fixture.clock.now().getTime(),
      };
      const before = durableSnapshot(fixture.databasePath, fixture.runId);
      expect(() => database.transaction(() => coordinator.admitUnroutedInCurrentTransaction(
        ticket,
        action,
        "generic",
        () => {
          const ref = { runId: fixture.runId, adapterId: "fake-lifecycle", actionId };
          if (corrupt) corruptOwner(database, ref);
          else bindProviderAgentOwner(database, ref);
        },
      ))()).toThrowError(expect.objectContaining({
        name: "ProviderActionAdmissionTransactionError",
        code: "CAPABILITY_FORBIDDEN",
      }));
      expect(durableSnapshot(fixture.databasePath, fixture.runId)).toEqual(before);
      expect(database.prepare("SELECT state FROM provider_action_pair_preflights WHERE adapter_id=? AND action_id=?").get("fake-lifecycle", actionId)).toMatchObject({ state: "resolving" });
      expect(database.prepare("SELECT COUNT(*) AS count FROM provider_actions WHERE run_id=? AND action_id=?").get(fixture.runId, actionId)).toEqual({ count: 0 });
      database.close();
      return close();
    });
  });
});
