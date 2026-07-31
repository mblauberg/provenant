import { join } from "node:path";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { connectFabricDaemon } from "../../../src/index.ts";
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

describe("S2a guards: lifecycle continuations get their own map and drain", () => {
  // KEY DISJOINTNESS. Before S2a, a lifecycle continuation and a generic deferred (ephemeral
  // answer-bearing spawn) action ride the SAME #ownedProviderActions map (fabric.ts:910), keyed
  // respectively `lifecycle\0runId\0agentId\0custodyId` (fabric.ts:9239) and
  // `runId\0adapterId\0actionId` (fabric.ts:1503-1505, #providerActionOwnershipKey). After S2a,
  // the lifecycle continuation moves to its own #lifecycleContinuations map. Either way, the two
  // key schemes must never alias: this drives BOTH kinds of action concurrently (sharing the same
  // fake-provider spawn barrier) and proves each dedupe guard (fabric.ts:1519 generic /
  // fabric.ts:9240 lifecycle) only ever sees its own action, and each settles with exactly one
  // effect, distinctly. This is the durable-row / effect-count observation the S1 E3-1 author
  // preferred over comparing key literals (see issue-354-characterisation-lifecycle.acceptance.test.ts:297-303).
  it("KEY-1 a concurrent generic ephemeral action and a lifecycle continuation settle independently without key collision", async () => {
    const fixture = await createLifecycleFixture({ retainedAgents: true, spawnBarrier: true, spawnDelayMs: 10 });
    const close = closeFixture(cleanup, fixture);

    // The retained fixture's exposed `fixture.chair` is deliberately narrowed to the operations
    // existing S1 tests needed (lifecycle-testkit.ts:684-697) and omits delegateAuthority.
    // Reconnect as chair with the full FabricClient over the same daemon socket the fixture
    // already started (lifecycle-testkit.ts:471), the same way the fixture's own internal
    // `remoteChair` does (lifecycle-testkit.ts:596-598), so we can drive a real ephemeral
    // answer-bearing spawn (the only path that ever populates the standard-key side of the map,
    // fabric.ts:6112-6128) alongside the retained lifecycle rotation.
    const fullChair = await connectFabricDaemon({
      socketPath: join(fixture.directory, "runtime", "fabric.sock"),
      capability: fixture.capabilities.chair,
    });
    cleanup.push(async () => {
      await fullChair.close();
    });

    const reviewAuthority = await fullChair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1 },
      },
      commandId: "issue-354:s2a:key:authority",
    });
    const genericActionId = "issue-354:s2a:key:generic-spawn";
    const genericSpawn = fullChair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: genericActionId,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: { model: "fake-reviewer-v1", modelFamily: "fake", prompt: "S2a key-disjointness probe", cwd: "leader" },
      commandId: "issue-354:s2a:key:generic-spawn:dispatch",
    });

    const checkpoint = await writeLifecycleCheckpoint(fixture, {
      agentId: "leader", inFlightChildren: ["child"], openWork: ["leader-task"], nextAction: "key disjointness probe",
    });
    const commandId = "issue-354:s2a:key:rotate";
    const replacementActionId = `${commandId}:spawn`;
    await expect(fixture.leader.requestLifecycle({
      action: "rotate", agentId: "leader", taskId: fixture.leaderTask.taskId,
      taskRevision: fixture.leaderTask.revision, checkpoint, commandId,
    })).resolves.toMatchObject({ kind: "accepted-suspended" });

    // Both actions are now admitted and blocked at the shared fake-provider spawn barrier: the
    // generic ephemeral spawn in the deferred-pump map, the lifecycle replacement spawn in the
    // continuation map. Neither's dedupe/admission guard mistook the other for itself.
    await eventually(() => {
      expect(actionSnapshot(fixture.databasePath, { runId: fixture.runId, adapterId: "fake-lifecycle", actionId: genericActionId }))
        .toMatchObject({ status: "dispatched", execution_count: 1, effect_count: 0 });
      expect(actionSnapshot(fixture.databasePath, { runId: fixture.runId, adapterId: "fake-lifecycle", actionId: replacementActionId }))
        .toMatchObject({ status: "dispatched", execution_count: 1, effect_count: 0 });
    });
    await fixture.providerSpawnBarrier!.release();
    await genericSpawn;

    await eventually(() => {
      expect(actionSnapshot(fixture.databasePath, { runId: fixture.runId, adapterId: "fake-lifecycle", actionId: genericActionId }))
        .toMatchObject({ status: "terminal", execution_count: 1, effect_count: 1 });
      expect(actionSnapshot(fixture.databasePath, { runId: fixture.runId, adapterId: "fake-lifecycle", actionId: replacementActionId }))
        .toMatchObject({ status: "terminal", execution_count: 1, effect_count: 1 });
    });
    const journal = await readFakeJournal(fixture.providerJournalPath);
    const actions = journal.actions as Record<string, { executionCount: number; effectCount: number }>;
    expect(actions[genericActionId]).toMatchObject({ executionCount: 1, effectCount: 1 });
    expect(actions[replacementActionId]).toMatchObject({ executionCount: 1, effectCount: 1 });

    const database = new Database(fixture.databasePath, { readonly: true });
    const custody = database.prepare(`
      SELECT provider_action_id FROM lifecycle_rotation_custodies WHERE run_id=? AND command_id=?
    `).get(fixture.runId, commandId) as Record<string, string>;
    database.close();
    expect(custody.provider_action_id).toBe(replacementActionId);
    expect(custody.provider_action_id).not.toBe(genericActionId);

    await close();
  });

  // TODO(chair): the two remaining invariants named in the S2a brief -- PREDECESSOR-AWAIT ORDERING
  // (continuation.ts `schedule`, "a still-pending standard-key generic predecessor prevents the
  // lifecycle continuation's provider effect until the predecessor settles") and CLOSE-3 ("close()
  // started while a lifecycle continuation is waiting on its predecessor drains correctly") CANNOT
  // be driven live from any *well-formed* call sequence in the current implementation, but the
  // predecessor lookup IS reachable adversarially: it is keyed by a non-injective NUL-delimited
  // join of (runId, adapterId, callerActionId) (issue #362), so a crafted adapterId/actionId pair
  // whose NUL-delimited concatenation collides with another triple's can make an unrelated
  // #ownedProviderActions entry read back as this continuation's predecessor. The read and its
  // ProviderActionOwnerError swallow are deliberately preserved rather than removed as dead code.
  // This is a narrower finding than the existing CLOSE-3 skip
  // (issue-354-characterisation-close-inverse.acceptance.test.ts:122-133), which attributes the
  // gap to the retained daemon running Fabric in a separate process.
  //
  // The predecessor lookup (continuation.ts `schedule`, via the injected getGenericPredecessor
  // port) reads #ownedProviderActions keyed by `runId\0adapterId\0callerActionId`, where
  // callerActionId is always the action_id of the CALLING agent's currently-active
  // provider_session_turn_leases row (fabric.ts:8090,8111 -- the "active lifecycle caller turn"
  // query, an INNER JOIN so acceptRotation cannot proceed without one). Turn leases are inserted in
  // exactly one place, `prepareTurnAction` (provider-session-coordinator.ts:403), invoked only for
  // a "send_turn" dispatch (fabric.ts:6183-6208). #ownedProviderActions standard-key entries,
  // meanwhile, are written in exactly one place, #enqueueDeferredProviderAction
  // (fabric.ts:1507-1526), invoked only from the ephemeral, task-bound, answer-bearing "spawn" path
  // (fabric.ts:6112-6128, the ONLY call site of `deferCompletion: true` in fabric.ts -- confirmed by
  // exhaustive grep). Under WELL-FORMED ids, a single (runId, adapterId, actionId) triple is
  // admitted under exactly one operation, so an action can never be both the caller's active turn
  // lease AND a live #ownedProviderActions entry: "send_turn" actions (the only source of
  // callerActionId) are dispatched through the direct, non-deferred path (fabric.ts:6272) and never
  // touch #enqueueDeferredProviderAction at all. KEY-1 above independently confirms the generic
  // (ephemeral-spawn) and lifecycle (send_turn-triggered rotate) action families are simultaneously
  // drivable but never share an actionId under well-formed ids -- but the join key itself is NOT
  // collision-resistant (issue #362): it does not length-prefix or escape its NUL-delimited
  // segments, so a generic actionId and a distinct (adapterId, callerActionId) pair can be crafted
  // to serialise to the identical key string, making the "never share an actionId" disjunction
  // adversarially defeatable without sharing an actionId at all. This holds regardless of process
  // topology (verified against both the real daemon subprocess used by KEY-1 above, and,
  // separately, the retained fixture's in-process `fault` variant, which also cannot manufacture
  // the well-formed combination -- and which additionally does not forward the spawn-barrier
  // environment to its adapter, a second, independent reason it cannot drive this scenario via
  // well-formed ids). Kept .skip (never .fails) so it cannot green the base; do not delete these
  // without either wiring a new caller shape that lets a send_turn's own action ride the
  // deferred/#ownedProviderActions path (or a collision-key harness for issue #362), or downgrading
  // the predecessor-await claim in the S2 plan's preservation checklist (docs section 4,
  // "Predecessor ordering" row) to reflect that it is reachable only adversarially, not via any
  // well-formed externally reachable call.
  // Harness note: needs in-process Fabric handle harness; see #362.
  it.skip("PRED-1 a still-pending standard-key generic predecessor blocks the continuation's provider effect", () => {
    // See TODO(chair) above.
  });

  // Harness note: needs in-process Fabric handle harness; see #362.
  it.skip("PRED-2 close() started while a continuation awaits a pending predecessor drains without early return (CLOSE-3)", () => {
    // See TODO(chair) above. This case additionally requires PRED-1's precondition (a live
    // standard-key predecessor at the moment #scheduleLifecycleContinuation runs), so it inherits
    // the same unreachability.
  });
});
