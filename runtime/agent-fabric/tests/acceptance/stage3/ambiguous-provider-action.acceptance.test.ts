import { createHash } from "node:crypto";
import { readFile, realpath, rm } from "node:fs/promises";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";
import type { FabricClient } from "../../../src/index.ts";
import { classifyProviderActionOwner } from "../../../src/application/provider-action-owner.ts";

import {
  asLifecycleClient,
  createLifecycleFixture,
  reopenLifecycleFabric,
} from "../../support/lifecycle-testkit.ts";
import { settle, waitUntil } from "../../shared/deadline-wait.ts";
import { admitProviderActionFixture } from "../../support/provider-action-fixture.ts";

const cleanup: Array<() => Promise<void>> = [];

type ObservedProviderAction = Awaited<ReturnType<FabricClient["getProviderAction"]>>;

function canonicalJson(value: unknown): string {
  if (value === null || typeof value === "boolean" || typeof value === "number" || typeof value === "string") {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (typeof value === "object") {
    const record = value as Record<string, unknown>;
    return `{${Object.keys(record).sort().map((key) =>
      `${JSON.stringify(key)}:${canonicalJson(record[key])}`
    ).join(",")}}`;
  }
  throw new Error("canonical test input must be JSON-compatible");
}

function canonicalDigest(value: unknown): string {
  return `sha256:${createHash("sha256").update(canonicalJson(value)).digest("hex")}`;
}

function bindCertifyingReviewOwner(input: Readonly<{
  databasePath: string;
  runId: string;
  adapterId: string;
  actionId: string;
  taskId: string;
}>): void {
  const database = new Database(input.databasePath);
  try {
    database.pragma("foreign_keys = OFF");
    const emptyFindingSet = "sha256:58afae1b74b0f7295f280a34196c2e092e4040016e64927e132f99356b48b7a2";
    database.prepare(`
      INSERT OR IGNORE INTO review_finding_sets(
        finding_set_digest,finding_count,page_count,canonical_byte_length,created_at
      ) VALUES (?,0,0,47,1)
    `).run(emptyFindingSet);
    const ownerDigest = database.prepare(`
      SELECT owner_digest FROM provider_action_pair_preflights
       WHERE adapter_id=? AND action_id=?
    `).pluck().get(input.adapterId, input.actionId);
    const reservationDigest = canonicalDigest({
      kind: "provider-owner-boundary-fixture",
      adapterId: input.adapterId,
      actionId: input.actionId,
    });
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
      ownerDigest,
      emptyFindingSet,
      reservationDigest,
    );
    database.prepare(`
      UPDATE provider_actions SET finding_capacity_reservation_digest=?
       WHERE adapter_id=? AND action_id=?
    `).run(reservationDigest, input.adapterId, input.actionId);

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
      if (name === "adapter_id" || name === "requested_adapter_id" || name === "resolved_adapter_id") {
        return input.adapterId;
      }
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
  } finally {
    database.close();
  }
}

async function waitForProviderAction(
  client: FabricClient,
  actionId: string,
  adapterId = "fake-lifecycle",
): Promise<ObservedProviderAction> {
  return await waitUntil(async () => {
    const action = await readProviderAction(client, actionId, adapterId);
    return ["terminal", "ambiguous", "quarantined"].includes(action.status) ? action : undefined;
  }, 5_000, `Provider action ${actionId} settlement`);
}

function readProviderAction(
  client: FabricClient,
  actionId: string,
  adapterId = "fake-lifecycle",
): Promise<ObservedProviderAction> {
  return client.getProviderAction({ adapterId, actionId, expectedActionKind: "non-review" });
}

function authorityBudget(
  databasePath: string,
  authorityId: string,
): Record<string, { granted: number; reserved: number; consumed: number; usageUnknown: boolean }> {
  const database = new Database(databasePath, { readonly: true });
  try {
    return Object.fromEntries(database.prepare(`
      SELECT unit_key,granted,reserved,consumed,usage_unknown
        FROM authority_budget WHERE authority_id=? ORDER BY unit_key
    `).all(authorityId).map((value) => {
      const row = value as {
        unit_key: string;
        granted: number;
        reserved: number;
        consumed: number;
        usage_unknown: number;
      };
      return [row.unit_key, {
        granted: row.granted,
        reserved: row.reserved,
        consumed: row.consumed,
        usageUnknown: row.usage_unknown === 1,
      }];
    }));
  } finally {
    database.close();
  }
}

afterEach(async () => {
  await Promise.all(cleanup.splice(0).map((close) => close()));
});

describe("NFR-004/AC-011 Stage 3 durable provider actions", () => {
  it("rejects provider authority without a positive hard turns ceiling before provider I/O", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: {
          provider_calls: 1,
          "cost:USD": 10,
          "input_tokens:fake": 10,
          "output_tokens:fake": 10,
        },
      },
      commandId: "provider-review-no-turns:authority",
    });
    const actionId = "provider-review-no-turns:spawn";

    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Hard turns capacity is mandatory.",
        cwd: "src/leader",
      },
      commandId: "provider-review-no-turns:dispatch",
    })).rejects.toMatchObject({ code: "BUDGET_EXCEEDED" });
    await expect(readProviderAction(fixture.chair, actionId)).rejects.toMatchObject({ code: "NOT_FOUND" });
  });

  it("reserves and exactly settles every configured provider budget dimension", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: {
          turns: 2,
          provider_calls: 2,
          concurrent_turns: 1,
          wall_clock_milliseconds: 1_000,
          "cost:USD": 10,
          "input_tokens:fake": 10,
          "output_tokens:fake": 10,
          descendants: 1,
          message_bytes: 128,
          artifact_bytes: 128,
        },
      },
      commandId: "provider-review-vector:authority",
    });

    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "provider-review-vector:spawn",
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Return exact bounded usage.",
        cwd: "src/leader",
        scenario: "terminal-exact-usage",
      },
      commandId: "provider-review-vector:dispatch",
    })).resolves.toMatchObject({ status: "prepared", effectCount: 0 });
    await expect(waitForProviderAction(fixture.chair, "provider-review-vector:spawn"))
      .resolves.toMatchObject({ status: "terminal", providerAnswer: "fake provider review complete" });

    expect(authorityBudget(fixture.databasePath, reviewAuthority.authorityId)).toMatchObject({
      turns: { granted: 2, reserved: 0, consumed: 1, usageUnknown: false },
      provider_calls: { granted: 2, reserved: 0, consumed: 1, usageUnknown: false },
      concurrent_turns: { granted: 1, reserved: 0, consumed: 0, usageUnknown: false },
      wall_clock_milliseconds: { granted: 1_000, reserved: 0, consumed: 0, usageUnknown: false },
      "cost:USD": { granted: 10, reserved: 0, consumed: 5, usageUnknown: false },
      "input_tokens:fake": { granted: 10, reserved: 0, consumed: 3, usageUnknown: false },
      "output_tokens:fake": { granted: 10, reserved: 0, consumed: 4, usageUnknown: false },
      descendants: { granted: 1, reserved: 0, consumed: 0, usageUnknown: false },
      message_bytes: { granted: 128, reserved: 0, consumed: 0, usageUnknown: false },
      artifact_bytes: { granted: 128, reserved: 0, consumed: 0, usageUnknown: false },
    });
  });

  it("settles the provider-reported turns and releases an unused multi-turn reservation", async () => {
    const fixture = await createLifecycleFixture({ payloadMaxTurns: true });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 2 },
      },
      commandId: "provider-review-partial-turns:authority",
    });

    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "provider-review-partial-turns:first",
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Use one turn within a two-turn ceiling.",
        maxTurns: 2,
        cwd: "src/leader",
        scenario: "terminal-partial-turn-usage",
      },
      commandId: "provider-review-partial-turns:first:dispatch",
    })).resolves.toMatchObject({ status: "prepared" });
    await expect(waitForProviderAction(fixture.chair, "provider-review-partial-turns:first"))
      .resolves.toMatchObject({ status: "terminal" });

    expect(authorityBudget(fixture.databasePath, reviewAuthority.authorityId)).toMatchObject({
      turns: { granted: 2, reserved: 0, consumed: 1, usageUnknown: false },
    });
    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "provider-review-partial-turns:second",
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Use the released turn.",
        maxTurns: 1,
        cwd: "src/leader",
        scenario: "terminal-partial-turn-usage",
      },
      commandId: "provider-review-partial-turns:second:dispatch",
    })).resolves.toMatchObject({ status: "prepared" });
    await expect(waitForProviderAction(fixture.chair, "provider-review-partial-turns:second"))
      .resolves.toMatchObject({ status: "terminal" });
  });

  it("keeps an unreported multi-turn usage reservation unknown", async () => {
    const fixture = await createLifecycleFixture({ payloadMaxTurns: true });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 2 },
      },
      commandId: "provider-review-missing-turns:authority",
    });

    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "provider-review-missing-turns:spawn",
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Omit actual multi-turn usage.",
        maxTurns: 2,
        cwd: "src/leader",
      },
      commandId: "provider-review-missing-turns:dispatch",
    })).resolves.toMatchObject({ status: "prepared" });
    await expect(waitForProviderAction(fixture.chair, "provider-review-missing-turns:spawn"))
      .resolves.toMatchObject({ status: "terminal" });
    expect(authorityBudget(fixture.databasePath, reviewAuthority.authorityId)).toMatchObject({
      turns: { granted: 2, reserved: 2, consumed: 0, usageUnknown: true },
    });
  });

  it.each(["terminal-malformed-turn-usage", "terminal-over-turn-usage"] as const)(
    "quarantines %s against the admitted multi-turn ceiling",
    async (scenario) => {
      const fixture = await createLifecycleFixture({ payloadMaxTurns: true });
      cleanup.push(async () => {
        await fixture.fabric.close();
        await rm(fixture.directory, { recursive: true, force: true });
      });
      const reviewAuthority = await fixture.chair.delegateAuthority({
        parentAuthorityId: fixture.chairAuthorityId,
        authority: {
          ...fixture.rootAuthority,
          sourcePaths: ["src/leader"],
          actions: [...fixture.rootAuthority.actions],
          budget: { turns: 2 },
        },
        commandId: `provider-review-invalid-turns:${scenario}:authority`,
      });
      const actionId = `provider-review-invalid-turns:${scenario}:spawn`;

      await expect(fixture.chair.dispatchProviderAction({
        certifyingReview: null,
        adapterId: "fake-lifecycle",
        actionId,
        operation: "spawn",
        authorityId: reviewAuthority.authorityId,
        taskId: fixture.leaderTask.taskId,
        payload: {
          model: "fake-reviewer-v1",
          modelFamily: "fake",
          prompt: "Return invalid actual turn usage.",
          maxTurns: 2,
          cwd: "src/leader",
          scenario,
        },
        commandId: `provider-review-invalid-turns:${scenario}:dispatch`,
      })).resolves.toMatchObject({ status: "prepared" });
      await expect(waitForProviderAction(fixture.chair, actionId)).resolves.toMatchObject({ status: "ambiguous" });
      await expect(fixture.chair.reconcileProviderAction({
        adapterId: "fake-lifecycle",
        actionId,
        commandId: `provider-review-invalid-turns:${scenario}:reconcile`,
      })).resolves.toMatchObject({ status: "quarantined" });
      expect(authorityBudget(fixture.databasePath, reviewAuthority.authorityId)).toMatchObject({
        turns: { granted: 2, reserved: 2, consumed: 0, usageUnknown: true },
      });
    },
  );

  it.each([
    "terminal-unreserved-usage",
    "terminal-over-cap-usage",
    "terminal-malformed-usage",
  ] as const)("quarantines %s before accepting terminal settlement", async (scenario) => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1, "cost:USD": 10 },
      },
      commandId: `provider-review-invalid-usage:${scenario}:authority`,
    });
    const actionId = `provider-review-invalid-usage:${scenario}:spawn`;

    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Return an invalid usage vector.",
        cwd: "src/leader",
        scenario,
      },
      commandId: `provider-review-invalid-usage:${scenario}:dispatch`,
    })).resolves.toMatchObject({ status: "prepared" });
    await expect(waitForProviderAction(fixture.chair, actionId)).resolves.toMatchObject({ status: "ambiguous" });
    await expect(fixture.chair.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId,
      commandId: `provider-review-invalid-usage:${scenario}:reconcile`,
    })).resolves.toMatchObject({ status: "quarantined", executionCount: 1 });
    expect(authorityBudget(fixture.databasePath, reviewAuthority.authorityId)).toMatchObject({
      turns: { reserved: 1, consumed: 0, usageUnknown: true },
    });
  });

  it("rejects adapter-mandatory usage dimensions before provider I/O", async () => {
    const fixture = await createLifecycleFixture({ mandatoryUsageUnits: true });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1 },
      },
      commandId: "provider-review-mandatory-usage:authority",
    });
    const actionId = "provider-review-mandatory-usage:spawn";

    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Do not run without mandatory usage capacity.",
        cwd: "src/leader",
      },
      commandId: "provider-review-mandatory-usage:dispatch",
    })).rejects.toMatchObject({ code: "CAPABILITY_UNAVAILABLE" });
    await expect(readProviderAction(fixture.chair, actionId)).rejects.toMatchObject({ code: "NOT_FOUND" });
  });

  it("reconciles late exact usage without replaying the provider effect", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: {
          turns: 1,
          provider_calls: 1,
          "cost:USD": 10,
          "input_tokens:fake": 10,
          "output_tokens:fake": 10,
        },
      },
      commandId: "provider-review-late-usage:authority",
    });
    const actionId = "provider-review-late-usage:spawn";
    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Return usage through durable lookup.",
        cwd: "src/leader",
        scenario: "ambiguous-review-usage-late",
      },
      commandId: "provider-review-late-usage:dispatch",
    })).resolves.toMatchObject({ status: "prepared" });
    await expect(waitForProviderAction(fixture.chair, actionId)).resolves.toMatchObject({ status: "ambiguous" });
    await expect(fixture.chair.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId,
      commandId: "provider-review-late-usage:reconcile-1",
    })).resolves.toMatchObject({
      status: "terminal",
      providerAnswer: "recovered provider review with late usage",
    });
    expect(authorityBudget(fixture.databasePath, reviewAuthority.authorityId)).toMatchObject({
      turns: { reserved: 0, consumed: 1, usageUnknown: false },
      provider_calls: { reserved: 0, consumed: 1, usageUnknown: false },
      "cost:USD": { reserved: 10, consumed: 0, usageUnknown: true },
      "input_tokens:fake": { reserved: 10, consumed: 0, usageUnknown: true },
      "output_tokens:fake": { reserved: 10, consumed: 0, usageUnknown: true },
    });

    await expect(fixture.chair.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId,
      commandId: "provider-review-late-usage:reconcile-2",
    })).resolves.toMatchObject({ status: "terminal", executionCount: 1, effectCount: 1 });
    expect(authorityBudget(fixture.databasePath, reviewAuthority.authorityId)).toMatchObject({
      "cost:USD": { reserved: 0, consumed: 5, usageUnknown: false },
      "input_tokens:fake": { reserved: 0, consumed: 3, usageUnknown: false },
      "output_tokens:fake": { reserved: 0, consumed: 4, usageUnknown: false },
    });
  });

  it("rechecks task state atomically after adapter capabilities before provider dispatch", async () => {
    const fixture = await createLifecycleFixture({ capabilitiesDelayMs: 100 });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1 },
      },
      commandId: "provider-review-task-race:authority",
    });
    const dispatch = fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "provider-review-task-race:spawn",
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Do not start after task completion.",
        cwd: "src/leader",
      },
      commandId: "provider-review-task-race:dispatch",
    }).then(
      (value) => ({ status: "fulfilled" as const, value }),
      (error: unknown) => ({ status: "rejected" as const, error }),
    );
    await new Promise((resolve) => setTimeout(resolve, 25));
    await fixture.leader.updateTask({
      taskId: fixture.leaderTask.taskId,
      expectedRevision: fixture.leaderTask.revision,
      state: "complete",
      commandId: "provider-review-task-race:complete",
    });

    await expect(dispatch).resolves.toMatchObject({
      status: "rejected",
      error: { code: "LIFECYCLE_PRECONDITION_FAILED" },
    });
    await expect(readProviderAction(fixture.chair, "provider-review-task-race:spawn"))
      .rejects.toMatchObject({ code: "NOT_FOUND" });
  });

  it("does not classify the same action ID on another adapter as special custody", async () => {
    const fixture = await createLifecycleFixture({ capabilitiesDelayMs: 1_000 });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const actionId = "provider-review-cross-adapter:shared";
    const now = fixture.clock.now().getTime();
    const seed = new Database(fixture.databasePath);
    try {
      admitProviderActionFixture(seed, {
        runId: fixture.runId,
        actionId,
        adapterId: "herdr-control-v1",
        operation: "wakeup",
        identityHash: "a".repeat(64),
        payloadHash: "b".repeat(64),
        payloadJson: "{}",
        status: "terminal",
        historyJson: '["prepared","dispatched","terminal"]',
        executionCount: 1,
        effectCount: 1,
        idempotencyProven: true,
        resultJson: "{}",
        updatedAt: now,
      }, undefined, undefined, "herdr");
    } finally {
      seed.close();
    }
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1 },
      },
      commandId: "provider-review-cross-adapter:authority",
    });
    const dispatch = fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Do not alias another adapter's action.",
        cwd: "src/leader",
      },
      commandId: "provider-review-cross-adapter:dispatch",
    }).then(
      (value) => ({ status: "fulfilled" as const, value }),
      (error: unknown) => ({ status: "rejected" as const, error }),
    );
    await new Promise((resolve) => setTimeout(resolve, 25));
    await fixture.leader.updateTask({
      taskId: fixture.leaderTask.taskId,
      expectedRevision: fixture.leaderTask.revision,
      state: "complete",
      commandId: "provider-review-cross-adapter:complete",
    });

    const dispatchOutcome = await dispatch;
    expect(dispatchOutcome).toMatchObject({ status: "rejected" });
    if (dispatchOutcome.status !== "rejected") return;
    expect(dispatchOutcome.error).toMatchObject({ code: "LIFECYCLE_PRECONDITION_FAILED" });
    expect((dispatchOutcome.error as Error).message)
      .toBe("terminal task cannot admit an ephemeral provider spawn");
  });

  it("keeps the same action ID independent across two live adapters and restart", async () => {
    const fixture = await createLifecycleFixture({ secondaryAdapter: true });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const authority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: {
          turns: 4,
          provider_calls: 4,
          concurrent_turns: 2,
          wall_clock_milliseconds: 10_000,
          "cost:USD": 10,
          "input_tokens:fake": 100,
          "output_tokens:fake": 100,
          descendants: 1,
          message_bytes: 1_000,
          artifact_bytes: 1_000,
        },
      },
      commandId: "provider-review-pair:authority",
    });
    const actionId = "provider-review-pair:shared";
    const request = (adapterId: string, commandId: string) => ({
      certifyingReview: null,
      adapterId,
      actionId,
      operation: "spawn" as const,
      taskId: fixture.leaderTask.taskId,
      authorityId: authority.authorityId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: `Run independently on ${adapterId}.`,
        cwd: "src/leader",
        scenario: "terminal-exact-usage",
      },
      commandId,
    });

    const primary = await fixture.chair.dispatchProviderAction(
      request("fake-lifecycle", "provider-review-pair:primary"),
    );
    expect(primary).toMatchObject({ actionId, status: "prepared" });
    const primarySettled = await waitForProviderAction(fixture.chair, actionId, "fake-lifecycle");
    const secondary = await fixture.chair.dispatchProviderAction(
      request("fake-lifecycle-secondary", "provider-review-pair:secondary"),
    );
    expect(secondary).toMatchObject({ actionId, status: "prepared" });
    const secondarySettled = await waitForProviderAction(
      fixture.chair,
      actionId,
      "fake-lifecycle-secondary",
    );
    expect(primarySettled).toMatchObject({ actionId, executionCount: 1 });
    expect(secondarySettled).toMatchObject({ actionId, executionCount: 1 });

    await expect(fixture.chair.dispatchProviderAction(
      request("fake-lifecycle", "provider-review-pair:primary-retry"),
    )).resolves.toEqual(primarySettled);
    const secondaryReconciled = await fixture.chair.reconcileProviderAction({
      adapterId: "fake-lifecycle-secondary",
      actionId,
      commandId: "provider-review-pair:secondary-reconcile",
    });
    await expect(readProviderAction(fixture.chair, actionId, "fake-lifecycle"))
      .resolves.toEqual(primarySettled);
    await expect(readProviderAction(fixture.chair, actionId, "fake-lifecycle-secondary"))
      .resolves.toEqual(secondaryReconciled);

    await fixture.fabric.close();
    const reopened = await reopenLifecycleFabric(fixture);
    cleanup.push(async () => reopened.close());
    const chair = asLifecycleClient(reopened.connect(fixture.capabilities.chair));
    await expect(readProviderAction(chair, actionId, "fake-lifecycle")).resolves.toEqual(primarySettled);
    await expect(readProviderAction(chair, actionId, "fake-lifecycle-secondary"))
      .resolves.toEqual(secondaryReconciled);
  });

  it("persists the canonical pair preflight before adapter capability inspection", async () => {
    const actionRef = {
      adapterId: "fake-lifecycle",
      actionId: "provider-review-preflight:spawn",
    } as const;
    type CapabilityBoundaryObservation = {
      preflights: unknown[];
      dependants: unknown;
    };
    let databasePath: string | undefined;
    let capabilityBoundaryCalls = 0;
    let resolveCapabilityBoundary: (value: CapabilityBoundaryObservation) => void = () => {
      throw new Error("provider capability boundary resolver is unavailable");
    };
    const capabilityBoundary = new Promise<CapabilityBoundaryObservation>((resolvePromise) => {
      resolveCapabilityBoundary = resolvePromise;
    });
    const fixture = await createLifecycleFixture({
      capabilitiesDelayMs: 1_000,
      fault: (label) => {
        if (label !== "provider-action:before-capability-inspection") return;
        capabilityBoundaryCalls += 1;
        if (capabilityBoundaryCalls !== 1) {
          throw new Error("provider capability boundary fired more than once");
        }
        if (databasePath === undefined) {
          throw new Error("provider capability boundary fired before fixture setup completed");
        }
        const observer = new Database(databasePath, { readonly: true });
        try {
          resolveCapabilityBoundary({
            preflights: observer.prepare(`
              SELECT adapter_id,action_id,run_id,owner_digest,actor_principal_digest,input_digest,state
                FROM provider_action_pair_preflights
               WHERE adapter_id=? AND action_id=?
            `).all(actionRef.adapterId, actionRef.actionId),
            dependants: observer.prepare(`
              SELECT
                (SELECT COUNT(*) FROM adapter_effective_configurations
                  WHERE subject_action_adapter_id=? AND subject_action_id=?) AS configuration_count,
                (SELECT COUNT(*) FROM review_finding_capacity_reservations
                  WHERE adapter_id=? AND action_id=?) AS reservation_count,
                (SELECT COUNT(*) FROM provider_actions
                  WHERE adapter_id=? AND action_id=?) AS action_count,
                (SELECT COUNT(*) FROM provider_action_routes
                  WHERE adapter_id=? AND action_id=?) AS route_count
            `).get(
              actionRef.adapterId, actionRef.actionId,
              actionRef.adapterId, actionRef.actionId,
              actionRef.adapterId, actionRef.actionId,
              actionRef.adapterId, actionRef.actionId,
            ),
          });
        } finally {
          observer.close();
        }
      },
    });
    databasePath = fixture.databasePath;
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1 },
      },
      commandId: "provider-review-preflight:authority",
    });
    const canonicalFixtureDirectory = await realpath(fixture.directory);
    const providerPayload = {
      taskId: fixture.leaderTask.taskId,
      model: "fake-reviewer-v1",
      modelFamily: "fake",
      prompt: "Release this admission after the task terminalises.",
      maxTurns: 1,
      cwd: `${canonicalFixtureDirectory}/src/leader`,
      readOnlyRoot: `${canonicalFixtureDirectory}/src/leader`,
      allowedTools: ["Read", "Glob", "Grep"],
      approvalPolicy: "never",
      sandbox: "read-only",
    } as const;
    const scope = { kind: "run-action", runId: fixture.runId } as const;
    const canonicalInput = {
      schemaVersion: 1,
      scope,
      actionRef,
      operation: "spawn",
      taskId: fixture.leaderTask.taskId,
      authorityId: reviewAuthority.authorityId,
      targetAgentId: null,
      providerSessionGeneration: null,
      providerPayload,
      routeRequest: null,
      certifyingBinding: null,
    } as const;
    const identityDatabase = new Database(fixture.databasePath, { readonly: true });
    const principal = identityDatabase.prepare(`
      SELECT run.project_session_id AS projectSessionId,
             capability.principal_generation AS principalGeneration
        FROM runs run
        JOIN capabilities capability
          ON capability.run_id=run.run_id AND capability.agent_id=run.chair_agent_id
       WHERE run.run_id=? AND capability.revoked_at IS NULL
       ORDER BY capability.principal_generation DESC
       LIMIT 1
    `).get(fixture.runId) as {
      projectSessionId: string;
      principalGeneration: number;
    };
    identityDatabase.close();
    const actorPrincipalDigest = canonicalDigest({
      agentId: "chair",
      projectSessionId: principal.projectSessionId,
      coordinationRunId: fixture.runId,
      principalGeneration: principal.principalGeneration,
    });
    const inputDigest = canonicalDigest(canonicalInput);
    const ownerDigest = canonicalDigest({
      schemaVersion: 1,
      scope,
      actionRef,
      actorPrincipalDigest,
      inputDigest,
    });

    const dispatch = fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      ...actionRef,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: providerPayload.model,
        modelFamily: providerPayload.modelFamily,
        prompt: providerPayload.prompt,
        cwd: "src/leader",
      },
      commandId: "provider-review-preflight:dispatch",
    }).then(
      (value) => ({ status: "fulfilled" as const, value }),
      (error: unknown) => ({ status: "rejected" as const, error }),
    );
    const boundaryOrDispatch = await Promise.race([
      capabilityBoundary.then((observation) => ({ kind: "boundary" as const, observation })),
      dispatch.then((outcome) => ({ kind: "dispatch" as const, outcome })),
    ]);
    expect(boundaryOrDispatch).toMatchObject({ kind: "boundary" });
    if (boundaryOrDispatch.kind !== "boundary") return;
    expect(capabilityBoundaryCalls).toBe(1);
    const inFlightObservation = boundaryOrDispatch.observation;

    await fixture.leader.updateTask({
      taskId: fixture.leaderTask.taskId,
      expectedRevision: fixture.leaderTask.revision,
      state: "complete",
      commandId: "provider-review-preflight:complete",
    });
    const dispatchOutcome = await dispatch;

    const terminalObserver = new Database(fixture.databasePath, { readonly: true });
    const terminalObservation = (() => {
      try {
        return {
          preflight: terminalObserver.prepare(`
            SELECT state,failure_json FROM provider_action_pair_preflights
             WHERE adapter_id=? AND action_id=?
          `).get(actionRef.adapterId, actionRef.actionId),
          dependants: terminalObserver.prepare(`
            SELECT
              (SELECT COUNT(*) FROM adapter_effective_configurations
                WHERE subject_action_adapter_id=? AND subject_action_id=?) AS configuration_count,
              (SELECT COUNT(*) FROM review_finding_capacity_reservations
                WHERE adapter_id=? AND action_id=?) AS reservation_count,
              (SELECT COUNT(*) FROM provider_actions
                WHERE adapter_id=? AND action_id=?) AS action_count,
              (SELECT COUNT(*) FROM provider_action_routes
                WHERE adapter_id=? AND action_id=?) AS route_count
          `).get(
            actionRef.adapterId, actionRef.actionId,
            actionRef.adapterId, actionRef.actionId,
            actionRef.adapterId, actionRef.actionId,
            actionRef.adapterId, actionRef.actionId,
          ),
        };
      } finally {
        terminalObserver.close();
      }
    })();

    expect(inFlightObservation.preflights).toEqual([{
      adapter_id: actionRef.adapterId,
      action_id: actionRef.actionId,
      run_id: fixture.runId,
      owner_digest: ownerDigest,
      actor_principal_digest: actorPrincipalDigest,
      input_digest: inputDigest,
      state: "resolving",
    }]);
    expect(inFlightObservation.dependants).toEqual({
      configuration_count: 0,
      reservation_count: 0,
      action_count: 0,
      route_count: 0,
    });
    expect(dispatchOutcome).toMatchObject({
      status: "rejected",
      error: { code: "LIFECYCLE_PRECONDITION_FAILED" },
    });
    expect(terminalObservation.preflight).toMatchObject({
      state: "released",
      failure_json: expect.stringContaining("terminal task cannot admit an ephemeral provider spawn"),
    });
    expect(terminalObservation.dependants).toEqual({
      configuration_count: 0,
      reservation_count: 0,
      action_count: 0,
      route_count: 0,
    });
    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      ...actionRef,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: providerPayload.model,
        modelFamily: providerPayload.modelFamily,
        prompt: providerPayload.prompt,
        cwd: "src/leader",
      },
      commandId: "provider-review-preflight:retry-released",
    })).rejects.toMatchObject({
      code: "LIFECYCLE_PRECONDITION_FAILED",
      message: "terminal task cannot admit an ephemeral provider spawn",
    });
    expect(capabilityBoundaryCalls).toBe(1);

    await fixture.fabric.close();
    const reopened = await reopenLifecycleFabric(fixture);
    cleanup.push(async () => reopened.close());
    const reopenedChair = asLifecycleClient(reopened.connect(fixture.capabilities.chair));
    await expect(reopenedChair.dispatchProviderAction({
      certifyingReview: null,
      ...actionRef,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: providerPayload.model,
        modelFamily: providerPayload.modelFamily,
        prompt: providerPayload.prompt,
        cwd: "src/leader",
      },
      commandId: "provider-review-preflight:retry-released-after-reopen",
    })).rejects.toMatchObject({
      code: "LIFECYCLE_PRECONDITION_FAILED",
      message: "terminal task cannot admit an ephemeral provider spawn",
    });
  });

  it("joins exact pair replay once and conflicts changed input before resolver work", async () => {
    let capabilityBoundaryCalls = 0;
    let resolveFirstCapabilityBoundary: () => void = () => {
      throw new Error("first capability boundary resolver is unavailable");
    };
    const firstCapabilityBoundary = new Promise<void>((resolvePromise) => {
      resolveFirstCapabilityBoundary = resolvePromise;
    });
    const fixture = await createLifecycleFixture({
      capabilitiesDelayMs: 1_000,
      fault: (label) => {
        if (label !== "provider-action:before-capability-inspection") return;
        capabilityBoundaryCalls += 1;
        if (capabilityBoundaryCalls === 1) resolveFirstCapabilityBoundary();
      },
    });
    let activeFabric = fixture.fabric;
    cleanup.push(async () => {
      await activeFabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 2 },
      },
      commandId: "provider-review-pair-flight:authority",
    });
    const actionRef = {
      adapterId: "fake-lifecycle",
      actionId: "provider-review-pair-flight:spawn",
    } as const;
    const providerTaskId = fixture.leaderTask.taskId;
    const providerPayload = {
      model: "fake-reviewer-v1",
      modelFamily: "fake",
      prompt: "Join this exact semantic provider request.",
      cwd: "src/leader",
    };
    const dispatch = (commandId: string, payload: typeof providerPayload) =>
      fixture.chair.dispatchProviderAction({
        certifyingReview: null,
        ...actionRef,
        operation: "spawn",
        taskId: providerTaskId,
        authorityId: reviewAuthority.authorityId,
        payload,
        commandId,
      });
    const outcome = async <T>(promise: Promise<T>) => await promise.then(
      (value) => ({ status: "fulfilled" as const, value }),
      (error: unknown) => ({ status: "rejected" as const, error }),
    );

    const first = outcome(dispatch("provider-review-pair-flight:exact:first", providerPayload));
    await firstCapabilityBoundary;
    const second = outcome(dispatch("provider-review-pair-flight:exact:second", providerPayload));
    const changed = outcome(dispatch("provider-review-pair-flight:changed:first", {
      ...providerPayload,
      prompt: "This changed prompt must conflict before resolver work.",
    }));
    const [firstOutcome, secondOutcome, changedOutcome] = await Promise.all([first, second, changed]);

    expect(changedOutcome).toMatchObject({
      status: "rejected",
      error: { code: "ACTION_INPUT_CONFLICT" },
    });
    expect(capabilityBoundaryCalls).toBe(1);
    expect([firstOutcome.status, secondOutcome.status]).toEqual(["fulfilled", "fulfilled"]);
    if (firstOutcome.status !== "fulfilled" || secondOutcome.status !== "fulfilled") return;
    expect(secondOutcome.value).toEqual(firstOutcome.value);
    expect(firstOutcome.value).toMatchObject({
      actionId: actionRef.actionId,
      status: "prepared",
      executionCount: 0,
      effectCount: 0,
    });

    const durable = await waitForProviderAction(fixture.chair, actionRef.actionId);
    expect(durable).toMatchObject({
      actionId: actionRef.actionId,
      status: "terminal",
      executionCount: 1,
      effectCount: 1,
      providerAnswer: "fake provider review complete",
    });
    const providerJournalBeforeRestart = await readFile(fixture.providerJournalPath, "utf8");
    const providerJournal = JSON.parse(providerJournalBeforeRestart) as {
      actions: Record<string, { executionCount?: number }>;
      sessions: Record<string, { spawnRequests?: number }>;
    };
    expect(providerJournal.actions[actionRef.actionId]).toMatchObject({ executionCount: 1 });
    expect(Object.values(providerJournal.sessions).reduce(
      (total, session) => total + (session.spawnRequests ?? 0),
      0,
    )).toBe(1);
    const verification = new Database(fixture.databasePath, { readonly: true });
    try {
      expect(verification.prepare(`
        SELECT COUNT(*) AS count FROM provider_actions
         WHERE run_id=? AND adapter_id=? AND action_id=?
      `).get(fixture.runId, actionRef.adapterId, actionRef.actionId)).toEqual({ count: 1 });
      expect(verification.prepare(`
        SELECT state FROM provider_action_pair_preflights
         WHERE adapter_id=? AND action_id=?
      `).get(actionRef.adapterId, actionRef.actionId)).toEqual({ state: "admitted" });
      const commandResults = verification.prepare(`
        SELECT result_json FROM commands
         WHERE run_id=? AND actor_agent_id='chair' AND command_id IN (?,?)
         ORDER BY command_id
      `).all(
        fixture.runId,
        "provider-review-pair-flight:exact:first",
        "provider-review-pair-flight:exact:second",
      ) as Array<{ result_json: string }>;
      expect(commandResults).toHaveLength(2);
      expect(commandResults[0]?.result_json).toBe(commandResults[1]?.result_json);
    } finally {
      verification.close();
    }

    await activeFabric.close();
    activeFabric = await reopenLifecycleFabric(fixture);
    const reopenedChair = asLifecycleClient(activeFabric.connect(fixture.capabilities.chair));
    await expect(reopenedChair.dispatchProviderAction({
      certifyingReview: null,
      ...actionRef,
      operation: "spawn",
      taskId: providerTaskId,
      authorityId: reviewAuthority.authorityId,
      payload: providerPayload,
      commandId: "provider-review-pair-flight:restart:exact",
    })).resolves.toEqual(durable);
    await expect(reopenedChair.dispatchProviderAction({
      certifyingReview: null,
      ...actionRef,
      operation: "spawn",
      taskId: providerTaskId,
      authorityId: reviewAuthority.authorityId,
      payload: {
        ...providerPayload,
        prompt: "This changed restart prompt must retain the conflict.",
      },
      commandId: "provider-review-pair-flight:restart:changed",
    })).rejects.toMatchObject({ code: "ACTION_INPUT_CONFLICT" });
    expect(await readFile(fixture.providerJournalPath, "utf8")).toBe(providerJournalBeforeRestart);
    await expect(readProviderAction(reopenedChair, actionRef.actionId)).resolves.toEqual(durable);
  });

  it("replays an admitted pair after only its delegated authority expires", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const authority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        expiresAt: new Date(fixture.clock.now().getTime() + 100).toISOString(),
        budget: { turns: 1 },
      },
      commandId: "provider-review-expired-replay:authority",
    });
    const request = (commandId: string) => ({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "provider-review-expired-replay:spawn",
      operation: "spawn" as const,
      taskId: fixture.leaderTask.taskId,
      authorityId: authority.authorityId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Replay this admitted action after delegated authority expiry.",
        cwd: "src/leader",
      },
      commandId,
    });
    await fixture.chair.dispatchProviderAction(request("provider-review-expired-replay:first"));
    const admitted = await waitForProviderAction(
      fixture.chair,
      "provider-review-expired-replay:spawn",
    );
    fixture.clock.advance(200);
    await expect(fixture.chair.dispatchProviderAction(
      request("provider-review-expired-replay:retry"),
    )).resolves.toEqual(admitted);
  });

  it.each([
    "provider-action-admission:after-action-insert",
    "provider-action-admission:after-dependants",
    "provider-action-admission:after-final-cas",
  ] as const)("leaves only the durable resolving preflight after %s faults", async (faultLabel) => {
    const fixture = await createLifecycleFixture({
      fault: (label) => {
        if (label === faultLabel) throw new Error(`fault:${faultLabel}`);
      },
    });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1 },
      },
      commandId: `provider-review-admission-fault:${faultLabel}:authority`,
    });
    const actionRef = {
      adapterId: "fake-lifecycle",
      actionId: `provider-review-admission-fault:${faultLabel}:spawn`,
    } as const;

    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      ...actionRef,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Rollback this admission cut without releasing its durable preflight.",
        cwd: "src/leader",
      },
      commandId: `provider-review-admission-fault:${faultLabel}:dispatch`,
    })).rejects.toThrow(`fault:${faultLabel}`);

    const observer = new Database(fixture.databasePath, { readonly: true });
    try {
      expect(observer.prepare(`
        SELECT state FROM provider_action_pair_preflights
         WHERE adapter_id=? AND action_id=?
      `).get(actionRef.adapterId, actionRef.actionId)).toEqual({ state: "resolving" });
      expect(observer.prepare(`
        SELECT
          (SELECT COUNT(*) FROM provider_actions WHERE adapter_id=? AND action_id=?) AS action_count,
          (SELECT COUNT(*) FROM provider_action_routes WHERE adapter_id=? AND action_id=?) AS route_count,
          (SELECT COUNT(*) FROM commands WHERE command_id=?) AS command_count
      `).get(
        actionRef.adapterId,
        actionRef.actionId,
        actionRef.adapterId,
        actionRef.actionId,
        `provider-review-admission-fault:${faultLabel}:dispatch`,
      )).toEqual({ action_count: 0, route_count: 0, command_count: 0 });
    } finally {
      observer.close();
    }
  });

  it.each([
    ["send_turn", "chair"] as const,
    ["steer", "chair"] as const,
  ])("keeps a %s admission fault resolving", async (operation, actor) => {
    const faultLabel = "provider-action-admission:after-action-insert";
    const fixture = await createLifecycleFixture({
      fault: (label) => {
        if (label === faultLabel) throw new Error(`fault:${operation}`);
      },
    });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const actionId = `provider-review-${operation}-admission-fault`;
    const client = actor === "chair" ? fixture.chair : fixture.leader;
    await expect(client.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId,
      operation,
      payload: operation === "send_turn"
        ? { scenario: "terminal", taskId: fixture.leaderTask.taskId }
        : { instruction: "Do not cross the faulted admission cut." },
      commandId: `provider-review-${operation}-admission-fault:dispatch`,
    })).rejects.toThrow(`fault:${operation}`);

    const observer = new Database(fixture.databasePath, { readonly: true });
    try {
      expect(observer.prepare(`
        SELECT state FROM provider_action_pair_preflights
         WHERE adapter_id='fake-lifecycle' AND action_id=?
      `).get(actionId)).toEqual({ state: "resolving" });
      expect(observer.prepare(`
        SELECT COUNT(*) AS count FROM provider_actions
         WHERE adapter_id='fake-lifecycle' AND action_id=?
      `).get(actionId)).toEqual({ count: 0 });
    } finally {
      observer.close();
    }
  });

  it("rolls back a faulted preflight insert before any admission work", async () => {
    const faultLabel = "provider-action-admission:after-preflight-insert";
    const fixture = await createLifecycleFixture({
      fault: (label) => {
        if (label === faultLabel) throw new Error(`fault:${faultLabel}`);
      },
    });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1 },
      },
      commandId: "provider-review-preflight-insert-fault:authority",
    });
    const actionRef = {
      adapterId: "fake-lifecycle",
      actionId: "provider-review-preflight-insert-fault:spawn",
    } as const;
    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      ...actionRef,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Rollback this preflight insert.",
        cwd: "src/leader",
      },
      commandId: "provider-review-preflight-insert-fault:dispatch",
    })).rejects.toThrow(`fault:${faultLabel}`);
    const observer = new Database(fixture.databasePath, { readonly: true });
    try {
      expect(observer.prepare(`
        SELECT COUNT(*) AS count FROM provider_action_pair_preflights
         WHERE adapter_id=? AND action_id=?
      `).get(actionRef.adapterId, actionRef.actionId)).toEqual({ count: 0 });
      expect(observer.prepare(`
        SELECT COUNT(*) AS count FROM provider_actions WHERE adapter_id=? AND action_id=?
      `).get(actionRef.adapterId, actionRef.actionId)).toEqual({ count: 0 });
    } finally {
      observer.close();
    }
  });

  it.each([
    ["provider-action-admission:after-action-insert", "action"],
    ["provider-action-admission:after-dependant-command-append", "command"],
    ["provider-action-admission:after-final-preflight-cas", "preflight-cas"],
  ] as const)("keeps the separately committed preflight resolving across %s", async (faultLabel, cut) => {
    const fixture = await createLifecycleFixture();
    let activeFabric = fixture.fabric;
    cleanup.push(async () => {
      await activeFabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1 },
      },
      commandId: `provider-review-admission-cut:${cut}:authority`,
    });
    const actionId = `provider-review-admission-cut:${cut}:spawn`;
    const commandId = `provider-review-admission-cut:${cut}:dispatch`;
    const injector = new Database(fixture.databasePath);
    try {
      if (cut === "action") {
        injector.exec(`
          CREATE TRIGGER provider_action_admission_fault_after_action
          AFTER INSERT ON provider_actions
          WHEN NEW.adapter_id='fake-lifecycle' AND NEW.action_id='${actionId}'
          BEGIN
            SELECT RAISE(ABORT, '${faultLabel}');
          END;
        `);
      } else if (cut === "command") {
        injector.exec(`
          CREATE TRIGGER provider_action_admission_fault_after_command
          AFTER INSERT ON commands
          WHEN NEW.run_id='${fixture.runId}' AND NEW.actor_agent_id='chair'
            AND NEW.command_id='${commandId}'
          BEGIN
            SELECT RAISE(ABORT, '${faultLabel}');
          END;
        `);
      } else {
        injector.exec(`
          CREATE TRIGGER provider_action_admission_fault_after_preflight_cas
          AFTER UPDATE OF state ON provider_action_pair_preflights
          WHEN OLD.state='resolving' AND NEW.state='admitted'
            AND NEW.adapter_id='fake-lifecycle' AND NEW.action_id='${actionId}'
          BEGIN
            SELECT RAISE(ABORT, '${faultLabel}');
          END;
        `);
      }
    } finally {
      injector.close();
    }

    const dispatchOutcome = await fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Crash the admission transaction before provider work.",
        cwd: "src/leader",
      },
      commandId,
    }).then(
      (value) => ({ status: "fulfilled" as const, value }),
      (error: unknown) => ({ status: "rejected" as const, error }),
    );
    expect(dispatchOutcome).toMatchObject({
      status: "rejected",
      error: { message: faultLabel },
    });

    const cleanupInjector = new Database(fixture.databasePath);
    try {
      cleanupInjector.exec(`DROP TRIGGER IF EXISTS ${cut === "action"
        ? "provider_action_admission_fault_after_action"
        : cut === "command"
          ? "provider_action_admission_fault_after_command"
          : "provider_action_admission_fault_after_preflight_cas"}`);
    } finally {
      cleanupInjector.close();
    }

    await activeFabric.close();
    activeFabric = await reopenLifecycleFabric(fixture);
    const verification = new Database(fixture.databasePath, { readonly: true });
    try {
      expect(verification.prepare(`
        SELECT
          (SELECT COUNT(*) FROM provider_actions
            WHERE adapter_id=? AND action_id=?) AS action_count,
          (SELECT COUNT(*) FROM provider_action_routes
            WHERE adapter_id=? AND action_id=?) AS route_count,
          (SELECT COUNT(*) FROM review_finding_capacity_reservations
            WHERE adapter_id=? AND action_id=?) AS reservation_count,
          (SELECT COUNT(*) FROM adapter_effective_configurations
            WHERE subject_action_adapter_id=? AND subject_action_id=?) AS configuration_count,
          (SELECT COUNT(*) FROM commands
            WHERE run_id=? AND actor_agent_id='chair' AND command_id=?) AS command_count,
          (SELECT COALESCE(SUM(reserved),0) FROM authority_budget
            WHERE authority_id=?) AS budget_reserved
      `).get(
        "fake-lifecycle", actionId,
        "fake-lifecycle", actionId,
        "fake-lifecycle", actionId,
        "fake-lifecycle", actionId,
        fixture.runId, commandId,
        reviewAuthority.authorityId,
      )).toEqual({
        action_count: 0,
        route_count: 0,
        reservation_count: 0,
        configuration_count: 0,
        command_count: 0,
        budget_reserved: 0,
      });
      expect(verification.prepare(`
        SELECT state FROM provider_action_pair_preflights
         WHERE adapter_id=? AND action_id=?
      `).get("fake-lifecycle", actionId)).toEqual({ state: "resolving" });
    } finally {
      verification.close();
    }
    const providerJournal = await readFile(fixture.providerJournalPath, "utf8").then(
      (bytes) => JSON.parse(bytes) as { actions?: Record<string, unknown> },
      (error: unknown) => {
        if (error instanceof Error && "code" in error && error.code === "ENOENT") return { actions: {} };
        throw error;
      },
    );
    expect(providerJournal.actions ?? {}).toEqual({});
  });

  it.each([
    ["gate", "GATE_BLOCKED"],
    ["quiesce", "LIFECYCLE_PRECONDITION_FAILED"],
    ["authority-expiry", "AUTHENTICATION_FAILED"],
    ["chair-handoff", "CAPABILITY_FORBIDDEN"],
  ] as const)("rechecks %s after delayed capabilities before provider dispatch", async (scenario, code) => {
    const fixture = await createLifecycleFixture({ capabilitiesDelayMs: 200 });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        ...(scenario === "authority-expiry"
          ? { expiresAt: new Date(fixture.clock.now().getTime() + 100).toISOString() }
          : {}),
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1 },
      },
      commandId: `provider-review-admission-race:${scenario}:authority`,
    });
    const actionId = `provider-review-admission-race:${scenario}:spawn`;
    const dispatch = fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Do not start after admission changes.",
        cwd: "src/leader",
      },
      commandId: `provider-review-admission-race:${scenario}:dispatch`,
    });
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 50));

    if (scenario === "authority-expiry") {
      fixture.clock.advance(200);
    } else {
      const database = new Database(fixture.databasePath);
      try {
        if (scenario === "quiesce") {
          database.prepare(`
            UPDATE runs SET lifecycle_state='quiescing',revision=revision+1 WHERE run_id=?
          `).run(fixture.runId);
        } else if (scenario === "chair-handoff") {
          database.prepare(`
            UPDATE runs SET chair_agent_id='leader',revision=revision+1 WHERE run_id=?
          `).run(fixture.runId);
        } else {
          const identity = database.prepare(`
            SELECT project_session_id,dependency_revision FROM runs WHERE run_id=?
          `).get(fixture.runId) as { project_session_id: string; dependency_revision: number };
          database.prepare(`
            INSERT INTO scoped_gates(
              gate_id,project_session_id,coordination_run_id,dedupe_key,scope_kind,
              scope_task_id,dependency_revision,blocked_operation_ids_json,
              enforcement_points_json,question,reason,options_json,recommendation,
              consequences_json,evidence_refs_json,created_by_ref,expected_approver_ref,
              status,human_required,revision,created_at,updated_at
            ) VALUES (?,?,?,'provider-review-admission-race','task',?,?,
                      '["fabric.v1.provider-action.dispatch"]','["operation"]','Proceed?',
                      'Admission changed','["approve","defer"]','defer','[]','[]',
                      'agent:chair','authenticated-human-operator','pending',1,1,1,1)
          `).run(
            `gate-provider-review-admission-race`,
            identity.project_session_id,
            fixture.runId,
            fixture.leaderTask.taskId,
            identity.dependency_revision,
          );
          database.prepare(`
            INSERT INTO scoped_gate_tasks(
              gate_id,project_session_id,run_id,task_id,binding_kind,bound_dependency_revision
            ) VALUES ('gate-provider-review-admission-race',?,?,?,'direct',?)
          `).run(
            identity.project_session_id,
            fixture.runId,
            fixture.leaderTask.taskId,
            identity.dependency_revision,
          );
          database.prepare(`
            INSERT INTO scoped_gate_operations(gate_id,operation_id)
            VALUES ('gate-provider-review-admission-race','fabric.v1.provider-action.dispatch')
          `).run();
        }
      } finally {
        database.close();
      }
    }

    await expect(dispatch).rejects.toMatchObject({ code });
    const verification = new Database(fixture.databasePath, { readonly: true });
    try {
      expect(verification.prepare(`
        SELECT 1 FROM provider_actions WHERE run_id=? AND action_id=?
      `).get(fixture.runId, actionId)).toBeUndefined();
    } finally {
      verification.close();
    }
  });

  it("fences delayed provider admission before closing Fabric", async () => {
    const fixture = await createLifecycleFixture({ capabilitiesDelayMs: 200 });
    cleanup.push(async () => rm(fixture.directory, { recursive: true, force: true }));
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1 },
      },
      commandId: "provider-review-close-race:authority",
    });
    const actionId = "provider-review-close-race:spawn";
    const dispatch = fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Do not start while Fabric closes.",
        cwd: "src/leader",
      },
      commandId: "provider-review-close-race:dispatch",
    });
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 50));
    const close = fixture.fabric.close();

    await expect(dispatch).rejects.toMatchObject({ code: "LIFECYCLE_PRECONDITION_FAILED" });
    await close;
    const database = new Database(fixture.databasePath, { readonly: true });
    try {
      expect(database.prepare(`SELECT 1 FROM provider_actions WHERE action_id=?`).get(actionId)).toBeUndefined();
    } finally {
      database.close();
    }
  });

  it("blocks task terminalisation while its provider action remains unresolved", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1 },
      },
      commandId: "provider-review-task-obligation:authority",
    });
    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "provider-review-task-obligation:spawn",
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Remain unresolved until lookup.",
        cwd: "src/leader",
        scenario: "ambiguous-review-valid",
      },
      commandId: "provider-review-task-obligation:dispatch",
    })).resolves.toMatchObject({ status: "prepared" });
    await expect(waitForProviderAction(fixture.chair, "provider-review-task-obligation:spawn"))
      .resolves.toMatchObject({ status: "ambiguous" });
    await expect(fixture.leader.updateTask({
      taskId: fixture.leaderTask.taskId,
      expectedRevision: fixture.leaderTask.revision,
      state: "complete",
      commandId: "provider-review-task-obligation:complete-early",
    })).rejects.toMatchObject({ code: "LIFECYCLE_PRECONDITION_FAILED" });
    await fixture.chair.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId: "provider-review-task-obligation:spawn",
      commandId: "provider-review-task-obligation:reconcile",
    });
    await expect(fixture.leader.updateTask({
      taskId: fixture.leaderTask.taskId,
      expectedRevision: fixture.leaderTask.revision,
      state: "complete",
      commandId: "provider-review-task-obligation:complete",
    })).resolves.toMatchObject({ state: "complete" });
  });
  it("atomically spends one delegated turn across concurrent ephemeral provider spawns", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1 },
      },
      commandId: "provider-review-concurrent:authority",
    });
    const dispatch = async (suffix: string) => await fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: `provider-review-concurrent:${suffix}`,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Review the current implementation read-only.",
        cwd: "src/leader",
      },
      commandId: `provider-review-concurrent:${suffix}:dispatch`,
    });

    const outcomes = await Promise.allSettled([dispatch("one"), dispatch("two")]);
    expect(outcomes.filter((outcome) => outcome.status === "fulfilled")).toHaveLength(1);
    const rejected = outcomes.find((outcome) => outcome.status === "rejected");
    expect(rejected).toMatchObject({ status: "rejected", reason: { code: "BUDGET_EXCEEDED" } });
    const database = new Database(fixture.databasePath, { readonly: true });
    try {
      expect(database.prepare(`
        SELECT state FROM provider_action_pair_preflights
         WHERE adapter_id='fake-lifecycle'
           AND action_id IN ('provider-review-concurrent:one','provider-review-concurrent:two')
         ORDER BY state
      `).all()).toEqual([{ state: "admitted" }, { state: "resolving" }]);
    } finally {
      database.close();
    }
  });

  it("queues answer-bearing work within the shared provider-turn ceiling", async () => {
    const fixture = await createLifecycleFixture({
      maximumConcurrentProviderTurns: 1,
      spawnBarrier: true,
    });
    cleanup.push(async () => {
      await fixture.providerSpawnBarrier?.release();
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 2 },
      },
      commandId: "provider-review-queue:authority",
    });
    const dispatch = async (suffix: string) => await fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: `provider-review-queue:${suffix}`,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: `Queued review ${suffix}.`,
        cwd: "src/leader",
      },
      commandId: `provider-review-queue:${suffix}:dispatch`,
    });

    await expect(dispatch("one")).resolves.toMatchObject({ status: "prepared", executionCount: 0 });
    await expect(dispatch("two")).resolves.toMatchObject({ status: "prepared", executionCount: 0 });
    const spawnBarrier = fixture.providerSpawnBarrier;
    if (spawnBarrier === undefined) throw new Error("provider spawn barrier is required");
    await spawnBarrier.waitUntilEntered();
    const database = new Database(fixture.databasePath, { readonly: true });
    try {
      expect(database.prepare(`
        SELECT action_id,status FROM provider_actions
         WHERE action_id IN ('provider-review-queue:one','provider-review-queue:two')
         ORDER BY action_id
      `).all()).toEqual([
        { action_id: "provider-review-queue:one", status: "dispatched" },
        { action_id: "provider-review-queue:two", status: "prepared" },
      ]);
    } finally {
      database.close();
    }
    const blockedJournal = JSON.parse(await readFile(fixture.providerJournalPath, "utf8")) as {
      actions: Record<string, { executionCount: number; effectCount: number }>;
    };
    expect(Object.keys(blockedJournal.actions)).toEqual(["provider-review-queue:one"]);
    expect(blockedJournal.actions["provider-review-queue:one"])
      .toMatchObject({ executionCount: 1, effectCount: 1 });
    await expect(fixture.leader.updateTask({
      taskId: fixture.leaderTask.taskId,
      expectedRevision: fixture.leaderTask.revision,
      state: "complete",
      commandId: "provider-review-queue:complete-early",
    })).rejects.toMatchObject({ code: "LIFECYCLE_PRECONDITION_FAILED" });

    await spawnBarrier.release();
    const firstSettled = await waitForProviderAction(fixture.chair, "provider-review-queue:one");
    const ownerDatabase = new Database(fixture.databasePath, { readonly: true });
    try {
      expect(classifyProviderActionOwner(ownerDatabase, {
        runId: fixture.runId,
        adapterId: "fake-lifecycle",
        actionId: "provider-review-queue:one",
      })).toBe("generic");
    } finally {
      ownerDatabase.close();
    }
    expect(firstSettled).toMatchObject({ status: "terminal", executionCount: 1, effectCount: 1 });
    await expect(waitForProviderAction(fixture.chair, "provider-review-queue:two"))
      .resolves.toMatchObject({ status: "terminal", executionCount: 1, effectCount: 1 });
  });

  it("keeps ambiguous answer-bearing work inside the shared provider-turn ceiling", async () => {
    const fixture = await createLifecycleFixture({ maximumConcurrentProviderTurns: 1 });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1 },
      },
      commandId: "provider-review-ambiguous-cap:authority",
    });
    const secondAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1 },
      },
      commandId: "provider-review-ambiguous-cap:second-authority",
    });
    const dispatch = async (suffix: string, authorityId: string, scenario?: string) => await fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: `provider-review-ambiguous-cap:${suffix}`,
      operation: "spawn",
      authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: `Ambiguous capacity review ${suffix}.`,
        cwd: "src/leader",
        ...(scenario === undefined ? {} : { scenario }),
      },
      commandId: `provider-review-ambiguous-cap:${suffix}:dispatch`,
    });

    await expect(dispatch("one", reviewAuthority.authorityId, "ambiguous-review-valid"))
      .resolves.toMatchObject({ status: "prepared" });
    await expect(waitForProviderAction(fixture.chair, "provider-review-ambiguous-cap:one"))
      .resolves.toMatchObject({ status: "ambiguous" });
    await expect(dispatch("two", secondAuthority.authorityId)).resolves.toMatchObject({ status: "prepared" });
    await settle(150, "the second review never executes while the first holds the capability");
    await expect(readProviderAction(fixture.chair, "provider-review-ambiguous-cap:two"))
      .resolves.toMatchObject({ status: "prepared", executionCount: 0 });

    await expect(fixture.chair.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId: "provider-review-ambiguous-cap:one",
      commandId: "provider-review-ambiguous-cap:one:reconcile",
    })).resolves.toMatchObject({ status: "terminal" });
    await expect(waitForProviderAction(fixture.chair, "provider-review-ambiguous-cap:two"))
      .resolves.toMatchObject({ status: "terminal" });
  });

  it.each(["ambiguous", "quarantined"] as const)(
    "closes around a %s capacity holder without executing durable queued work",
    async (holderStatus) => {
      const fixture = await createLifecycleFixture({
        maximumConcurrentProviderTurns: 2,
        spawnDelayMs: 500,
      });
      let closed = false;
      cleanup.push(async () => {
        if (!closed) await fixture.fabric.close();
        await rm(fixture.directory, { recursive: true, force: true });
      });
      const delegateReviewAuthority = async (suffix: string) => await fixture.chair.delegateAuthority({
        parentAuthorityId: fixture.chairAuthorityId,
        authority: {
          ...fixture.rootAuthority,
          sourcePaths: ["src/leader"],
          actions: [...fixture.rootAuthority.actions],
          budget: { turns: 1 },
        },
        commandId: `provider-review-close-queue:${holderStatus}:${suffix}:authority`,
      });
      const [holderAuthority, claimedAuthority, queuedAuthority] = await Promise.all([
        delegateReviewAuthority("holder"),
        delegateReviewAuthority("claimed"),
        delegateReviewAuthority("queued"),
      ]);
      const actionId = (suffix: string): string =>
        `provider-review-close-queue:${holderStatus}:${suffix}`;
      const dispatch = async (
        suffix: string,
        authorityId: string,
        scenario?: string,
      ) => await fixture.chair.dispatchProviderAction({
        certifyingReview: null,
        adapterId: "fake-lifecycle",
        actionId: actionId(suffix),
        operation: "spawn",
        authorityId,
        taskId: fixture.leaderTask.taskId,
        payload: {
          model: "fake-reviewer-v1",
          modelFamily: "fake",
          prompt: `Close queue review ${suffix}.`,
          cwd: "src/leader",
          ...(scenario === undefined ? {} : { scenario }),
        },
        commandId: `${actionId(suffix)}:dispatch`,
      });

      await expect(dispatch(
        "holder",
        holderAuthority.authorityId,
        holderStatus === "ambiguous" ? "ambiguous-review-valid" : "ambiguous-review-empty",
      )).resolves.toMatchObject({ status: "prepared" });
      await expect(waitForProviderAction(fixture.chair, actionId("holder")))
        .resolves.toMatchObject({ status: "ambiguous" });
      if (holderStatus === "quarantined") {
        await expect(fixture.chair.reconcileProviderAction({
          adapterId: "fake-lifecycle",
          actionId: actionId("holder"),
          commandId: `${actionId("holder")}:reconcile`,
        })).resolves.toMatchObject({ status: "quarantined" });
      }

      await expect(dispatch("claimed", claimedAuthority.authorityId))
        .resolves.toMatchObject({ status: "prepared" });
      await expect(dispatch("queued", queuedAuthority.authorityId))
        .resolves.toMatchObject({ status: "prepared" });
      await expect(readProviderAction(fixture.chair, actionId("claimed")))
        .resolves.toMatchObject({ status: "dispatched", executionCount: 1 });
      await expect(readProviderAction(fixture.chair, actionId("queued")))
        .resolves.toMatchObject({ status: "prepared", executionCount: 0 });

      const closing = fixture.fabric.close();
      let rescueTimer: ReturnType<typeof setTimeout> | undefined;
      const rescued = new Promise<"rescued">((resolvePromise, rejectPromise) => {
        rescueTimer = setTimeout(() => {
          try {
            const database = new Database(fixture.databasePath);
            try {
              database.prepare(`
                UPDATE provider_actions
                   SET status='terminal',history_json='["prepared","dispatched","terminal"]',
                       updated_at=?
                 WHERE run_id=? AND action_id=?
              `).run(fixture.clock.now().getTime(), fixture.runId, actionId("holder"));
            } finally {
              database.close();
            }
          } catch (error: unknown) {
            rejectPromise(error);
            return;
          }
          void closing.then(() => resolvePromise("rescued"), rejectPromise);
        }, 2_000);
      });
      const closeOutcome = await Promise.race([
        closing.then(() => "closed" as const),
        rescued,
      ]);
      if (rescueTimer !== undefined) clearTimeout(rescueTimer);
      closed = true;

      expect(closeOutcome).toBe("closed");
      const database = new Database(fixture.databasePath, { readonly: true });
      try {
        expect(database.prepare(`
          SELECT action_id,status,execution_count,effect_count
            FROM provider_actions
           WHERE action_id IN (?,?,?)
           ORDER BY action_id
        `).all(actionId("claimed"), actionId("holder"), actionId("queued"))).toEqual([
          {
            action_id: actionId("claimed"),
            status: "terminal",
            execution_count: 1,
            effect_count: 1,
          },
          {
            action_id: actionId("holder"),
            status: holderStatus,
            execution_count: 1,
            effect_count: 0,
          },
          {
            action_id: actionId("queued"),
            status: "prepared",
            execution_count: 0,
            effect_count: 0,
          },
        ]);
      } finally {
        database.close();
      }
    },
    10_000,
  );

  it("wakes queued review work after out-of-band turn-lease release", async () => {
    const fixture = await createLifecycleFixture({ maximumConcurrentProviderTurns: 1 });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const now = fixture.clock.now().getTime();
    const database = new Database(fixture.databasePath);
    try {
      admitProviderActionFixture(database, {
        runId: fixture.runId,
        actionId: "provider-review-capacity-sentinel",
        adapterId: "fake-lifecycle",
        operation: "send_turn",
        targetAgentId: "leader",
        providerSessionGeneration: 1,
        turnLeaseGeneration: 1,
        identityHash: "a".repeat(64),
        payloadHash: "b".repeat(64),
        payloadJson: `{"taskId":"${fixture.leaderTask.taskId}"}`,
        status: "dispatched",
        historyJson: '["prepared","dispatched"]',
        executionCount: 1,
        updatedAt: now,
      });
      database.exec(`
        INSERT INTO provider_session_turn_leases(
          run_id,agent_id,provider_session_generation,turn_lease_generation,
          adapter_id,action_id,status,created_at,updated_at
        ) VALUES (
          '${fixture.runId}','leader',1,1,'fake-lifecycle','provider-review-capacity-sentinel','active',${now},${now}
        );
      `);
    } finally {
      database.close();
    }
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1 },
      },
      commandId: "provider-review-external-capacity:authority",
    });
    const actionId = "provider-review-external-capacity:spawn";
    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Start after external turn capacity is released.",
        cwd: "src/leader",
      },
      commandId: "provider-review-external-capacity:dispatch",
    })).resolves.toMatchObject({ status: "prepared" });
    await settle(150, "the review never executes while external turn capacity is held");
    await expect(readProviderAction(fixture.chair, actionId))
      .resolves.toMatchObject({ status: "prepared", executionCount: 0 });

    const release = new Database(fixture.databasePath);
    try {
      release.transaction(() => {
        release.prepare(`
          UPDATE provider_session_turn_leases SET status='released',updated_at=?
           WHERE run_id=? AND action_id=?
        `).run(now + 1, fixture.runId, "provider-review-capacity-sentinel");
        release.prepare(`
          UPDATE provider_actions SET status='terminal',history_json='["prepared","dispatched","terminal"]',
                 effect_count=1,updated_at=? WHERE run_id=? AND action_id=?
        `).run(now + 1, fixture.runId, "provider-review-capacity-sentinel");
      })();
    } finally {
      release.close();
    }
    await expect(waitForProviderAction(fixture.chair, actionId))
      .resolves.toMatchObject({ status: "terminal", executionCount: 1 });
  });

  it("retains one turn reservation through restart and settles it from terminal lookup evidence", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => rm(fixture.directory, { recursive: true, force: true }));
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1 },
      },
      commandId: "provider-review-restart-budget:authority",
    });
    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "provider-review-restart-budget:ambiguous",
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Review the current implementation read-only.",
        cwd: "src/leader",
        scenario: "ambiguous-review-valid",
      },
      commandId: "provider-review-restart-budget:dispatch",
    })).resolves.toMatchObject({ status: "prepared" });
    await expect(waitForProviderAction(fixture.chair, "provider-review-restart-budget:ambiguous"))
      .resolves.toMatchObject({ status: "ambiguous" });

    await fixture.fabric.close();
    const reopened = await reopenLifecycleFabric(fixture);
    cleanup.push(async () => reopened.close());
    const chair = asLifecycleClient(reopened.connect(fixture.capabilities.chair));
    await expect(chair.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId: "provider-review-restart-budget:ambiguous",
      commandId: "provider-review-restart-budget:reconcile",
    })).resolves.toMatchObject({ status: "terminal", providerAnswer: "recovered provider review" });
    await expect(chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "provider-review-restart-budget:second",
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "A spent turn cannot be reused.",
        cwd: "src/leader",
      },
      commandId: "provider-review-restart-budget:second:dispatch",
    })).rejects.toMatchObject({ code: "BUDGET_EXCEEDED" });
  });

  it("rejects a requested turn ceiling that exceeds delegated capacity before provider work", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 3 },
      },
      commandId: "provider-review-exhausted:authority",
    });

    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "provider-review-exhausted:spawn",
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Two turns exceed this delegated review authority.",
        maxTurns: 2,
        cwd: "src/leader",
      },
      commandId: "provider-review-exhausted:dispatch",
    })).rejects.toMatchObject({ code: "CAPABILITY_UNAVAILABLE" });
    await expect(readProviderAction(fixture.chair, "provider-review-exhausted:spawn"))
      .rejects.toMatchObject({ code: "NOT_FOUND" });
  });

  it("freezes further turns when ambiguous provider usage cannot be validated", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 2 },
      },
      commandId: "provider-review-unknown:authority",
    });
    const action = "provider-review-unknown:ambiguous";
    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: action,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "An invalid terminal answer leaves usage unprovable.",
        cwd: "src/leader",
        scenario: "ambiguous-review-empty",
      },
      commandId: "provider-review-unknown:dispatch",
    })).resolves.toMatchObject({ status: "prepared" });
    await expect(waitForProviderAction(fixture.chair, action)).resolves.toMatchObject({ status: "ambiguous" });
    await expect(fixture.chair.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId: action,
      commandId: "provider-review-unknown:reconcile",
    })).resolves.toMatchObject({ status: "quarantined" });
    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "provider-review-unknown:second",
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Unknown usage must fail closed.",
        cwd: "src/leader",
      },
      commandId: "provider-review-unknown:second:dispatch",
    })).rejects.toMatchObject({ code: "BUDGET_USAGE_UNKNOWN" });
  });

  it("rejects an explicitly named terminal task before ephemeral provider I/O", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1 },
      },
      commandId: "provider-review-terminal-task:authority",
    });
    await fixture.leader.updateTask({
      taskId: fixture.leaderTask.taskId,
      expectedRevision: fixture.leaderTask.revision,
      state: "complete",
      commandId: "provider-review-terminal-task:complete",
    });

    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "provider-review-terminal-task:spawn",
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "This terminal task must not start provider work.",
        cwd: "src/leader",
      },
      commandId: "provider-review-terminal-task:dispatch",
    })).rejects.toMatchObject({ code: "LIFECYCLE_PRECONDITION_FAILED" });
    await expect(readProviderAction(fixture.chair, "provider-review-terminal-task:spawn"))
      .rejects.toMatchObject({ code: "NOT_FOUND" });
  });

  it("runs a task-bound ephemeral provider spawn without creating an agent identity", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 1, "cost:USD": 1 },
      },
      commandId: "provider-review:authority",
    });
    const before = await fixture.chair.getRunStatus({ runId: fixture.runId });
    const dispatchReceipt = await fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "provider-review:spawn",
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Review the current implementation read-only.",
        cwd: "src/leader",
      },
      commandId: "provider-review:dispatch",
    });

    expect(dispatchReceipt).toMatchObject({
      actionId: "provider-review:spawn",
      status: "prepared",
      executionCount: 0,
      effectCount: 0,
    });
    const result = await waitForProviderAction(fixture.chair, "provider-review:spawn");
    expect(result).toMatchObject({
      actionId: "provider-review:spawn",
      status: "terminal",
      executionCount: 1,
      effectCount: 1,
      result: { resumeReference: "new:replacement:g1", generation: 1, result: "fake provider review complete" },
      providerAnswer: "fake provider review complete",
    });
    expect((await fixture.chair.getRunStatus({ runId: fixture.runId })).counts.agents).toBe(before.counts.agents);
    expect(await readProviderAction(fixture.chair, "provider-review:spawn")).toEqual(result);
    expect(authorityBudget(fixture.databasePath, reviewAuthority.authorityId)).toMatchObject({
      turns: { reserved: 0, consumed: 1, usageUnknown: false },
      "cost:USD": { reserved: 1, consumed: 0, usageUnknown: true },
    });
    await expect(fixture.chair.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId: "provider-review:spawn",
      commandId: "provider-review:late-usage-unavailable",
    })).resolves.toEqual(result);
    expect(await readProviderAction(fixture.chair, "provider-review:spawn")).toEqual(result);
    await fixture.leader.updateTask({
      taskId: fixture.leaderTask.taskId,
      expectedRevision: fixture.leaderTask.revision,
      state: "complete",
      commandId: "provider-review:complete-task-before-replay",
    });
    const lifecycleDatabase = new Database(fixture.databasePath);
    try {
      lifecycleDatabase.prepare(`
        UPDATE runs SET lifecycle_state='quiescing',revision=revision+1 WHERE run_id=?
      `).run(fixture.runId);
    } finally {
      lifecycleDatabase.close();
    }
    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "provider-review:spawn",
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Review the current implementation read-only.",
        cwd: "src/leader",
      },
      commandId: "provider-review:dispatch-replay",
    })).resolves.toEqual(result);
    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "provider-review:spawn",
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Changed identity must not replay.",
        cwd: "src/leader",
      },
      commandId: "provider-review:dispatch-conflict-after-quiesce",
    })).rejects.toMatchObject({ code: "ACTION_INPUT_CONFLICT" });

    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "provider-review:missing-task",
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Missing task must fail before provider dispatch.",
      },
      commandId: "provider-review:missing-task",
    } as unknown as Parameters<FabricClient["dispatchProviderAction"]>[0]))
      .rejects.toMatchObject({ code: "PROTOCOL_INVALID" });
  });

  it("recovers only a validated answer from terminal adapter evidence", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 3, "cost:USD": 3 },
      },
      commandId: "provider-review-recovery:authority",
    });
    const dispatch = async (scenario: string, authorityId = reviewAuthority.authorityId) => await fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: `provider-review-recovery:${scenario}`,
      operation: "spawn",
      authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Review the current implementation read-only.",
        cwd: "src/leader",
        scenario,
      },
      commandId: `provider-review-recovery:${scenario}:dispatch`,
    });

    await expect(dispatch("ambiguous-review-valid")).resolves.toMatchObject({ status: "prepared" });
    await expect(waitForProviderAction(fixture.chair, "provider-review-recovery:ambiguous-review-valid"))
      .resolves.toMatchObject({ status: "ambiguous" });
    await expect(fixture.chair.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId: "provider-review-recovery:ambiguous-review-valid",
      commandId: "provider-review-recovery:valid:reconcile",
    })).resolves.toMatchObject({
      status: "terminal",
      providerAnswer: "recovered provider review",
    });

    for (const scenario of [
      "ambiguous-review-empty",
      "ambiguous-review-oversized",
      "ambiguous-review-wrong-action-id",
    ] as const) {
      const invalidAuthority = await fixture.chair.delegateAuthority({
        parentAuthorityId: fixture.chairAuthorityId,
        authority: {
          ...fixture.rootAuthority,
          sourcePaths: ["src/leader"],
          actions: [...fixture.rootAuthority.actions],
          budget: { turns: 1 },
        },
        commandId: `provider-review-recovery:${scenario}:authority`,
      });
      await expect(dispatch(scenario, invalidAuthority.authorityId)).resolves.toMatchObject({ status: "prepared" });
      await expect(waitForProviderAction(fixture.chair, `provider-review-recovery:${scenario}`))
        .resolves.toMatchObject({ status: "ambiguous" });
      await expect(fixture.chair.reconcileProviderAction({
        adapterId: "fake-lifecycle",
        actionId: `provider-review-recovery:${scenario}`,
        commandId: `provider-review-recovery:${scenario}:reconcile`,
      })).resolves.toMatchObject({ status: "quarantined" });
      expect(await readProviderAction(fixture.chair, `provider-review-recovery:${scenario}`))
        .toMatchObject({ status: "quarantined" });
    }
  });

  it("singleflights concurrent reconciliation commands so divergent lookup evidence cannot rewrite terminal custody", async () => {
    const fixture = await createLifecycleFixture({ payloadMaxTurns: true });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        actions: [...fixture.rootAuthority.actions],
        budget: { turns: 2, "cost:USD": 2 },
      },
      commandId: "provider-review-concurrent:authority",
    });
    const actionId = "provider-review-concurrent:spawn";
    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId,
      operation: "spawn",
      authorityId: reviewAuthority.authorityId,
      taskId: fixture.leaderTask.taskId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: "Recover one immutable answer.",
        maxTurns: 2,
        cwd: "src/leader",
        scenario: "ambiguous-review-concurrent-divergent",
      },
      commandId: "provider-review-concurrent:dispatch",
    })).resolves.toMatchObject({ status: "prepared" });
    await expect(waitForProviderAction(fixture.chair, actionId)).resolves.toMatchObject({ status: "ambiguous" });

    const proxy = asLifecycleClient(fixture.fabric.connect(fixture.capabilities.chair));
    const firstPromise = fixture.chair.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId,
      commandId: "provider-review-concurrent:reconcile:first",
    });
    await new Promise((resolvePromise) => setTimeout(resolvePromise, 10));
    const secondPromise = proxy.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId,
      commandId: "provider-review-concurrent:reconcile:second",
    });
    const [first, second] = await Promise.all([firstPromise, secondPromise]);

    expect(first).toEqual(second);
    expect(first).toMatchObject({ status: "terminal", providerAnswer: "recovered provider review" });
    expect(await readProviderAction(fixture.chair, actionId)).toEqual(first);
    await expect(fixture.chair.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId,
      commandId: "provider-review-concurrent:reconcile:first",
    })).resolves.toEqual(first);
    await expect(proxy.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId,
      commandId: "provider-review-concurrent:reconcile:second",
    })).resolves.toEqual(first);

    const providerJournal = JSON.parse(await readFile(fixture.providerJournalPath, "utf8")) as {
      actions: Record<string, { lookupCount?: number }>;
    };
    expect(providerJournal.actions[actionId]?.lookupCount).toBe(1);
    expect(authorityBudget(fixture.databasePath, reviewAuthority.authorityId)).toMatchObject({
      turns: { granted: 2, reserved: 0, consumed: 1, usageUnknown: false },
      "cost:USD": { granted: 2, reserved: 2, consumed: 0, usageUnknown: true },
    });
    const database = new Database(fixture.databasePath, { readonly: true });
    try {
      const receipts = database.prepare(`
        SELECT result_json FROM commands
         WHERE run_id=? AND actor_agent_id='chair' AND command_id LIKE 'provider-review-concurrent:reconcile:%'
         ORDER BY command_id
      `).all(fixture.runId) as Array<{ result_json: string }>;
      expect(receipts).toHaveLength(2);
      expect(receipts[0]?.result_json).toBe(receipts[1]?.result_json);
      expect(JSON.parse(receipts[0]?.result_json ?? "null")).toEqual(first);
    } finally {
      database.close();
    }
  });

  it("persists prepared, dispatched, accepted and terminal states across a core restart", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => rm(fixture.directory, { recursive: true, force: true }));
    const terminal = await fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "action-terminal",
      operation: "send_turn",
      payload: { scenario: "terminal", taskId: fixture.leaderTask.taskId },
      commandId: "provider-action:terminal:dispatch",
    });
    expect(terminal).toMatchObject({
      actionId: "action-terminal",
      status: "terminal",
      history: ["prepared", "dispatched", "accepted", "terminal"],
      executionCount: 1,
      effectCount: 1,
    });

    await fixture.fabric.close();
    const reopened = await reopenLifecycleFabric(fixture);
    cleanup.push(async () => reopened.close());
    const chair = asLifecycleClient(reopened.connect(fixture.capabilities.chair));
    expect(await readProviderAction(chair, "action-terminal")).toEqual(terminal);
  });

  it("keeps certifying-review and generic restart recovery with their canonical owners", async () => {
    const fixture = await createLifecycleFixture({ secondaryAdapter: true });
    cleanup.push(async () => rm(fixture.directory, { recursive: true, force: true }));
    const secondaryAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader/child"],
        budget: { turns: 4, provider_calls: 4, concurrent_turns: 1 },
      },
      commandId: "restart-owner:secondary-authority",
    });
    const secondaryRegistration = await fixture.chair.registerAgent({
      agentId: "restart-owner-secondary",
      authorityId: secondaryAuthority.authorityId,
      providerSessionRef: "fake-session:restart-owner-secondary:g1",
      adapterId: "fake-lifecycle-secondary",
    });
    const secondaryClient = asLifecycleClient(fixture.fabric.connect(secondaryRegistration.capability));
    const secondaryTaskReady = await fixture.chair.createTask({
      taskId: "restart-owner-secondary-task",
      authorityId: secondaryAuthority.authorityId,
      eligibleAgentIds: ["restart-owner-secondary"],
      objective: "retain a generic pending provider action across restart",
      baseRevision: "stage3-base",
      commandId: "restart-owner:secondary-task-create",
    });
    const secondaryTask = await secondaryClient.claimTask({
      taskId: secondaryTaskReady.taskId,
      expectedRevision: secondaryTaskReady.revision,
      commandId: "restart-owner:secondary-task-claim",
    });
    const actions = [
      {
        adapterId: "fake-lifecycle",
        actionId: "restart-owner:certifying",
        taskId: fixture.leaderTask.taskId,
      },
      {
        adapterId: "fake-lifecycle-secondary",
        actionId: "restart-owner:generic",
        taskId: secondaryTask.taskId,
      },
    ] as const;
    for (const { adapterId, actionId, taskId } of actions) {
      await expect(fixture.chair.dispatchProviderAction({
        certifyingReview: null,
        adapterId,
        actionId,
        operation: "send_turn",
        payload: { scenario: "ambiguous-unproven", taskId },
        commandId: `${actionId}:dispatch`,
      })).resolves.toMatchObject({ status: "ambiguous", executionCount: 1, effectCount: 1 });
    }
    await fixture.fabric.close();

    bindCertifyingReviewOwner({
      databasePath: fixture.databasePath,
      runId: fixture.runId,
      adapterId: actions[0].adapterId,
      actionId: actions[0].actionId,
      taskId: fixture.leaderTask.taskId,
    });
    const database = new Database(fixture.databasePath);
    expect(classifyProviderActionOwner(database, {
      runId: fixture.runId,
      adapterId: "fake-lifecycle",
      actionId: actions[0].actionId,
    })).toBe("certifying_review");
    const before = database.prepare(`
      SELECT adapter_id,action_id,status,execution_count,effect_count FROM provider_actions
       WHERE (adapter_id=? AND action_id=?) OR (adapter_id=? AND action_id=?)
       ORDER BY adapter_id,action_id
    `).all(
      actions[0].adapterId,
      actions[0].actionId,
      actions[1].adapterId,
      actions[1].actionId,
    );
    database.close();

    const restarted = await reopenLifecycleFabric(fixture);
    cleanup.push(async () => restarted.close());
    await expect(restarted.recoverStartupState()).resolves.toMatchObject({ actionsReconciled: 0, actionsQuarantined: 2 });
    const restartedChair = asLifecycleClient(restarted.connect(fixture.capabilities.chair));
    await expect(restartedChair.reconcileProviderAction({
      adapterId: actions[0].adapterId,
      actionId: actions[0].actionId,
      commandId: "restart-owner:certifying:generic-reconcile",
    })).rejects.toMatchObject({
      name: "ProviderActionOwnerError",
      expectedOwner: "generic",
      actualOwner: "certifying_review",
    });
    await expect(restartedChair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: actions[0].adapterId,
      actionId: actions[0].actionId,
      operation: "send_turn",
      payload: { scenario: "ambiguous-unproven", taskId: fixture.leaderTask.taskId },
      commandId: "restart-owner:certifying:generic-dispatch",
    })).rejects.toMatchObject({
      name: "ProviderActionOwnerError",
      expectedOwner: "generic",
      actualOwner: "certifying_review",
    });
    const observed = new Database(fixture.databasePath, { readonly: true });
    try {
      const after = observed.prepare(`
        SELECT adapter_id,action_id,status,execution_count,effect_count FROM provider_actions
         WHERE (adapter_id=? AND action_id=?) OR (adapter_id=? AND action_id=?)
         ORDER BY adapter_id,action_id
      `).all(
        actions[0].adapterId,
        actions[0].actionId,
        actions[1].adapterId,
        actions[1].actionId,
      ) as Array<{ adapter_id: string; action_id: string; status: string; execution_count: number; effect_count: number }>;
      expect(after.map(({ execution_count, effect_count }) => ({ execution_count, effect_count })))
        .toEqual((before as typeof after).map(({ execution_count, effect_count }) => ({ execution_count, effect_count })));
      expect(after.every(({ status }) => status === "quarantined")).toBe(true);
      const commands = observed.prepare(`
        SELECT command_id FROM commands
         WHERE command_id LIKE 'certifying-review-recovery:%' OR command_id LIKE 'startup-recovery:%'
         ORDER BY command_id
      `).pluck().all() as string[];
      expect(commands).toHaveLength(2);
      expect(commands.find((command) => command.startsWith("certifying-review-recovery:"))).toContain(actions[0].actionId);
      expect(commands.find((command) => command.startsWith("startup-recovery:"))).toContain(actions[1].actionId);
      expect(commands.find((command) => command.startsWith("startup-recovery:"))).not.toContain(actions[0].actionId);
      expect(observed.prepare(`
        SELECT COUNT(*) AS count FROM commands
         WHERE command_id IN ('restart-owner:certifying:generic-reconcile','restart-owner:certifying:generic-dispatch')
      `).get()).toEqual({ count: 0 });
    } finally {
      observed.close();
    }
  });

  it("quarantines a malformed owner record at startup and continues recovering valid records", async () => {
    const fixture = await createLifecycleFixture({ secondaryAdapter: true });
    cleanup.push(async () => rm(fixture.directory, { recursive: true, force: true }));
    const secondaryAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader/child"],
        budget: { turns: 2, provider_calls: 2, concurrent_turns: 1 },
      },
      commandId: "restart-owner-quarantine:secondary-authority",
    });
    const secondaryRegistration = await fixture.chair.registerAgent({
      agentId: "restart-owner-quarantine-secondary",
      authorityId: secondaryAuthority.authorityId,
      providerSessionRef: "fake-session:restart-owner-quarantine-secondary:g1",
      adapterId: "fake-lifecycle-secondary",
    });
    const secondaryClient = asLifecycleClient(fixture.fabric.connect(secondaryRegistration.capability));
    const secondaryTaskReady = await fixture.chair.createTask({
      taskId: "restart-owner-quarantine-secondary-task",
      authorityId: secondaryAuthority.authorityId,
      eligibleAgentIds: ["restart-owner-quarantine-secondary"],
      objective: "retain a valid pending provider action across restart",
      baseRevision: "stage3-base",
      commandId: "restart-owner-quarantine:secondary-task-create",
    });
    const secondaryTask = await secondaryClient.claimTask({
      taskId: secondaryTaskReady.taskId,
      expectedRevision: secondaryTaskReady.revision,
      commandId: "restart-owner-quarantine:secondary-task-claim",
    });
    const reviewAuthority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        budget: { turns: 1, provider_calls: 1, concurrent_turns: 1 },
      },
      commandId: "restart-owner-quarantine:review-authority",
    });
    const actions = [
      {
        adapterId: "fake-lifecycle",
        actionId: "restart-owner:malformed",
        taskId: fixture.leaderTask.taskId,
      },
      {
        adapterId: "fake-lifecycle-secondary",
        actionId: "restart-owner:valid",
        taskId: secondaryTask.taskId,
      },
    ] as const;
    await expect(fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: actions[1].adapterId,
      actionId: actions[1].actionId,
      operation: "send_turn",
      payload: { scenario: "ambiguous-unproven", taskId: actions[1].taskId },
      commandId: `${actions[1].actionId}:dispatch`,
    })).resolves.toMatchObject({ status: "ambiguous", executionCount: 1, effectCount: 1 });
    await fixture.fabric.close();

    const malformedReservationDigest = `sha256:${"f".repeat(64)}`;
    const database = new Database(fixture.databasePath);
    admitProviderActionFixture(database, {
      runId: fixture.runId,
      adapterId: actions[0].adapterId,
      actionId: actions[0].actionId,
      operation: "spawn",
      identityHash: "restart-owner-malformed-identity",
      payloadHash: "restart-owner-malformed-payload",
      payloadJson: '{"model":"fake-reviewer-v1","maxTurns":1}',
      status: "prepared",
      historyJson: '["prepared"]',
      executionCount: 0,
      effectCount: 0,
      taskId: actions[0].taskId,
      budgetAuthorityId: reviewAuthority.authorityId,
      budgetReservationJson: '{"concurrent_turns":1,"provider_calls":1,"turns":1}',
      budgetState: "reserved",
      budgetStartedAt: 1,
      updatedAt: 1,
    });
    database.pragma("foreign_keys = OFF");
    database.prepare(`
      UPDATE provider_actions SET finding_capacity_reservation_digest=?,history_json='malformed'
       WHERE adapter_id=? AND action_id=?
    `).run(malformedReservationDigest, actions[0].adapterId, actions[0].actionId);
    database.close();

    const restarted = await reopenLifecycleFabric(fixture);
    cleanup.push(async () => restarted.close());
    await expect(restarted.recoverStartupState()).resolves.toMatchObject({
      actionsReconciled: 0,
      actionsQuarantined: 2,
    });
    await expect(restarted.recoverStartupState()).resolves.toMatchObject({
      actionsReconciled: 0,
      actionsQuarantined: 0,
    });

    const observed = new Database(fixture.databasePath, { readonly: true });
    try {
      expect(observed.prepare(`
        SELECT adapter_id,action_id,status,budget_state,budget_settlement_json FROM provider_actions
         WHERE (adapter_id=? AND action_id=?) OR (adapter_id=? AND action_id=?)
         ORDER BY adapter_id,action_id
      `).all(
        actions[0].adapterId,
        actions[0].actionId,
        actions[1].adapterId,
        actions[1].actionId,
      )).toStrictEqual([
        {
          adapter_id: actions[0].adapterId,
          action_id: actions[0].actionId,
          status: "quarantined",
          budget_state: "usage-unknown",
          budget_settlement_json: '{"concurrent_turns":"unknown","provider_calls":"unknown","turns":"unknown"}',
        },
        {
          adapter_id: actions[1].adapterId,
          action_id: actions[1].actionId,
          status: "quarantined",
          budget_state: null,
          budget_settlement_json: null,
        },
      ]);
      expect(authorityBudget(fixture.databasePath, reviewAuthority.authorityId)).toMatchObject({
        turns: { reserved: 1, consumed: 0, usageUnknown: true },
        provider_calls: { reserved: 1, consumed: 0, usageUnknown: true },
        concurrent_turns: { reserved: 1, consumed: 0, usageUnknown: true },
      });
      expect(observed.prepare(`
        SELECT history_json,idempotency_proven,result_json,journal_revision
          FROM provider_actions WHERE adapter_id=? AND action_id=?
      `).get(actions[0].adapterId, actions[0].actionId)).toStrictEqual({
        history_json: '["quarantined"]',
        idempotency_proven: 0,
        result_json: null,
        journal_revision: 2,
      });
      const recoveryCommands = observed.prepare(`
        SELECT command_id FROM commands
         WHERE command_id LIKE 'startup-recovery:restart-owner:%' ORDER BY command_id
      `).pluck().all() as string[];
      expect(recoveryCommands).toHaveLength(1);
      expect(recoveryCommands[0]).toContain(actions[1].actionId);
      expect(recoveryCommands[0]).not.toContain(actions[0].actionId);
      const diagnostics = observed.prepare(`
        SELECT payload_json FROM events
         WHERE run_id=? AND type='startup-provider-action-quarantined'
           AND json_extract(payload_json,'$.actionId')=?
      `).pluck().all(fixture.runId, actions[0].actionId);
      expect(diagnostics.map((diagnostic) => JSON.parse(String(diagnostic)))).toStrictEqual([
        {
          actionId: actions[0].actionId,
          adapterId: actions[0].adapterId,
          owner: "integrity_failed",
          reason: "owner_integrity_failed",
        },
      ]);
    } finally {
      observed.close();
    }
  });

  it("revalidates ownership after command replay and before acknowledging re-entry", async () => {
    let armReentry = false;
    let fixtureIdentity: Readonly<{ databasePath: string; runId: string; taskId: string }> | undefined;
    const actionId = "provider-owner-reentry:certifying";
    const commandId = "provider-owner-reentry:dispatch";
    const fixture = await createLifecycleFixture({
      fault: (label) => {
        if (
          label !== "provider-action-owner:before-reentry-acknowledgement" ||
          !armReentry || fixtureIdentity === undefined
        ) return;
        armReentry = false;
        bindCertifyingReviewOwner({
          databasePath: fixtureIdentity.databasePath,
          runId: fixtureIdentity.runId,
          adapterId: "fake-lifecycle",
          actionId,
          taskId: fixtureIdentity.taskId,
        });
      },
    });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    fixtureIdentity = {
      databasePath: fixture.databasePath,
      runId: fixture.runId,
      taskId: fixture.leaderTask.taskId,
    };
    const request = {
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId,
      operation: "send_turn" as const,
      payload: { scenario: "ambiguous-unproven", taskId: fixture.leaderTask.taskId },
      commandId,
    };
    await expect(fixture.chair.dispatchProviderAction(request)).resolves.toMatchObject({
      status: "ambiguous",
      executionCount: 1,
      effectCount: 1,
    });
    armReentry = true;
    await expect(fixture.chair.dispatchProviderAction(request)).rejects.toMatchObject({
      name: "ProviderActionOwnerError",
      expectedOwner: "generic",
      actualOwner: "certifying_review",
    });
    const database = new Database(fixture.databasePath, { readonly: true });
    try {
      expect(database.prepare(`
        SELECT status,execution_count,effect_count FROM provider_actions
         WHERE adapter_id='fake-lifecycle' AND action_id=?
      `).get(actionId)).toEqual({ status: "ambiguous", execution_count: 1, effect_count: 1 });
      expect(database.prepare(`
        SELECT COUNT(*) AS count FROM commands WHERE command_id=?
      `).get(commandId)).toEqual({ count: 1 });
    } finally {
      database.close();
    }
  });

  it.each([
    ["provider-action-owner:before-deferred-enqueue", "prepared", 0],
    ["provider-action-owner:before-deferred-claim", "prepared", 0],
    ["provider-action-owner:before-deferred-completion", "dispatched", 1],
  ] as const)("fails closed at %s without a deferred provider effect or acknowledgement mutation", async (
    boundary,
    expectedStatus,
    expectedExecutionCount,
  ) => {
    let activeActionId: string | undefined;
    let boundaryFired = false;
    let fixtureIdentity: Readonly<{
      databasePath: string;
      runId: string;
      taskId: string;
    }> | undefined;
    const fixture = await createLifecycleFixture({
      maximumConcurrentProviderTurns: 16,
      fault: (label) => {
        if (
          label !== boundary || boundaryFired ||
          activeActionId === undefined || fixtureIdentity === undefined
        ) return;
        boundaryFired = true;
        bindCertifyingReviewOwner({
          databasePath: fixtureIdentity.databasePath,
          runId: fixtureIdentity.runId,
          adapterId: "fake-lifecycle",
          actionId: activeActionId,
          taskId: fixtureIdentity.taskId,
        });
      },
    });
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    fixtureIdentity = {
      databasePath: fixture.databasePath,
      runId: fixture.runId,
      taskId: fixture.leaderTask.taskId,
    };
    const authority = await fixture.chair.delegateAuthority({
      parentAuthorityId: fixture.chairAuthorityId,
      authority: {
        ...fixture.rootAuthority,
        sourcePaths: ["src/leader"],
        budget: { turns: 2, provider_calls: 2, concurrent_turns: 1 },
      },
      commandId: `${boundary}:authority`,
    });
    activeActionId = `${boundary}:action`;
    const commandId = `${boundary}:dispatch`;
    const dispatch = fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: activeActionId,
      operation: "spawn",
      taskId: fixture.leaderTask.taskId,
      authorityId: authority.authorityId,
      payload: {
        model: "fake-reviewer-v1",
        modelFamily: "fake",
        prompt: `Exercise ${boundary}.`,
        cwd: "src/leader",
        scenario: "terminal-exact-usage",
        maxTurns: 1,
      },
      commandId,
    });
    await expect(dispatch).rejects.toMatchObject({
      name: "ProviderActionOwnerError",
      expectedOwner: "generic",
      actualOwner: "certifying_review",
    });
    const database = new Database(fixture.databasePath, { readonly: true });
    try {
      expect(database.prepare(`
        SELECT status,execution_count,effect_count FROM provider_actions
         WHERE adapter_id='fake-lifecycle' AND action_id=?
      `).get(activeActionId)).toEqual({
        status: expectedStatus,
        execution_count: expectedExecutionCount,
        effect_count: 0,
      });
      expect(database.prepare(`
        SELECT json_extract(result_json,'$.status') AS status
          FROM commands WHERE command_id=?
      `).get(commandId)).toEqual({ status: "prepared" });
    } finally {
      database.close();
    }
    await expect(readFile(fixture.providerJournalPath, "utf8")).rejects.toMatchObject({ code: "ENOENT" });
    if (boundary === "provider-action-owner:before-deferred-claim") {
      await settle(150, "no deferred claim lands before recovery inspects the boundary");
      await expect(fixture.fabric.recoverStartupState()).resolves.toMatchObject({ actionsReconciled: 1 });
      const specialisedRecovery = await waitForProviderAction(fixture.chair, activeActionId);
      expect(specialisedRecovery.status).not.toBe("prepared");
      expect(specialisedRecovery.executionCount).toBe(1);
    }
  });

  it("quarantines ambiguity without replay when downstream idempotency is unproven", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const ambiguous = await fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "action-ambiguous-unproven",
      operation: "send_turn",
      payload: { scenario: "ambiguous-unproven", taskId: fixture.leaderTask.taskId },
      commandId: "provider-action:ambiguous-unproven:dispatch",
    });
    expect(ambiguous).toMatchObject({
      status: "ambiguous",
      history: ["prepared", "dispatched", "accepted", "ambiguous"],
      executionCount: 1,
      effectCount: 1,
    });

    const reconciled = await fixture.chair.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId: ambiguous.actionId,
      commandId: "provider-action:ambiguous-unproven:reconcile",
    });
    expect(reconciled).toMatchObject({ status: "quarantined", executionCount: 1, effectCount: 1 });
    expect(await readProviderAction(fixture.chair, ambiguous.actionId)).toEqual(reconciled);
  });

  it("replays only the same action ID when the adapter proves idempotency", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const ambiguous = await fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "action-ambiguous-idempotent",
      operation: "send_turn",
      payload: { scenario: "ambiguous-idempotent", taskId: fixture.leaderTask.taskId },
      commandId: "provider-action:ambiguous-idempotent:dispatch",
    });
    const reconciled = await fixture.chair.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId: ambiguous.actionId,
      commandId: "provider-action:ambiguous-idempotent:reconcile",
    });

    expect(reconciled).toMatchObject({
      actionId: "action-ambiguous-idempotent",
      status: "terminal",
      executionCount: 2,
      effectCount: 1,
    });
    expect(reconciled.history).toContain("ambiguous");
    expect(reconciled.history.at(-1)).toBe("terminal");
  });

  it("does not replay an idempotent action after the target principal is revoked", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const ambiguous = await fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "action-revoked-before-replay",
      operation: "send_turn",
      payload: { scenario: "ambiguous-idempotent", taskId: fixture.leaderTask.taskId },
      commandId: "provider-action:revoked:dispatch",
    });
    await fixture.chair.revokeCapability({ agentId: "leader", commandId: "provider-action:revoke-leader" });

    await expect(fixture.chair.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId: ambiguous.actionId,
      commandId: "provider-action:revoked:reconcile",
    })).rejects.toMatchObject({ code: "AUTHENTICATION_FAILED" });
  });

  it("does not replay an idempotent action after the target principal expires", async () => {
    const fixture = await createLifecycleFixture();
    cleanup.push(async () => {
      await fixture.fabric.close();
      await rm(fixture.directory, { recursive: true, force: true });
    });
    const ambiguous = await fixture.chair.dispatchProviderAction({
      certifyingReview: null,
      adapterId: "fake-lifecycle",
      actionId: "action-expired-before-replay",
      operation: "send_turn",
      payload: { scenario: "ambiguous-idempotent", taskId: fixture.leaderTask.taskId },
      commandId: "provider-action:expired:dispatch",
    });
    fixture.clock.advance(Date.parse("2100-01-01T00:00:00.000Z") - fixture.clock.now().getTime());

    await expect(fixture.chair.reconcileProviderAction({
      adapterId: "fake-lifecycle",
      actionId: ambiguous.actionId,
      commandId: "provider-action:expired:reconcile",
    })).rejects.toMatchObject({ code: "AUTHENTICATION_FAILED" });
  });
});
