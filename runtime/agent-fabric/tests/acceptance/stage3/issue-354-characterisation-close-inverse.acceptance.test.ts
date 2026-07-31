import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { settle } from "../../shared/deadline-wait.ts";
import {
  createLifecycleFixture,
  writeLifecycleCheckpoint,
} from "../../support/lifecycle-testkit.ts";
import {
  actionSnapshot,
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

describe("S1 CLOSE: shared close fence and owned-operation drain", () => {
  it("CLOSE-1 rejects fresh lifecycle, dispatch and reconcile work while draining the active operation", async () => {
    const fixture = await createLifecycleFixture({ capabilitiesDelayMs: 200 });
    const close = closeFixture(cleanup, fixture);
    const authority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: { ...fixture.rootAuthority, sourcePaths: ["src/leader"], actions: [...fixture.rootAuthority.actions], budget: { turns: 1 } },
      commandId: "issue-354:close:authority",
    });
    const activeActionId = "issue-354:close:active";
    const active = fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: activeActionId,
      operation: "spawn",
      authorityId: authority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: { model: "fake-reviewer-v1", modelFamily: "fake", prompt: "close fence active operation", cwd: "src/leader" },
      commandId: "issue-354:close:active:dispatch",
    });
    await new Promise<void>((resolve) => setImmediate(resolve));
    const before = durableSnapshot(fixture.databasePath, fixture.runId);
    const closing = fixture.fabric.close();
    const checkpoint = await writeLifecycleCheckpoint(fixture, { agentId: "leader", nextAction: "close fence" });
    const freshLifecycle = fixture.leader.requestLifecycle({
      action: "completion-ready",
      agentId: "leader",
      taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision,
      checkpoint,
      commandId: "issue-354:close:fresh:lifecycle",
    });
    const freshDispatch = fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "issue-354:close:fresh:dispatch",
      operation: "wakeup",
      payload: {},
      commandId: "issue-354:close:fresh:dispatch:command",
    });
    const freshReconcile = fixture.chair.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId: activeActionId,
      commandId: "issue-354:close:fresh:reconcile",
    });
    const barrierArm = fixture.leader.requestLifecycle({
      action: "completion-ready",
      agentId: "leader",
      taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision,
      checkpoint,
      commandId: "issue-354:close:fresh:barrier-arm",
    });
    await expect(freshLifecycle).rejects.toMatchObject({ code: "LIFECYCLE_PRECONDITION_FAILED" });
    await expect(freshDispatch).rejects.toMatchObject({ code: "LIFECYCLE_PRECONDITION_FAILED" });
    await expect(freshReconcile).rejects.toMatchObject({ code: "LIFECYCLE_PRECONDITION_FAILED" });
    await expect(barrierArm).rejects.toMatchObject({ code: "LIFECYCLE_PRECONDITION_FAILED" });
    await expect(active).rejects.toMatchObject({ code: "LIFECYCLE_PRECONDITION_FAILED" });
    await closing;
    expect(durableSnapshot(fixture.databasePath, fixture.runId)).toEqual(before);
    expect(actionSnapshot(fixture.databasePath, { runId: fixture.runId, adapterId: "fake-lifecycle", actionId: "issue-354:close:fresh:dispatch" })).toBeUndefined();
    expect(await readFakeJournal(fixture.providerJournalPath)).toMatchObject({ actions: {} });
    await close();
  });

  // A lifecycle rotation continuation lives in #ownedProviderActions under the lifecycle-prefixed
  // key (fabric.ts:9248); the dead #lifecycleProviderActions map (fabric.ts:912) is never written.
  // close() (fabric.ts:1461-1471) drains #ownedProviderActions, so it must not resolve until the
  // in-flight continuation settles. We block the continuation at the replacement-spawn barrier,
  // then prove close() resolves only AFTER the barrier is released and the continuation drains.
  // (The map-level invariant that the drain flows through the owned map and #lifecycleProviderActions
  // stays empty is an in-process detail; from the retained daemon it is observable only as this
  // ordering, and is the S2 equivalence review's static concern per the adjudication.)
  it("CLOSE-2 drains a continuation through the owned map before close resolves", async () => {
    const fixture = await createLifecycleFixture({ retainedAgents: true, spawnBarrier: true, spawnDelayMs: 10 });
    const close = closeFixture(cleanup, fixture);
    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader", inFlightChildren: ["child"], openWork: ["leader-task"], nextAction: "hold continuation for close",
    });
    await fixture.leader.requestLifecycle({
      action: "rotate", agentId: "leader", taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision, checkpoint, commandId: "issue-354:close2:rotate",
    });
    // The continuation is now registered in the owned map and blocked at the replacement spawn.
    await fixture.providerSpawnBarrier!.waitUntilEntered();
    const order: string[] = [];
    const closing = fixture.fabric.close().then(() => order.push("closed"));
    // While the continuation is blocked, close() must not resolve (it awaits the owned-map drain).
    await settle(300, "close() must not resolve while the continuation is blocked");
    expect(order).toEqual([]);
    order.push("released");
    await fixture.providerSpawnBarrier!.release();
    await closing;
    // close() resolved only after the blocked continuation was allowed to settle.
    expect(order).toEqual(["released", "closed"]);
    await close();
  });

  // TODO(chair): CLOSE-3 pins that a continuation still blocked on its PREDECESSOR await
  // (fabric.ts:9245) is suppressed by the #closing gate (fabric.ts:9246) — no replacement request,
  // effect, custody advance, or continuation-failed event once close begins. Driving this
  // deterministically requires holding the predecessor owned-promise (the caller action's
  // #ownedProviderActions entry) open across the close() call, which is only reachable with an
  // in-process Fabric handle. The retained daemon runs Fabric in a separate process, so the test
  // cannot hold that promise or observe the suppression without a new harness seam. CLOSE-2 already
  // pins the complementary past-gate drain; this predecessor-gate arm is left for the chair to wire
  // an in-process retained-continuation fixture. Kept .skip (never .fails) so it cannot green the base.
  // Harness note: needs in-process Fabric handle harness; see #362.
  it.skip("CLOSE-3 suppresses post-predecessor continuation work after closing", () => {
    // See TODO(chair) above: needs an in-process Fabric handle to hold the predecessor promise.
  });

  // CITE CLOSE-4: ambiguous-provider-action.acceptance.test.ts:1954-2091 covers queued-vs-claimed close.
  // CITE CRASH/DUR: existing crash-after-provider-acceptance, ambiguous-provider-action, and
  // lifecycle-checkpoint acceptance tests remain the oracles; no duplicate bodies are added here.
});

describe("S1 LC-INV: lifecycle-expected inverse owner fence", () => {
  // The lifecycle-expected fence at #requestLifecycle (fabric.ts:5607-5619) looks up the custody
  // row by command_id and asserts the referenced action is owner "lifecycle". A row can never
  // classify plain "generic" AND carry a lifecycle custody: the classifier derives "lifecycle"
  // from that very custody row (provider-action-owner.ts:90-93). The only non-lifecycle owner a
  // custody-backed action can present is "integrity_failed" (a crossed lifecycle+provider_agent
  // custody, owner.ts:213). We drive that crossed row into the rotate fence and pin that it throws
  // ProviderActionOwnerError with ZERO adapter effect (no replacement spawn, journal untouched).
  it("LCI-1 fences a crossed row at the lifecycle-expected rotate boundary with zero effect", async () => {
    const fixture = await createLifecycleFixture();
    const close = closeFixture(cleanup, fixture);
    const commandId = "issue-354:lci-1:rotate";
    const actionId = `${commandId}:spawn`;
    const database = new Database(fixture.databasePath);
    const ref = seedProviderAction(database, { runId: fixture.runId, actionId, operation: "spawn", payload: {} });
    const projectSessionId = database.prepare("SELECT project_session_id FROM runs WHERE run_id=?").pluck().get(fixture.runId);
    if (typeof projectSessionId !== "string") throw new Error("lci-1 project session is unavailable");
    // Custody row keyed to the rotate command so the fence resolves this action...
    database.prepare(`
      INSERT INTO lifecycle_rotation_custodies(
        project_session_id,run_id,agent_id,custody_id,command_id,
        provider_action_adapter_id,provider_action_id,replacement_contract_digest,
        staged_capability_hash,target_principal_generation
      ) VALUES (?,?,?,?,?,?,?,?,?,?)
    `).run(projectSessionId, fixture.runId, "leader", `lci-1:${actionId}`, commandId, ref.adapterId, ref.actionId, `sha256:${"3".repeat(64)}`, "fixture-capability", 1);
    // ...and a provider_agent custody crosses it, so the action classifies integrity_failed.
    bindProviderAgentOwner(database, ref);
    database.close();
    const before = durableSnapshot(fixture.databasePath, fixture.runId, ref);
    const checkpoint = await writeLifecycleCheckpoint(fixture, { agentId: "leader", nextAction: "rotate" });
    await expect(fixture.leader.requestLifecycle({
      action: "rotate",
      agentId: "leader",
      taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision,
      checkpoint,
      commandId,
    })).rejects.toMatchObject({
      name: "ProviderActionOwnerError",
      expectedOwner: "lifecycle",
      actualOwner: "integrity_failed",
    });
    // Fence threw before any adapter interaction: no effect on the row, nothing in the journal.
    expect(durableSnapshot(fixture.databasePath, fixture.runId, ref)).toEqual(before);
    expect(actionSnapshot(fixture.databasePath, ref)).toMatchObject({ status: "prepared", effect_count: 0 });
    const journal = await readFakeJournal(fixture.providerJournalPath);
    expect((journal.actions as Record<string, unknown>)[actionId]).toBeUndefined();
    await close();
  });

  // The continuation-site owner-error SWALLOW (fabric.ts:9250-9257 — a ProviderActionOwnerError
  // from #continueLifecycleRotation returns WITHOUT a lifecycle-continuation-failed event, the
  // inverse of the E3-3 non-owner failure that DOES emit one) IS deterministically drivable: it is
  // not a tight in-process race, it is the same spawnBarrier + shared-SQLite corruptOwner technique
  // as CLOSE-2 and REC-2/E2R-1's crossed blocks. #continueLifecycleRotation asserts owner "lifecycle"
  // immediately before dispatching the replacement spawn (fabric.ts:9353-9357) and again immediately
  // after the spawn adapter call returns (fabric.ts:9394-9398, ahead of #persistProviderAction's own
  // assert at fabric.ts:10563). Holding the continuation at the replacement-spawn barrier and
  // crossing the row's owner while it is held lands the corruption strictly between those two
  // pre/post-spawn asserts, so the post-spawn assert throws ProviderActionOwnerError, which
  // propagates untouched through #completeAdapterOperation's rethrow (fabric.ts:~7963) to the
  // #scheduleLifecycleContinuation .catch (fabric.ts:9250) and is swallowed there. Note the
  // deliberate asymmetry for S2: the startup recovery loop RE-THROWS owner errors instead
  // (fabric.ts:9192), so only this live continuation site swallows them silently.
  it("LCI-2 swallows a continuation owner-error crossed during the replacement spawn barrier", async () => {
    const fixture = await createLifecycleFixture({ retainedAgents: true, spawnBarrier: true, spawnDelayMs: 10 });
    const close = closeFixture(cleanup, fixture);
    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader", inFlightChildren: ["child"], openWork: ["leader-task"], nextAction: "hold continuation for owner-error swallow",
    });
    const commandId = "issue-354:lci-2:swallow";
    const actionId = `${commandId}:spawn`;
    await expect(fixture.leader.requestLifecycle({
      action: "rotate", agentId: "leader", taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision, checkpoint, commandId,
    })).resolves.toMatchObject({ kind: "accepted-suspended" });
    // The continuation is now registered in the owned map and blocked at the replacement spawn,
    // AFTER its pre-spawn owner asserts (fabric.ts:9238, 9299, 9336, 9357) have already passed.
    await fixture.providerSpawnBarrier!.waitUntilEntered();
    const ref = { runId: fixture.runId, adapterId: "fake-lifecycle", actionId };
    expect(actionSnapshot(fixture.databasePath, ref)).toMatchObject({ status: "dispatched", execution_count: 1, effect_count: 0 });
    // Cross the continuation's owner from the test process while it is blocked, using the same
    // shared-SQLite corruptOwner technique as E4S-1/REC-2: a foreign finding_capacity_reservation_
    // digest fails owner.ts's reservation lookup unconditionally, so the row classifies
    // "integrity_failed" regardless of its (valid) lifecycle/provider_agent custody pair. The
    // custody tables themselves are append-only (immutable by trigger), so this is the only
    // available crossing lever once the continuation's own custody rows already exist.
    const crossing = new Database(fixture.databasePath);
    corruptOwner(crossing, ref);
    crossing.close();
    await fixture.providerSpawnBarrier!.release();
    // close() drains #ownedProviderActions, so it does not resolve until the continuation settles
    // through its post-spawn owner assert, which now throws ProviderActionOwnerError and is
    // swallowed at fabric.ts:9250 (never rethrown, no event).
    await close();
    const database = new Database(fixture.databasePath, { readonly: true });
    expect(database.prepare(`
      SELECT COUNT(*) AS count FROM events WHERE run_id=? AND type='lifecycle-continuation-failed'
    `).get(fixture.runId)).toEqual({ count: 0 });
    database.close();
    // No adapter effect landed for the crossed continuation: the row is stuck at dispatched, never
    // advanced to terminal/effect_count=1 by #persistProviderAction, which the corrupted owner check
    // pre-empts.
    expect(actionSnapshot(fixture.databasePath, ref)).toMatchObject({ status: "dispatched", execution_count: 1, effect_count: 0 });
  });
});
