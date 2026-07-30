import { readFile, rm } from "node:fs/promises";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { digest } from "../../src/persistence/row-codec.ts";
import { waitUntil } from "../shared/deadline-wait.ts";
import { createLifecycleFixture, reopenLifecycleFabric } from "../support/lifecycle-testkit.ts";

const cleanup: Array<() => Promise<void>> = [];

afterEach(async () => {
  await Promise.all(cleanup.splice(0).map((close) => close()));
});

async function waitForProviderAction(
  fixture: Awaited<ReturnType<typeof createLifecycleFixture>>,
  actionId: string,
): Promise<string> {
  const timeoutMs = 5_000;
  const description = `Provider action ${actionId} settlement`;
  try {
    return await waitUntil(async () => {
      const action = await fixture.chair.getProviderAction({
        adapterId: "fake-lifecycle",
        actionId,
        expectedActionKind: "non-review",
      });
      return ["ambiguous", "terminal", "quarantined"].includes(action.status) ? action.status : "";
    }, timeoutMs, description);
  } catch (error: unknown) {
    if (
      error instanceof Error &&
      error.message === `${description} did not complete within ${String(timeoutMs)}ms`
    ) {
      throw new Error(`Provider action ${actionId} did not settle (awaited ${String(timeoutMs)}ms)`);
    }
    throw error;
  }
}

function bindCertifyingReviewOwner(
  database: Database.Database,
  input: Readonly<{ runId: string; actionId: string; taskId: string }>,
): void {
  const adapterId = "fake-lifecycle";
  const emptyFindingSet = "sha256:58afae1b74b0f7295f280a34196c2e092e4040016e64927e132f99356b48b7a2";
  database.prepare(`
    INSERT OR IGNORE INTO review_finding_sets(
      finding_set_digest,finding_count,page_count,canonical_byte_length,created_at
    ) VALUES (?,0,0,47,1)
  `).run(emptyFindingSet);
  const ownerDigest = database.prepare(`
    SELECT owner_digest FROM provider_action_pair_preflights WHERE adapter_id=? AND action_id=?
  `).pluck().get(adapterId, input.actionId);
  const reservationDigest = digest({ kind: "provider-owner-wiring", adapterId, actionId: input.actionId });
  database.prepare(`
    INSERT INTO review_finding_capacity_reservations(
      adapter_id,action_id,run_id,target_generation,slot,owner_digest,
      finding_window_mode,prior_open_finding_set_digest,maximum_new_findings,
      maximum_new_finding_bytes,reservation_digest,state,created_at,updated_at
    ) VALUES (?,?,?,1,'native',?,'normal',?,32,65536,?,'attached',1,1)
  `).run(adapterId, input.actionId, input.runId, ownerDigest, emptyFindingSet, reservationDigest);
  database.prepare(`
    UPDATE provider_actions SET finding_capacity_reservation_digest=?
     WHERE adapter_id=? AND action_id=?
  `).run(reservationDigest, adapterId, input.actionId);
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
    "reviewed_artifact_id", "publication_lineage_digest", "bundle_digest", "manifest_root_digest",
    "coverage_digest", "profile_digest", "profile_schema_digest", "final_prompt_digest",
  ]);
  const values = routeColumns.map(({ name, notnull }) => {
    if (name === "adapter_id" || name === "requested_adapter_id" || name === "resolved_adapter_id") return adapterId;
    if (name === "action_id") return input.actionId;
    if (name === "run_id") return input.runId;
    if (name === "task_id") return input.taskId;
    if (name === "certifying_review") return 1;
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

describe("provider-action owner boundary wiring", () => {
  it("revalidates persisted ownership before acknowledging dispatch re-entry", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const request = {
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "provider-owner-structural-reentry",
      operation: "send_turn" as const,
      payload: { scenario: "ambiguous-unproven", taskId: fixture.leaderTask.taskId },
      commandId: "provider-owner-structural-reentry:dispatch",
    };
    await expect(fixture.chair.dispatchProviderAction(request)).resolves.toMatchObject({
      status: "ambiguous",
      executionCount: 1,
    });

    const database = new Database(fixture.databasePath);
    database.pragma("foreign_keys = OFF");
    database.prepare(`
      UPDATE provider_actions SET finding_capacity_reservation_digest=?
       WHERE adapter_id=? AND action_id=?
    `).run(`sha256:${"a".repeat(64)}`, request.adapterId, request.actionId);
    database.close();

    await expect(fixture.chair.dispatchProviderAction(request)).rejects.toMatchObject({
      name: "ProviderActionOwnerError",
      expectedOwner: "generic",
      actualOwner: "integrity_failed",
    });
  });

  it("tombstones a doubly corrupt reserved action exactly once across startup passes", async () => {
    const fixture = await createLifecycleFixture({ payloadMaxTurns: true, spawnUnresolved: true });
    let originalClosed = false;
    let restarted: Awaited<ReturnType<typeof reopenLifecycleFabric>> | undefined;
    cleanup.push(async () => {
      await restarted?.close();
      if (!originalClosed) await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const authority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1, provider_calls: 1, concurrent_turns: 1 },
      },
      commandId: "startup-owner-double-corruption:authority",
    });
    const actionId = "startup-owner-double-corruption";
    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId,
      operation: "spawn",
      authorityId: authority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Remain unresolved for corrupt startup recovery.",
        maxTurns: 1,
        cwd: "src/leader",
      },
      commandId: `${actionId}:dispatch`,
    })).resolves.toMatchObject({ status: "prepared" });
    await expect(waitForProviderAction(fixture, actionId)).resolves.toBe("ambiguous");
    await fixture.fabric.close();
    originalClosed = true;

    const database = new Database(fixture.databasePath);
    database.pragma("foreign_keys = OFF");
    const budgetTriggers = database.prepare(`
      SELECT name,sql FROM sqlite_master
       WHERE type='trigger' AND (
         name LIKE 'provider_actions_budget_%' OR name='authority_budget_provider_ledger_update'
       ) ORDER BY name
    `).all() as Array<{ name: string; sql: string }>;
    if (budgetTriggers.length === 0) throw new Error("provider action budget triggers are unavailable");
    for (const trigger of budgetTriggers) database.exec(`DROP TRIGGER "${trigger.name}"`);
    database.pragma("ignore_check_constraints = ON");
    database.prepare(`
      UPDATE provider_actions
         SET finding_capacity_reservation_digest=?,budget_reservation_json='not-json',
             budget_settlement_json=NULL,budget_state='reserved'
       WHERE adapter_id='fake-lifecycle' AND action_id=?
    `).run(`sha256:${"f".repeat(64)}`, actionId);
    database.prepare("UPDATE authority_budget SET usage_unknown=0 WHERE authority_id=?")
      .run(authority.authorityId);
    database.pragma("ignore_check_constraints = OFF");
    for (const trigger of budgetTriggers) database.exec(trigger.sql);
    const beforeRecovery = database.prepare(`
      SELECT journal_revision FROM provider_actions
       WHERE adapter_id='fake-lifecycle' AND action_id=?
    `).get(actionId) as { journal_revision: number };
    const budgetBefore = database.prepare(`
      SELECT unit_key,reserved,provider_reserved,usage_unknown FROM authority_budget
       WHERE authority_id=? ORDER BY unit_key
    `).all(authority.authorityId);
    database.close();

    restarted = await reopenLifecycleFabric(fixture);
    await expect(restarted.recoverStartupState()).resolves.toMatchObject({ actionsQuarantined: 1 });
    await expect(restarted.recoverStartupState()).resolves.toMatchObject({ actionsQuarantined: 0 });

    const observed = new Database(fixture.databasePath, { readonly: true });
    const actionAfter = observed.prepare(`
      SELECT status,history_json,journal_revision,budget_state,budget_reservation_json
        FROM provider_actions WHERE adapter_id='fake-lifecycle' AND action_id=?
    `).get(actionId) as Record<string, unknown>;
    expect(actionAfter).toMatchObject({
      status: "quarantined",
      history_json: '["prepared","dispatched","ambiguous","quarantined"]',
      journal_revision: beforeRecovery.journal_revision + 1,
      budget_state: null,
      budget_reservation_json: null,
    });
    expect(observed.prepare(`
      SELECT COUNT(*) AS count FROM events
       WHERE run_id=? AND type='startup-provider-action-quarantined'
         AND json_extract(payload_json,'$.actionId')=?
    `).get(fixture.runId, actionId)).toEqual({ count: 1 });
    const budgetAfter = observed.prepare(`
      SELECT unit_key,reserved,provider_reserved,usage_unknown FROM authority_budget
       WHERE authority_id=? ORDER BY unit_key
    `).all(authority.authorityId) as Array<{
      unit_key: string; reserved: number; provider_reserved: number; usage_unknown: number;
    }>;
    expect(budgetAfter.map(({ reserved }) => reserved))
      .toStrictEqual((budgetBefore as Array<{ reserved: number }>).map(({ reserved }) => reserved));
    expect(budgetAfter.every(({ provider_reserved, usage_unknown }) =>
      provider_reserved === 0 && usage_unknown === 1)).toBe(true);
    observed.close();
    const ledgerProbe = new Database(fixture.databasePath);
    expect(() => ledgerProbe.prepare(`
      UPDATE authority_budget SET reserved=reserved WHERE authority_id=?
    `).run(authority.authorityId)).not.toThrow();
    ledgerProbe.close();
  });

  it("quarantines a pending row by rowid when persisted run and action identities are not strings", async () => {
    const fixture = await createLifecycleFixture();
    let originalClosed = false;
    let restarted: Awaited<ReturnType<typeof reopenLifecycleFabric>> | undefined;
    cleanup.push(async () => {
      await restarted?.close();
      if (!originalClosed) await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const actionId = "startup-owner-malformed-identity";
    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId,
      operation: "send_turn",
      payload: { scenario: "ambiguous-unproven", taskId: fixture.leaderTask.taskId },
      commandId: `${actionId}:dispatch`,
    })).resolves.toMatchObject({ status: "ambiguous" });
    await fixture.fabric.close();
    originalClosed = true;

    const database = new Database(fixture.databasePath);
    database.pragma("foreign_keys = OFF");
    const rowId = database.prepare(`
      SELECT rowid FROM provider_actions WHERE adapter_id='fake-lifecycle' AND action_id=?
    `).pluck().get(actionId);
    database.prepare(`
      UPDATE provider_actions SET run_id=x'0304',action_id=x'0102',target_agent_id=NULL
       WHERE adapter_id='fake-lifecycle' AND action_id=?
    `).run(actionId);
    database.close();

    restarted = await reopenLifecycleFabric(fixture);
    await expect(restarted.recoverStartupState()).resolves.toMatchObject({ actionsQuarantined: 1 });
    await expect(restarted.recoverStartupState()).resolves.toMatchObject({ actionsQuarantined: 0 });
    const observed = new Database(fixture.databasePath, { readonly: true });
    expect(observed.prepare("SELECT status FROM provider_actions WHERE rowid=?").get(rowId))
      .toEqual({ status: "quarantined" });
    expect(observed.prepare(`
      SELECT run_id,json_extract(payload_json,'$.actionId') AS action_id,COUNT(*) AS count FROM events
       WHERE type='startup-provider-action-quarantined'
    `).get()).toEqual({
      run_id: "<invalid-run-id:object>",
      action_id: "<invalid-action-id:object>",
      count: 1,
    });
    observed.close();
  });

  it("quarantines certifying-review owner failure after awaited startup lookup", async () => {
    const fixture = await createLifecycleFixture({ payloadMaxTurns: true });
    let originalClosed = false;
    let restarted: Awaited<ReturnType<typeof reopenLifecycleFabric>> | undefined;
    cleanup.push(async () => {
      await restarted?.close();
      if (!originalClosed) await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const authority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 2, "cost:USD": 2 },
      },
      commandId: "startup-certifying-owner:authority",
    });
    const actionId = "startup-certifying-owner";
    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId,
      operation: "spawn",
      authorityId: authority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Delay startup lookup for an owner mutation.",
        maxTurns: 2,
        cwd: "src/leader",
        scenario: "ambiguous-review-concurrent-divergent",
      },
      commandId: `${actionId}:dispatch`,
    })).resolves.toMatchObject({ status: "prepared" });
    await expect(waitForProviderAction(fixture, actionId)).resolves.toBe("ambiguous");
    await fixture.fabric.close();
    originalClosed = true;

    const database = new Database(fixture.databasePath);
    database.pragma("foreign_keys = OFF");
    bindCertifyingReviewOwner(database, {
      runId: fixture.runId,
      actionId,
      taskId: fixture.leaderTask.taskId,
    });
    database.close();

    restarted = await reopenLifecycleFabric(fixture);
    const recovery = restarted.recoverStartupState();
    const timeoutMs = 3_000;
    const description = "Action lookup start";
    let lookupCount = 0;
    try {
      await waitUntil(async () => {
        const journal = JSON.parse(await readFile(fixture.providerJournalPath, "utf8")) as {
          actions?: Record<string, { lookupCount?: number }>;
        };
        lookupCount = journal.actions?.[actionId]?.lookupCount ?? 0;
        return lookupCount >= 1;
      }, timeoutMs, description);
    } catch (error: unknown) {
      if (
        error instanceof Error &&
        error.message === `${description} did not complete within ${String(timeoutMs)}ms`
      ) {
        throw new Error(
          `Action lookup did not start: lookupCount=${String(lookupCount)} after ${String(timeoutMs)}ms`,
        );
      }
      throw error;
    }
    const mutated = new Database(fixture.databasePath);
    mutated.pragma("foreign_keys = OFF");
    const immutableRouteTrigger = mutated.prepare(`
      SELECT sql FROM sqlite_master WHERE type='trigger' AND name='provider_action_routes_immutable_update'
    `).pluck().get();
    if (typeof immutableRouteTrigger !== "string") throw new Error("provider action route trigger is unavailable");
    mutated.exec("DROP TRIGGER provider_action_routes_immutable_update");
    mutated.prepare(`
      UPDATE provider_action_routes SET slot='other-primary'
       WHERE run_id=? AND adapter_id='fake-lifecycle' AND action_id=?
    `).run(fixture.runId, actionId);
    mutated.exec(immutableRouteTrigger);
    mutated.close();
    await expect(recovery).resolves.toMatchObject({ actionsQuarantined: 1 });

    const observed = new Database(fixture.databasePath, { readonly: true });
    expect(observed.prepare(`
      SELECT status FROM provider_actions WHERE adapter_id='fake-lifecycle' AND action_id=?
    `).get(actionId)).toEqual({ status: "quarantined" });
    expect(observed.prepare(`
      SELECT COUNT(*) AS count FROM commands WHERE command_id LIKE 'startup-recovery:%'
        AND command_id LIKE '%' || ? || '%'
    `).get(actionId)).toEqual({ count: 0 });
    expect(observed.prepare(`
      SELECT COUNT(*) AS count FROM events WHERE run_id=? AND type='startup-provider-action-quarantined'
        AND json_extract(payload_json,'$.actionId')=?
    `).get(fixture.runId, actionId)).toEqual({ count: 1 });
    observed.close();
  });

  it("skips a row when startup quarantine fails and continues reconciling healthy rows", async () => {
    const fixture = await createLifecycleFixture();
    let originalClosed = false;
    let restarted: Awaited<ReturnType<typeof reopenLifecycleFabric>> | undefined;
    cleanup.push(async () => {
      await restarted?.close();
      if (!originalClosed) await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const healthyActionId = "startup-quarantine-failure-z-healthy";
    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: healthyActionId,
      operation: "send_turn",
      payload: { scenario: "terminal", taskId: fixture.leaderTask.taskId },
      commandId: `${healthyActionId}:dispatch`,
    })).resolves.toMatchObject({ status: "terminal" });
    const corruptActionId = "startup-quarantine-failure-a";
    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: corruptActionId,
      operation: "send_turn",
      payload: { scenario: "ambiguous-unproven", taskId: fixture.leaderTask.taskId },
      commandId: `${corruptActionId}:dispatch`,
    })).resolves.toMatchObject({ status: "ambiguous" });
    await fixture.fabric.close();
    originalClosed = true;

    const database = new Database(fixture.databasePath);
    database.pragma("foreign_keys = OFF");
    const valuesTrigger = database.prepare(`
      SELECT sql FROM sqlite_master WHERE type='trigger' AND name='provider_actions_values_update'
    `).pluck().get();
    if (typeof valuesTrigger !== "string") throw new Error("provider action values trigger is unavailable");
    database.exec("DROP TRIGGER provider_actions_values_update");
    database.prepare(`
      UPDATE provider_actions SET target_agent_id='missing-agent',updated_at=1
       WHERE adapter_id='fake-lifecycle' AND action_id=?
    `).run(corruptActionId);
    database.prepare(`
      UPDATE provider_actions
         SET status='ambiguous',history_json='["prepared","dispatched","ambiguous"]',
             idempotency_proven=0,result_json=NULL,updated_at=2
       WHERE adapter_id='fake-lifecycle' AND action_id=?
    `).run(healthyActionId);
    database.exec(valuesTrigger);
    const corruptRowId = String(database.prepare(`
      SELECT rowid FROM provider_actions WHERE adapter_id='fake-lifecycle' AND action_id=?
    `).pluck().get(corruptActionId));
    database.close();

    restarted = await reopenLifecycleFabric(fixture);
    await expect(restarted.recoverStartupState()).resolves.toMatchObject({
      actionsReconciled: 1,
      actionsQuarantined: 0,
    });

    const observed = new Database(fixture.databasePath, { readonly: true });
    expect(observed.prepare(`
      SELECT action_id,status FROM provider_actions
       WHERE adapter_id='fake-lifecycle' AND action_id IN (?,?) ORDER BY action_id
    `).all(corruptActionId, healthyActionId)).toEqual([
      { action_id: corruptActionId, status: "ambiguous" },
      { action_id: healthyActionId, status: "terminal" },
    ]);
    expect(observed.prepare(`
      SELECT run_id,json_extract(payload_json,'$.actionId') AS action_id,
             json_extract(payload_json,'$.adapterId') AS adapter_id,
             json_extract(payload_json,'$.rowId') AS row_id,
             json_extract(payload_json,'$.reason') AS reason,COUNT(*) AS count
        FROM events WHERE type='startup-provider-action-quarantine-failed'
    `).get()).toEqual({
      run_id: fixture.runId,
      action_id: corruptActionId,
      adapter_id: "fake-lifecycle",
      row_id: corruptRowId,
      reason: "INVARIANT_provider_actions_target_same_run",
      count: 1,
    });
    observed.close();
  });
});
