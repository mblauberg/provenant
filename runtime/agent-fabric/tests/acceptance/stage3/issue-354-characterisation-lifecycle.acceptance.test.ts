import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { classifyProviderActionOwner } from "../../../src/application/provider-action-owner.ts";
import {
  createLifecycleFixture,
  writeLifecycleCheckpoint,
} from "../../support/lifecycle-testkit.ts";
import { eventually } from "../../shared/deadline-wait.ts";
import {
  actionSnapshot,
  closeFixture,
  readFakeJournal,
} from "../../support/w354-characterisation-testkit.ts";

const cleanup: Array<() => Promise<void>> = [];

afterEach(async () => {
  await Promise.all(cleanup.splice(0).map((close) => close()));
});

async function prepareLeaderRelease(fixture: Awaited<ReturnType<typeof createLifecycleFixture>>) {
  const blockedCheckpoint = await writeLifecycleCheckpoint(fixture, {
    agentId: "leader",
    inFlightChildren: ["child"],
    openWork: ["leader-task"],
    nextAction: "complete the run before release",
  });
  await fixture.leader.requestLifecycle({
    action: "completion-ready",
    agentId: "leader",
    taskId: fixture.leaderTask.taskId,
    taskRevision: fixture.leaderTask.revision,
    checkpoint: blockedCheckpoint,
    commandId: "issue-354:release:leader-ready",
  });
  const childTask = await fixture.child.updateTask({
    taskId: fixture.childTask.taskId,
    expectedRevision: fixture.childTask.revision,
    state: "complete",
    commandId: "issue-354:release:child-complete",
  });
  const leaderTask = await fixture.leader.updateTask({
    taskId: fixture.leaderTask.taskId,
    expectedRevision: fixture.leaderTask.revision,
    state: "complete",
    commandId: "issue-354:release:leader-complete",
  });
  const childCheckpoint = await writeLifecycleCheckpoint(fixture, { agentId: "child", nextAction: "release" });
  await fixture.child.requestLifecycle({
    action: "completion-ready",
    agentId: "child",
    taskId: childTask.taskId,
    taskRevision: childTask.revision,
    checkpoint: childCheckpoint,
    commandId: "issue-354:release:child-ready",
  });
  const finalCheckpoint = await writeLifecycleCheckpoint(fixture, { agentId: "leader", nextAction: "release" });
  await fixture.leader.requestLifecycle({
    action: "completion-ready",
    agentId: "leader",
    taskId: leaderTask.taskId,
    taskRevision: leaderTask.revision,
    checkpoint: finalCheckpoint,
    commandId: "issue-354:release:leader-ready-final",
  });
  await fixture.chair.closeBarrier({ scope: "run", commandId: "issue-354:release:barrier-close" });
  await fixture.child.requestLifecycle({
    action: "release",
    agentId: "child",
    taskId: childTask.taskId,
    taskRevision: childTask.revision,
    checkpoint: childCheckpoint,
    commandId: "issue-354:release:child-release",
  });
  return { leaderTask, finalCheckpoint };
}

function readLifecycleAction(fixture: Awaited<ReturnType<typeof createLifecycleFixture>>, actionId: string): Record<string, unknown> {
  const database = new Database(fixture.databasePath, { readonly: true });
  try {
    return database.prepare(`
      SELECT status,history_json,execution_count,effect_count,journal_revision,
             task_id,budget_authority_id,budget_reservation_json,budget_settlement_json,
             budget_state,budget_started_at,finding_capacity_reservation_digest
        FROM provider_actions WHERE run_id=? AND adapter_id='fake-lifecycle' AND action_id=?
    `).get(fixture.runId, actionId) as Record<string, unknown>;
  } finally {
    database.close();
  }
}

describe("S1 E2-REL: lifecycle release custody and persistence order", () => {
  it("E2R-1 persists release as generic with null task and budget columns and revalidates replay", async () => {
    const fixture = await createLifecycleFixture();
    const close = closeFixture(cleanup, fixture);
    const { leaderTask, finalCheckpoint } = await prepareLeaderRelease(fixture);
    const commandId = "issue-354:release:leader";
    const request = {
      action: "release" as const,
      agentId: "leader",
      taskId: leaderTask.taskId,
      taskRevision: leaderTask.revision,
      checkpoint: finalCheckpoint,
      commandId,
    };
    const released = await fixture.leader.requestLifecycle(request);
    expect(released).toMatchObject({ agentId: "leader", lifecycle: "archived" });
    const actionId = `${commandId}:release`;
    const database = new Database(fixture.databasePath, { readonly: true });
    const row = database.prepare(`
      SELECT status,history_json,execution_count,effect_count,task_id,budget_authority_id,
             budget_reservation_json,budget_settlement_json,budget_state,budget_started_at
        FROM provider_actions WHERE run_id=? AND adapter_id='fake-lifecycle' AND action_id=?
    `).get(fixture.runId, actionId) as Record<string, unknown>;
    const custodyCount = database.prepare(`
      SELECT COUNT(*) AS count FROM lifecycle_operations WHERE run_id=? AND agent_id='leader' AND action='release'
    `).get(fixture.runId);
    database.close();
    const ownerDatabase = new Database(fixture.databasePath, { readonly: true });
    expect(classifyProviderActionOwner(ownerDatabase, {
      runId: fixture.runId, adapterId: "fake-lifecycle", actionId,
    })).toBe("generic");
    ownerDatabase.close();
    expect(row).toMatchObject({
      status: "terminal",
      history_json: '["prepared","dispatched","accepted","terminal"]',
      execution_count: 1,
      effect_count: 1,
      task_id: null,
      budget_authority_id: null,
      budget_reservation_json: null,
      budget_settlement_json: null,
      budget_state: null,
      budget_started_at: null,
    });
    expect(custodyCount).toEqual({ count: 1 });
    const firstJournal = await readFakeJournal(fixture.providerJournalPath);
    const replay = await fixture.chair.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId,
      commandId: `${commandId}:generic-reconcile`,
    });
    expect(replay).toMatchObject({ actionId, status: "terminal", effectCount: 1 });
    expect(await readFakeJournal(fixture.providerJournalPath)).toEqual(firstJournal);

    const crossed = new Database(fixture.databasePath);
    crossed.pragma("foreign_keys = OFF");
    crossed.prepare(`UPDATE provider_actions SET finding_capacity_reservation_digest=? WHERE run_id=? AND adapter_id=? AND action_id=?`)
      .run(`sha256:${"e".repeat(64)}`, fixture.runId, "fake-lifecycle", actionId);
    crossed.close();
    const before = actionSnapshot(fixture.databasePath, { runId: fixture.runId, adapterId: "fake-lifecycle", actionId });
    await expect(fixture.chair.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId,
      commandId: `${commandId}:generic-reconcile-crossed`,
    })).rejects.toMatchObject({
      name: "ProviderActionOwnerError",
      expectedOwner: "generic",
      actualOwner: "integrity_failed",
    });
    expect(actionSnapshot(fixture.databasePath, { runId: fixture.runId, adapterId: "fake-lifecycle", actionId })).toEqual(before);
    await close();
  });

  it("E2R-2 keeps rotate provider actions under lifecycle custody", async () => {
    const fixture = await createLifecycleFixture({ retainedAgents: true, spawnDelayMs: 25 });
    const close = closeFixture(cleanup, fixture);
    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader",
      inFlightChildren: ["child"],
      openWork: ["leader-task"],
      nextAction: "rotate and classify the replacement",
    });
    const commandId = "issue-354:rotate:owner";
    await expect(fixture.leader.requestLifecycle({
      action: "rotate",
      agentId: "leader",
      taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision,
      checkpoint,
      commandId,
    })).resolves.toMatchObject({ kind: "accepted-suspended" });
    const actionId = `${commandId}:spawn`;
    await eventually(() => expect(readLifecycleAction(fixture, actionId)).toMatchObject({ status: "terminal", effect_count: 1 }));
    const database = new Database(fixture.databasePath, { readonly: true });
    expect(classifyProviderActionOwner(database, { runId: fixture.runId, adapterId: "fake-lifecycle", actionId })).toBe("lifecycle");
    database.close();
    await close();
  });

  it("E2R-3 deleted=true keeps the durable generic release row before the failed proof branch", async () => {
    const fixture = await createLifecycleFixture();
    const close = closeFixture(cleanup, fixture);
    const { leaderTask } = await prepareLeaderRelease(fixture);
    const commandId = "issue-354:release:proof:deleted";
    // Drive the fake adapter's destructive-release result variant purely through the
    // test-controlled provider_session_ref (see lifecycle-fake-provider.ts release branch). The
    // release checkpoint's resume reference must equal the agent's session ref (fabric.ts:10448),
    // so both carry the ":release-deleted" sentinel.
    const setup = new Database(fixture.databasePath);
    const currentRef = setup.prepare("SELECT provider_session_ref FROM agents WHERE run_id=? AND agent_id='leader'").pluck().get(fixture.runId) as string;
    const deletedRef = `${currentRef}:release-deleted`;
    setup.prepare("UPDATE agents SET provider_session_ref=? WHERE run_id=? AND agent_id='leader'").run(deletedRef, fixture.runId);
    setup.close();
    const finalCheckpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader",
      nextAction: "release",
      providerResumeReference: deletedRef,
    });
    const setup2 = new Database(fixture.databasePath, { readonly: true });
    const beforeCounts = {
      operations: setup2.prepare("SELECT COUNT(*) AS count FROM lifecycle_operations WHERE run_id=? AND agent_id='leader' AND action='release'").get(fixture.runId),
      events: setup2.prepare("SELECT COUNT(*) AS count FROM events WHERE run_id=?").get(fixture.runId),
      lifecycle: setup2.prepare("SELECT lifecycle FROM agents WHERE run_id=? AND agent_id='leader'").pluck().get(fixture.runId),
      releaseCommands: setup2.prepare("SELECT COUNT(*) AS count FROM commands WHERE run_id=? AND command_id=?").get(fixture.runId, commandId),
    };
    setup2.close();
    await expect(fixture.leader.requestLifecycle({
      action: "release",
      agentId: "leader",
      taskId: leaderTask.taskId,
      taskRevision: leaderTask.revision,
      checkpoint: finalCheckpoint,
      commandId,
    })).rejects.toMatchObject({ code: "LIFECYCLE_PRECONDITION_FAILED" });
    // The generic release row is nevertheless durable at 1/1 (persist-before-proof ordering).
    const actionId = `${commandId}:release`;
    const row = readLifecycleAction(fixture, actionId);
    expect(row).toMatchObject({
      status: "terminal",
      history_json: '["prepared","dispatched","accepted","terminal"]',
      execution_count: 1,
      effect_count: 1,
    });
    const ownerDatabase = new Database(fixture.databasePath, { readonly: true });
    expect(classifyProviderActionOwner(ownerDatabase, { runId: fixture.runId, adapterId: "fake-lifecycle", actionId })).toBe("generic");
    ownerDatabase.close();
    // The agent is NOT archived and no release lifecycle op, command or new event was recorded.
    const after = new Database(fixture.databasePath, { readonly: true });
    expect(after.prepare("SELECT lifecycle FROM agents WHERE run_id=? AND agent_id='leader'").pluck().get(fixture.runId)).toBe(beforeCounts.lifecycle);
    expect(after.prepare("SELECT COUNT(*) AS count FROM lifecycle_operations WHERE run_id=? AND agent_id='leader' AND action='release'").get(fixture.runId)).toEqual(beforeCounts.operations);
    expect(after.prepare("SELECT COUNT(*) AS count FROM commands WHERE run_id=? AND command_id=?").get(fixture.runId, commandId)).toEqual(beforeCounts.releaseCommands);
    expect(after.prepare("SELECT COUNT(*) AS count FROM events WHERE run_id=?").get(fixture.runId)).toEqual(beforeCounts.events);
    after.close();
    // Retry reproduces the rejection without a second adapter effect.
    await expect(fixture.leader.requestLifecycle({
      action: "release",
      agentId: "leader",
      taskId: leaderTask.taskId,
      taskRevision: leaderTask.revision,
      checkpoint: finalCheckpoint,
      commandId,
    })).rejects.toMatchObject({ code: "LIFECYCLE_PRECONDITION_FAILED" });
    expect(readLifecycleAction(fixture, actionId)).toMatchObject({ status: "terminal", effect_count: 1 });
    await close();
  });

  // The prior "successful proof branch" E2R-3 body is cut: it hard-coded `deleted = false`, so its
  // `if (deleted)` branch was dead code, and its deleted=false assertions (durable generic release
  // row + one lifecycle_operations row) fully duplicated E2R-1 above. The genuine deleted=true case
  // is pinned by the test above; E2R-1 remains the one clean deleted=false pin.
});

describe("S1 E3: lifecycle continuation keys, ordering and custody dedupe", () => {
  // CITE: lifecycle crash cuts and replay coalescing remain in lifecycle-checkpoint.acceptance.test.ts:264,327,382,448.

  // Read APIs (getProviderAction/reconcileProviderAction) are not exposed on the retained
  // fixture's hand-rolled chair/leader clients, so this case pins the distinct-key property
  // directly against the durable custody row (fabric.ts:9239 vs the standard key helper 1503).
  it("E3-1 keeps the lifecycle continuation action distinct from standard action lookup", async () => {
    const fixture = await createLifecycleFixture({ retainedAgents: true, spawnDelayMs: 25 });
    const close = closeFixture(cleanup, fixture);
    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader", inFlightChildren: ["child"], openWork: ["leader-task"], nextAction: "hold continuation",
    });
    const commandId = "issue-354:e3:key";
    await fixture.leader.requestLifecycle({
      action: "rotate", agentId: "leader", taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision, checkpoint, commandId,
    });
    const actionId = `${commandId}:spawn`;
    // The replacement spawn is dispatched exactly once and rides the lifecycle cone.
    await eventually(() => expect(readLifecycleAction(fixture, actionId)).toMatchObject({
      status: "terminal", effect_count: 1, execution_count: 1,
    }));
    const database = new Database(fixture.databasePath, { readonly: true });
    expect(classifyProviderActionOwner(database, { runId: fixture.runId, adapterId: "fake-lifecycle", actionId })).toBe("lifecycle");
    const custody = database.prepare(`
      SELECT custody_id, provider_action_id
        FROM lifecycle_rotation_custodies WHERE run_id=? AND command_id=?
    `).get(fixture.runId, commandId) as Record<string, string>;
    database.close();
    // Genuine pin: the rotate custody row links to the SAME spawn action that classifies
    // "lifecycle" above.
    expect(custody.provider_action_id).toBe(actionId);
    // NOT pinned here: the two key namespaces (fabric.ts:9239 lifecycle-prefixed vs the standard
    // key at fabric.ts:1503) were previously asserted disjoint by comparing two strings built
    // entirely from test literals/prefixes -- that would stay green even after a key-unification
    // refactor that merged the schemes, so it was removed as false confidence. The retained
    // fixture exposes no provider-action read API to observe the actual in-memory key used by the
    // reconcile fast-path guard (fabric.ts:6392).
    // TODO(S2): verify #ownedProviderActions key schemes stay disjoint during extraction.
    await close();
  });

  // CHARACTERISATION: this pins replay-coalescing (schedule-once), NOT predecessor ordering.
  // Two identical rotate requests sharing one commandId are coalesced by #requestLifecycle command-
  // journal replay (fabric.ts:5637) before either reaches #scheduleLifecycleContinuation, so the
  // continuation is scheduled once regardless of whether the predecessor await actually orders
  // anything. Deleting `await predecessor` (fabric.ts:9245) would stay green here.
  // TODO(S2): predecessor-await ordering (fabric.ts:9245) is not observed by this test.
  it("E3-2 coalesces a repeat rotate into a single scheduled continuation via command-journal replay", async () => {
    const fixture = await createLifecycleFixture({ retainedAgents: true, spawnDelayMs: 100 });
    const close = closeFixture(cleanup, fixture);
    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader", inFlightChildren: ["child"], openWork: ["leader-task"], nextAction: "settle predecessor first",
    });
    const request = {
      action: "rotate" as const, agentId: "leader", taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision, checkpoint, commandId: "issue-354:e3:predecessor",
    };
    const first = await fixture.leader.requestLifecycle(request);
    const second = await fixture.leader.requestLifecycle(request);
    expect(second).toEqual(first);
    await eventually(() => expect(readLifecycleAction(fixture, `${request.commandId}:spawn`)).toMatchObject({ effect_count: 1 }));
    const journal = await readFakeJournal(fixture.providerJournalPath);
    const actions = journal.actions as Record<string, { executionCount: number; effectCount: number }>;
    expect(actions[`${request.commandId}:spawn`]).toMatchObject({ executionCount: 1, effectCount: 1 });
    await close();
  });

  // CHARACTERISATION of the true base behaviour on a repeat identical rotate.
  // Two identical requestLifecycle calls share one commandId, so #requestLifecycle replays the
  // second from the command journal (fabric.ts:5637) and the retained send_turn action replays
  // too; the continuation is therefore SCHEDULED ONCE (fabric.ts:9203) via replay coalescing, the
  // same mechanism E3-2 pins -- NOT the has(key) dedupe guard at fabric.ts:9240, which this test
  // does not exercise (a second live call never reaches #scheduleLifecycleContinuation to test that
  // guard). That single continuation dispatches the replacement spawn exactly once (effect_count 1)
  // and then FAILS at the lifecycle terminal-apply step because this fixture supplies no external
  // receipt authority (fabric.ts:9506-9512, CAPABILITY_UNAVAILABLE). The failure is not a
  // ProviderActionOwnerError, so the catch at fabric.ts:9250-9257 emits exactly ONE
  // lifecycle-continuation-failed event. The result is observable as: one spawn effect and one
  // failure event despite two requests. S2 must preserve this count of 1.
  it("E3-3 schedules a rotate continuation once via replay coalescing and emits its single terminal-apply failure", async () => {
    const fixture = await createLifecycleFixture({ retainedAgents: true, spawnDelayMs: 25 });
    const close = closeFixture(cleanup, fixture);
    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader", inFlightChildren: ["child"], openWork: ["leader-task"], nextAction: "dedupe callback",
    });
    const request = {
      action: "rotate" as const, agentId: "leader", taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision, checkpoint, commandId: "issue-354:e3:dedupe",
    };
    const first = await fixture.leader.requestLifecycle(request);
    const second = await fixture.leader.requestLifecycle(request);
    expect(second).toEqual(first);
    await eventually(() => expect(readLifecycleAction(fixture, `${request.commandId}:spawn`)).toMatchObject({ status: "terminal", effect_count: 1, execution_count: 1 }));
    await eventually(() => {
      const database = new Database(fixture.databasePath, { readonly: true });
      try {
        expect(database.prepare(`SELECT COUNT(*) AS count FROM events WHERE run_id=? AND type='lifecycle-continuation-failed'`).get(fixture.runId)).toEqual({ count: 1 });
      } finally {
        database.close();
      }
    });
    const database = new Database(fixture.databasePath, { readonly: true });
    // The single failure carries the terminal-apply receipt-authority cause, not a duplicate/race.
    const failure = database.prepare(`SELECT payload_json FROM events WHERE run_id=? AND type='lifecycle-continuation-failed'`).get(fixture.runId) as { payload_json: string };
    expect(failure.payload_json).toContain("lifecycle terminal apply requires an external receipt authority");
    // Exactly one replacement spawn effect despite the two requests.
    expect(database.prepare(`SELECT COUNT(*) AS count FROM provider_actions WHERE run_id=? AND action_id=?`).get(fixture.runId, `${request.commandId}:spawn`)).toEqual({ count: 1 });
    database.close();
    await close();
  });
});
