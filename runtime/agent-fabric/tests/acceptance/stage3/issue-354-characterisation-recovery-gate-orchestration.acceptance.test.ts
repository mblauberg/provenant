import { readFile, readdir, writeFile } from "node:fs/promises";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import {
  ProviderActionAdmissionCoordinator,
  type ProviderActionInsert,
} from "../../../src/application/provider-action-admission.ts";
import { canonicaliseProviderActionDispatchRequest } from "../../../src/application/provider-action-dispatch-request.ts";
import { inspectFabricDatabase } from "../../../src/core/migrations.ts";
import {
  createLifecycleFixture,
  reopenLifecycleFabric,
} from "../../support/lifecycle-testkit.ts";
import { eventually } from "../../shared/deadline-wait.ts";
import {
  actionSnapshot,
  bindProviderAgentOwner,
  closeFixture,
  corruptOwner,
  readFakeJournal,
  seedProviderAction,
} from "../../support/w354-characterisation-testkit.ts";

const cleanup: Array<() => Promise<void>> = [];

afterEach(async () => {
  await Promise.all(cleanup.splice(0).map((close) => close()));
});

const emptyFindingSet = "sha256:58afae1b74b0f7295f280a34196c2e092e4040016e64927e132f99356b48b7a2";

function insertCertifyingRoute(database: Database.Database, input: Readonly<{
  runId: string;
  adapterId: string;
  actionId: string;
  taskId: string;
  ownerDigest: string;
  reservationDigest: string;
}>): void {
  database.prepare(`
    INSERT OR IGNORE INTO review_finding_sets(
      finding_set_digest,finding_count,page_count,canonical_byte_length,created_at
    ) VALUES (?,0,0,47,1)
  `).run(emptyFindingSet);
  database.prepare(`
    INSERT INTO review_finding_capacity_reservations(
      adapter_id,action_id,run_id,target_generation,slot,owner_digest,
      finding_window_mode,prior_open_finding_set_digest,maximum_new_findings,
      maximum_new_finding_bytes,reservation_digest,state,created_at,updated_at
    ) VALUES (?,?,?,1,'native',?,'normal',?,32,65536,?,'attached',1,1)
  `).run(
    input.adapterId,
    input.actionId,
    input.runId,
    input.ownerDigest,
    emptyFindingSet,
    input.reservationDigest,
  );
  database.prepare(`
    UPDATE provider_actions SET finding_capacity_reservation_digest=?
     WHERE run_id=? AND adapter_id=? AND action_id=?
  `).run(input.reservationDigest, input.runId, input.adapterId, input.actionId);

  const routeColumns = database.prepare(`
    SELECT name,"notnull" FROM pragma_table_info('provider_action_routes') ORDER BY cid
  `).all() as Array<{ name: string; notnull: number }>;
  const nullable = new Set([
    "requested_effort", "resolved_effort_value", "bundle_search_index_digest",
    "risk_read_map_digest", "mandatory_read_set_digest",
  ]);
  const integerColumns = new Set([
    "target_generation", "slot_head_generation", "attempt_generation", "reviewed_artifact_revision",
    "chair_binding_generation", "capability_snapshot_generation", "effective_configuration_revision",
    "route_policy_revision", "harness_revision", "context_policy_revision",
    "discovery_surface_evidence_revision", "created_at",
  ]);
  const certifyingStrings = new Set([
    "reviewed_artifact_id", "publication_lineage_digest", "bundle_digest",
    "manifest_root_digest", "coverage_digest", "profile_digest",
    "profile_schema_digest", "final_prompt_digest",
  ]);
  const values = routeColumns.map(({ name, notnull }) => {
    if (name === "adapter_id" || name === "requested_adapter_id" || name === "resolved_adapter_id") return input.adapterId;
    if (name === "action_id") return input.actionId;
    if (name === "run_id") return input.runId;
    if (name === "task_id") return input.taskId;
    if (name === "certifying_review") return 1;
    if (name === "target_generation" || name === "slot_head_generation" || name === "attempt_generation") return 1;
    if (name === "slot") return "native";
    if (name === "resolved_effort_kind") return "inapplicable";
    if (name.endsWith("_json")) return "{}";
    if (integerColumns.has(name)) return 1;
    if (certifyingStrings.has(name)) return `${name}:fixture`;
    if (nullable.has(name) || notnull === 0) return null;
    return `${name}:fixture`;
  });
  database.prepare(`
    INSERT INTO provider_action_routes(${routeColumns.map(({ name }) => `"${name}"`).join(",")})
    VALUES (${routeColumns.map(() => "?").join(",")})
  `).run(...values);
}

function admitDualBoundCertifyingAction(
  fixture: Awaited<ReturnType<typeof createLifecycleFixture>>,
  actionId: string,
  authorityId: string,
): { runId: string; adapterId: string; actionId: string } {
  const database = new Database(fixture.databasePath);
  database.pragma("foreign_keys = OFF");
  const coordinator = new ProviderActionAdmissionCoordinator({
    database,
    clock: () => fixture.clock.now().getTime(),
  });
  const project = database.prepare(`
    SELECT session.project_id FROM runs run
    JOIN project_sessions session ON session.project_session_id=run.project_session_id
    WHERE run.run_id=?
  `).pluck().get(fixture.runId);
  if (typeof project !== "string") throw new Error("certifying fixture project is unavailable");
  const ticket = coordinator.preflight({
    actionRef: { adapterId: "fake-lifecycle", actionId },
    scope: { kind: "run-action", runId: fixture.runId },
    principal: { kind: "integration", integrationId: "review-evidence-daemon", projectId: project },
    canonicalInput: { schemaVersion: 1, kind: "issue-354-certifying-budget", actionId },
  });
  const ownerDigest = database.prepare(`
    SELECT owner_digest FROM provider_action_pair_preflights WHERE adapter_id=? AND action_id=?
  `).pluck().get("fake-lifecycle", actionId);
  if (typeof ownerDigest !== "string") throw new Error("certifying fixture owner digest is unavailable");
  const reservationDigest = `sha256:${"9".repeat(64)}`;
  const action: ProviderActionInsert = {
    runId: fixture.runId,
    actionId,
    adapterId: "fake-lifecycle",
    operation: "spawn",
    targetAgentId: null,
    providerSessionGeneration: null,
    turnLeaseGeneration: null,
    identityHash: "a".repeat(64),
    payloadHash: "b".repeat(64),
    payloadJson: '{"maxTurns":1,"model":"fake-reviewer-v1","modelFamily":"fake","prompt":"certifying budget"}',
    status: "ambiguous",
    historyJson: '["prepared","dispatched","accepted","ambiguous"]',
    executionCount: 1,
    effectCount: 1,
    taskId: fixture.leaderTask.taskId,
    budgetAuthorityId: authorityId,
    budgetReservationJson: '{"turns":1}',
    budgetState: "reserved",
    budgetStartedAt: fixture.clock.now().getTime(),
    updatedAt: fixture.clock.now().getTime(),
  };
  database.transaction(() => coordinator.admitUnroutedInCurrentTransaction(
    ticket,
    action,
    "certifying_review",
    () => insertCertifyingRoute(database, {
      runId: fixture.runId,
      adapterId: "fake-lifecycle",
      actionId,
      taskId: fixture.leaderTask.taskId,
      ownerDigest,
      reservationDigest,
    }),
  ))();
  database.close();
  return { runId: fixture.runId, adapterId: "fake-lifecycle", actionId };
}

async function writeTerminalJournal(path: string, actionId: string, answer = "fake provider review complete"): Promise<void> {
  await writeFile(path, `${JSON.stringify({
    schemaVersion: 1,
    actions: {
      [actionId]: {
        actionId,
        payloadHash: "unused",
        status: "terminal",
        history: ["prepared", "dispatched", "accepted", "terminal"],
        executionCount: 1,
        effectCount: 1,
        idempotencyProven: true,
        result: { result: answer, resourceUsage: { turns: 1 } },
      },
    },
    sessions: {},
  }, null, 2)}\n`);
}

describe("S1 E4 and GATE: settlement ordering and certifying budget boundary", () => {
  it("E4S-1 asserts owner before terminal settlement changes action or ledger", async () => {
    const fixture = await createLifecycleFixture();
    const close = closeFixture(cleanup, fixture);
    let database = new Database(fixture.databasePath);
    const authorityId = database.prepare("SELECT authority_id FROM agents WHERE run_id=? AND agent_id='leader'").pluck().get(fixture.runId);
    if (typeof authorityId !== "string") throw new Error("settlement authority is unavailable");
    const ref = seedProviderAction(database, {
      runId: fixture.runId,
      actionId: "issue-354:E4S-1",
      operation: "spawn",
      payload: { maxTurns: 1, modelFamily: "fake", prompt: "settlement ordering", taskId: fixture.leaderTask.taskId },
      status: "dispatched",
      historyJson: '["prepared","dispatched"]',
      executionCount: 1,
      taskId: fixture.leaderTask.taskId,
      budgetAuthorityId: authorityId,
      budgetReservationJson: '{"turns":1}',
      budgetState: "reserved",
      budgetStartedAt: fixture.clock.now().getTime(),
    });
    const ledgerBefore = database.prepare("SELECT unit_key,reserved,consumed,usage_unknown FROM authority_budget WHERE authority_id=? ORDER BY unit_key").all(authorityId);
    corruptOwner(database, ref);
    database.close();
    const before = actionSnapshot(fixture.databasePath, ref);
    await expect(fixture.chair.reconcileProviderAction({
      adapterId: ref.adapterId,
      actionId: ref.actionId,
      commandId: "issue-354:E4S-1:reconcile",
    })).rejects.toMatchObject({ name: "ProviderActionOwnerError", actualOwner: "integrity_failed" });
    expect(actionSnapshot(fixture.databasePath, ref)).toMatchObject(before);
    const after = new Database(fixture.databasePath, { readonly: true });
    expect(after.prepare("SELECT unit_key,reserved,consumed,usage_unknown FROM authority_budget WHERE authority_id=? ORDER BY unit_key").all(authorityId)).toEqual(ledgerBefore);
    after.close();
    await close();
  });

  it.each([
    { name: "certifying binding", input: { certifyingReview: {} } },
    { name: "route request", input: { routeRequest: {} } },
  ])("GATE-A rejects $name at the closed generic boundary", ({ input }) => {
    expect(() => canonicaliseProviderActionDispatchRequest({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "issue-354:gate-a",
      operation: "wakeup",
      payload: {},
      commandId: "issue-354:gate-a:dispatch",
      ...input,
    } as never)).toThrowError(expect.objectContaining({ code: "PROTOCOL_INVALID" }));
  });

  it("GATE-B PINS LATENT BEHAVIOUR: certifying recovery settles its dual-bound budget", async () => {
    const fixture = await createLifecycleFixture();
    const closeOriginal = closeFixture(cleanup, fixture);
    const authorityId = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1 },
      },
      commandId: "issue-354:gate-b:authority",
    }).then((value) => value.authorityId);
    const ref = admitDualBoundCertifyingAction(fixture, "issue-354:gate-b", authorityId);
    await writeTerminalJournal(fixture.providerJournalPath, ref.actionId);
    await closeOriginal();
    const reopened = await reopenLifecycleFabric(fixture);
    cleanup.push(async () => await reopened.close());
    const first = await reopened.recoverStartupState();
    expect(first).toMatchObject({ actionsReconciled: 1, actionsQuarantined: 0 });
    const database = new Database(fixture.databasePath, { readonly: true });
    expect(database.prepare(`SELECT status,budget_state,budget_settlement_json FROM provider_actions WHERE run_id=? AND adapter_id=? AND action_id=?`).get(ref.runId, ref.adapterId, ref.actionId)).toMatchObject({
      status: "terminal",
      budget_state: "settled",
      budget_settlement_json: '{"turns":1}',
    });
    expect(database.prepare(`SELECT reserved,consumed FROM authority_budget WHERE authority_id=? AND unit_key='turns'`).get(authorityId)).toEqual({ reserved: 0, consumed: 1 });
    expect(database.prepare(`SELECT state FROM review_finding_capacity_reservations WHERE adapter_id=? AND action_id=?`).get(ref.adapterId, ref.actionId)).toEqual({ state: "attached" });
    expect(database.prepare(`SELECT COUNT(*) AS count FROM commands WHERE run_id=? AND command_id LIKE 'certifying-review-recovery:%'`).get(ref.runId)).toEqual({ count: 1 });
    expect(database.prepare(`SELECT COUNT(*) AS count FROM commands WHERE run_id=? AND command_id LIKE 'startup-recovery:%'`).get(ref.runId)).toEqual({ count: 0 });
    database.close();
    await expect(reopened.recoverStartupState()).resolves.toMatchObject({ actionsReconciled: 0, actionsQuarantined: 0 });
  });

  // Permanent by design: RESTATE decision; this GATE-B' characterisation remains skipped.
  it.skip("GATE-B' DECISION-GATE RESOLVED (restate): certifying settlement is owner-agnostic BY DESIGN", () => {
    // Decision gate (#354 S1->S2) RESOLVED 2026-07-21: verdict = RESTATE, not fix. Owner-agnostic budget
    // settlement is intended behaviour, not a latent bug. Settlement is ledger bookkeeping on a reservation
    // the authority already committed at admission: `budget_authority_id` IS the settlement authority, and
    // `assertProviderActionOwner` (fabric.ts:10563) already fences the row-owner precondition. Refusing to
    // settle a validly-bound row purely because its custody owner is `certifying_review` would STRAND the
    // reservation (reserved units never released) — that harms the reliability floor rather than protecting
    // it; budgets are ceilings and consuming an explicitly reserved ceiling is correct accounting regardless
    // of owner. The dangerous dual-bound combination is not producible by any in-repo writer (only the test
    // helper `admitDualBoundCertifyingAction`, impersonating the out-of-tree review-evidence daemon), so a
    // refusal branch would be a speculative guard on an unreachable path. GATE-B pins the real behaviour;
    // this stays a PERMANENT documented skip. Revisit ONLY if the external review-evidence daemon later
    // publishes a settled contract that demands custody-scoped settlement.
  });
});

describe("S1 REC: startup routing equivalence classes", () => {
  it("REC-1 routes generic, certifying, integrity_failed and specialized rows exactly once", async () => {
    const fixture = await createLifecycleFixture();
    const closeOriginal = closeFixture(cleanup, fixture);
    let database = new Database(fixture.databasePath);
    const authorityId = database.prepare("SELECT authority_id FROM agents WHERE run_id=? AND agent_id='leader'").pluck().get(fixture.runId);
    if (typeof authorityId !== "string") throw new Error("recovery authority is unavailable");
    const generic = seedProviderAction(database, { runId: fixture.runId, actionId: "issue-354:rec:generic", operation: "wakeup", payload: {} });
    database.close();
    const certifying = admitDualBoundCertifyingAction(fixture, "issue-354:rec:certifying", authorityId);
    database = new Database(fixture.databasePath);
    const integrity = seedProviderAction(database, { runId: fixture.runId, actionId: "issue-354:rec:integrity", operation: "wakeup", payload: {} });
    corruptOwner(database, integrity);
    const specialized = seedProviderAction(database, { runId: fixture.runId, actionId: "issue-354:rec:specialized", operation: "wakeup", payload: {} });
    // The specialized row is classified as lifecycle by its persisted custody and is deliberately
    // not given a lifecycle-recovery head, so startup routing leaves it untouched.
    database.close();
    const specializedDb = new Database(fixture.databasePath);
    const projectSessionId = specializedDb.prepare("SELECT project_session_id FROM runs WHERE run_id=?").pluck().get(fixture.runId);
    if (typeof projectSessionId !== "string") throw new Error("specialized project session is unavailable");
    specializedDb.prepare(`INSERT INTO lifecycle_rotation_custodies(project_session_id,run_id,agent_id,custody_id,provider_action_adapter_id,provider_action_id,replacement_contract_digest,staged_capability_hash,target_principal_generation) VALUES (?,?,?,?,?,?,?,?,?)`).run(projectSessionId, fixture.runId, "leader", "issue-354:rec:specialized:custody", specialized.adapterId, specialized.actionId, `sha256:${"3".repeat(64)}`, "fixture-capability", 1);
    specializedDb.close();
    await writeTerminalJournal(fixture.providerJournalPath, generic.actionId, "generic recovery");
    await writeTerminalJournal(fixture.providerJournalPath, certifying.actionId);
    await closeOriginal();
    const reopened = await reopenLifecycleFabric(fixture);
    cleanup.push(async () => await reopened.close());
    const first = await reopened.recoverStartupState();
    expect(first).toMatchObject({ actionsReconciled: 2, actionsQuarantined: 1 });
    const observed = new Database(fixture.databasePath, { readonly: true });
    expect(observed.prepare("SELECT status FROM provider_actions WHERE action_id=?").get(generic.actionId)).toEqual({ status: "terminal" });
    expect(observed.prepare("SELECT status FROM provider_actions WHERE action_id=?").get(certifying.actionId)).toEqual({ status: "terminal" });
    expect(observed.prepare("SELECT status FROM provider_actions WHERE action_id=?").get(integrity.actionId)).toEqual({ status: "quarantined" });
    expect(observed.prepare("SELECT status FROM provider_actions WHERE action_id=?").get(specialized.actionId)).toEqual({ status: "prepared" });
    expect(observed.prepare("SELECT COUNT(*) AS count FROM events WHERE run_id=? AND type='startup-provider-action-quarantined' AND json_extract(payload_json,'$.actionId')=?").get(fixture.runId, integrity.actionId)).toEqual({ count: 1 });
    observed.close();
    await expect(reopened.recoverStartupState()).resolves.toMatchObject({ actionsReconciled: 0, actionsQuarantined: 0 });
  });

  it("REC-2 quarantines a generic row that crosses owner during lookup", async () => {
    const fixture = await createLifecycleFixture();
    const closeOriginal = closeFixture(cleanup, fixture);
    const database = new Database(fixture.databasePath);
    const authorityId = database.prepare("SELECT authority_id FROM agents WHERE run_id=? AND agent_id='leader'").pluck().get(fixture.runId);
    if (typeof authorityId !== "string") throw new Error("drift authority is unavailable");
    const ref = seedProviderAction(database, {
      runId: fixture.runId,
      actionId: "issue-354:rec:drift",
      operation: "spawn",
      payload: { maxTurns: 1, modelFamily: "fake", prompt: "drift recovery", taskId: fixture.leaderTask.taskId, scenario: "ambiguous-review-concurrent-divergent" },
      status: "ambiguous",
      historyJson: '["prepared","dispatched","ambiguous"]',
      executionCount: 1,
      taskId: fixture.leaderTask.taskId,
      budgetAuthorityId: authorityId,
      budgetReservationJson: '{"turns":1}',
      budgetState: "reserved",
      budgetStartedAt: fixture.clock.now().getTime(),
    });
    database.close();
    // Write the journal directly (rather than via writeTerminalJournal) so the entry carries
    // scenario:"ambiguous-review-concurrent-divergent" -- the same scenario already declared on the
    // seeded row's payload above, which only reaches the adapter through the journal, not the
    // dispatch payload, for a pre-seeded ambiguous row. The fake adapter's lookup_action handler
    // special-cases this scenario: it delays its FIRST response by 100ms but records lookupCount=1
    // in the shared journal synchronously beforehand, giving a deterministic, pollable signal that
    // the request is in flight and the response is still pending.
    await writeFile(fixture.providerJournalPath, `${JSON.stringify({
      schemaVersion: 1,
      actions: {
        [ref.actionId]: {
          actionId: ref.actionId,
          payloadHash: "unused",
          status: "terminal",
          history: ["prepared", "dispatched", "accepted", "terminal"],
          executionCount: 1,
          effectCount: 1,
          idempotencyProven: true,
          result: { result: "recovered provider review", resourceUsage: { turns: 1 } },
          scenario: "ambiguous-review-concurrent-divergent",
        },
      },
      sessions: {},
    }, null, 2)}\n`);
    await closeOriginal();
    const reopened = await reopenLifecycleFabric(fixture);
    cleanup.push(async () => await reopened.close());
    const recovery = reopened.recoverStartupState();
    // Deterministic barrier (no sleep): poll the journal for lookupCount>=1 so the owner corruption
    // below lands inside the adapter round-trip -- after startup routing's initial "generic"
    // snapshot classified this row, and before #reconcileProviderAction's post-lookup persist
    // re-asserts ownership (the 100ms response delay above leaves ample margin either side).
    await eventually(async () => {
      const journal = await readFakeJournal(fixture.providerJournalPath);
      const actions = journal.actions as Record<string, { lookupCount?: number }>;
      expect(actions[ref.actionId]?.lookupCount).toBeGreaterThanOrEqual(1);
    });
    const crossed = new Database(fixture.databasePath);
    bindProviderAgentOwner(crossed, ref);
    crossed.close();
    await expect(recovery).resolves.toMatchObject({ actionsReconciled: 0, actionsQuarantined: 1 });
    const observed = new Database(fixture.databasePath, { readonly: true });
    expect(observed.prepare("SELECT status,effect_count FROM provider_actions WHERE run_id=? AND action_id=?").get(fixture.runId, ref.actionId)).toEqual({ status: "quarantined", effect_count: 0 });
    expect(observed.prepare("SELECT json_extract(payload_json,'$.owner') AS owner FROM events WHERE run_id=? AND type='startup-provider-action-quarantined' AND json_extract(payload_json,'$.actionId')=?").get(fixture.runId, ref.actionId)).toEqual({ owner: "provider_agent" });
    observed.close();
  });
});

describe("S1 ORCH: deferred pump FIFO and stale-head drop", () => {
  it("ORCH-1 preserves FIFO order across a blocked head and asserts every generic owner", async () => {
    const fixture = await createLifecycleFixture({ maximumConcurrentProviderTurns: 1, spawnDelayMs: 100 });
    const close = closeFixture(cleanup, fixture);
    const authority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: { ...fixture.rootAuthority, sourcePaths: ["src/leader"], actions: [...fixture.rootAuthority.actions], budget: { turns: 3 } },
      commandId: "issue-354:orch:fifo:authority",
    });
    const dispatch = (suffix: string) => fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: `issue-354:orch:fifo:${suffix}`,
      operation: "spawn",
      authorityId: authority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: { model: "fake-reviewer-v1", modelFamily: "fake", prompt: `FIFO ${suffix}`, cwd: "src/leader" },
      commandId: `issue-354:orch:fifo:${suffix}:dispatch`,
    });
    const firstPrepared = await dispatch("1");
    await eventually(() => expect(actionSnapshot(fixture.databasePath, { runId: fixture.runId, adapterId: "fake-lifecycle", actionId: "issue-354:orch:fifo:1" })).toMatchObject({ status: "dispatched" }));
    const prepared = [firstPrepared, ...(await Promise.all([dispatch("2"), dispatch("3")]))];
    expect(prepared).toEqual(expect.arrayContaining([
      expect.objectContaining({ actionId: "issue-354:orch:fifo:1", status: "prepared" }),
      expect.objectContaining({ actionId: "issue-354:orch:fifo:2", status: "prepared" }),
      expect.objectContaining({ actionId: "issue-354:orch:fifo:3", status: "prepared" }),
    ]));
    // What this line proves is that enqueuing 2 and 3 did not move the head
    // backwards. It must not also require the head to have stood still: the
    // fixture's spawnDelayMs is 100ms and the two dispatches above take real
    // time, so under load the head legitimately reaches `terminal` before this
    // executes. Asserting `dispatched` asserted that a transient state
    // persisted, which is why this failed intermittently in CI while passing in
    // isolation. Assert the invariant that actually holds -- the head never
    // returns to `prepared` -- and leave FIFO ordering to the journal check
    // below, which is what genuinely evidences it.
    expect(["dispatched", "terminal"]).toContain(
      actionSnapshot(fixture.databasePath, { runId: fixture.runId, adapterId: "fake-lifecycle", actionId: "issue-354:orch:fifo:1" }).status,
    );
    await eventually(async () => {
      const database = new Database(fixture.databasePath, { readonly: true });
      try {
        expect(database.prepare("SELECT COUNT(*) AS count FROM provider_actions WHERE run_id=? AND action_id LIKE 'issue-354:orch:fifo:%' AND status='terminal'").get(fixture.runId)).toEqual({ count: 3 });
      } finally {
        database.close();
      }
    });
    const journal = await readFakeJournal(fixture.providerJournalPath);
    const keys = Object.keys(journal.actions as Record<string, unknown>).filter((key) => key.startsWith("issue-354:orch:fifo:"));
    expect(keys).toEqual([
      "issue-354:orch:fifo:1",
      "issue-354:orch:fifo:2",
      "issue-354:orch:fifo:3",
    ]);
    await close();
  });

  it("ORCH-2 drops a stale prepared head and continues to the next item", async () => {
    const fixture = await createLifecycleFixture({ maximumConcurrentProviderTurns: 1 });
    const close = closeFixture(cleanup, fixture);
    const database = new Database(fixture.databasePath);
    const authorityId = database.prepare("SELECT authority_id FROM agents WHERE run_id=? AND agent_id='leader'").pluck().get(fixture.runId);
    if (typeof authorityId !== "string") throw new Error("pump authority is unavailable");
    const sentinel = seedProviderAction(database, {
      runId: fixture.runId,
      actionId: "issue-354:orch:sentinel",
      operation: "spawn",
      payload: { maxTurns: 1, modelFamily: "fake", prompt: "sentinel", taskId: fixture.leaderTask.taskId },
      status: "dispatched",
      historyJson: '["prepared","dispatched"]',
      executionCount: 1,
      taskId: fixture.leaderTask.taskId,
      budgetAuthorityId: authorityId,
      budgetReservationJson: '{"turns":1}',
      budgetState: "reserved",
      budgetStartedAt: fixture.clock.now().getTime(),
    });
    const head = seedProviderAction(database, { runId: fixture.runId, actionId: "issue-354:orch:stale-head", operation: "spawn", payload: { maxTurns: 1, modelFamily: "fake", prompt: "stale", taskId: fixture.leaderTask.taskId } });
    const next = seedProviderAction(database, { runId: fixture.runId, actionId: "issue-354:orch:next", operation: "spawn", payload: { maxTurns: 1, modelFamily: "fake", prompt: "next", taskId: fixture.leaderTask.taskId } });
    database.close();
    await expect(fixture.chair.reconcileProviderAction({ adapterId: head.adapterId, actionId: head.actionId, commandId: "issue-354:orch:stale-head:enqueue" })).resolves.toMatchObject({ status: "prepared" });
    await expect(fixture.chair.reconcileProviderAction({ adapterId: next.adapterId, actionId: next.actionId, commandId: "issue-354:orch:next:enqueue" })).resolves.toMatchObject({ status: "prepared" });
    const stale = new Database(fixture.databasePath);
    stale.prepare("UPDATE provider_actions SET status='terminal',history_json='[\"prepared\",\"terminal\"]',execution_count=1,effect_count=0,budget_state=NULL,budget_reservation_json=NULL WHERE run_id=? AND action_id=?").run(fixture.runId, head.actionId);
    stale.prepare("UPDATE provider_actions SET status='terminal',history_json='[\"prepared\",\"terminal\"]',execution_count=1,effect_count=0,budget_state='settled',budget_settlement_json='{\"turns\":1}',budget_reservation_json='{\"turns\":1}' WHERE run_id=? AND action_id=?").run(fixture.runId, sentinel.actionId);
    stale.prepare("UPDATE authority_budget SET provider_reserved=0,provider_consumed=1 WHERE authority_id=? AND unit_key='turns'").run(authorityId);
    stale.close();
    await expect(fixture.chair.reconcileProviderAction({ adapterId: sentinel.adapterId, actionId: sentinel.actionId, commandId: "issue-354:orch:sentinel:settle" })).resolves.toMatchObject({ status: "terminal" });
    await eventually(() => expect(actionSnapshot(fixture.databasePath, next)).toMatchObject({ status: "terminal", effect_count: 1 }));
    expect(actionSnapshot(fixture.databasePath, head)).toMatchObject({ status: "terminal", effect_count: 0 });
    const journal = await readFakeJournal(fixture.providerJournalPath);
    expect((journal.actions as Record<string, unknown>)[head.actionId]).toBeUndefined();
    await close();
  });
});

describe("S1 SCHEMA: cutover preserves the database and suppresses recovery", () => {
  it("SCHEMA-1 rejects a mismatched fingerprint with preserved=true and does not recover actions", async () => {
    const fixture = await createLifecycleFixture();
    const close = closeFixture(cleanup, fixture);
    const database = new Database(fixture.databasePath);
    const ref = seedProviderAction(database, { runId: fixture.runId, actionId: "issue-354:schema:pending", status: "prepared" });
    database.prepare("UPDATE fabric_schema SET baseline_sha256=?,catalog_sha256=? WHERE singleton=1").run("a".repeat(64), "b".repeat(64));
    database.close();
    const names = (await readdir(fixture.directory, { withFileTypes: true }))
      .filter((entry) => entry.isFile())
      .map((entry) => entry.name);
    const before = new Map(await Promise.all(names.map(async (name) => [name, await readFile(`${fixture.directory}/${name}`)] as const)));
    expect(() => inspectFabricDatabase(fixture.databasePath)).toThrowError(expect.objectContaining({ code: "SCHEMA_CUTOVER_REQUIRED", preserved: true }));
    await expect(reopenLifecycleFabric(fixture)).rejects.toMatchObject({ code: "SCHEMA_CUTOVER_REQUIRED", preserved: true });
    for (const [name, bytes] of before) {
      expect((await readFile(`${fixture.directory}/${name}`)).equals(bytes)).toBe(true);
    }
    expect(actionSnapshot(fixture.databasePath, ref)).toMatchObject({ status: "prepared", effect_count: 0 });
    await close();
  });
});
