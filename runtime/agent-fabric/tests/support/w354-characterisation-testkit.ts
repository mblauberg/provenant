import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";

import Database from "better-sqlite3";

import { classifyProviderActionOwner } from "../../src/application/provider-action-owner.ts";
import { admitProviderActionFixture } from "./provider-action-fixture.ts";
import type { LifecycleFixture } from "./lifecycle-testkit.ts";

export type ProviderActionRef = Readonly<{
  runId: string;
  adapterId: string;
  actionId: string;
}>;

export function canonicalJson(value: unknown): string {
  if (value === null || typeof value === "boolean" || typeof value === "number" || typeof value === "string") {
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (typeof value === "object") {
    const record = value as Record<string, unknown>;
    return `{${Object.keys(record).sort().map((key) => `${JSON.stringify(key)}:${canonicalJson(record[key])}`).join(",")}}`;
  }
  throw new Error("characterisation input must be JSON-compatible");
}

export function sha256Digest(value: unknown): string {
  return `sha256:${createHash("sha256").update(canonicalJson(value)).digest("hex")}`;
}

export function seedProviderAction(
  database: Database.Database,
  input: Readonly<{
    runId: string;
    adapterId?: string;
    actionId: string;
    operation?: string;
    payload?: Record<string, unknown>;
    status?: "prepared" | "dispatched" | "accepted" | "ambiguous" | "terminal";
    historyJson?: string;
    executionCount?: number;
    effectCount?: number;
    idempotencyProven?: boolean;
    resultJson?: string | null;
    taskId?: string | null;
    budgetAuthorityId?: string | null;
    budgetReservationJson?: string | null;
    budgetSettlementJson?: string | null;
    budgetState?: string | null;
    budgetStartedAt?: number | null;
    updatedAt?: number;
  }>,
): ProviderActionRef {
  const adapterId = input.adapterId ?? "fake-lifecycle";
  const payload = input.payload ?? {};
  const status = input.status ?? "prepared";
  const executionCount = input.executionCount ?? (status === "prepared" ? 0 : 1);
  const effectCount = input.effectCount ?? 0;
  const now = input.updatedAt ?? 1;
  const payloadJson = canonicalJson(payload);
  admitProviderActionFixture(database, {
    runId: input.runId,
    actionId: input.actionId,
    adapterId,
    operation: input.operation ?? "wakeup",
    targetAgentId: null,
    providerSessionGeneration: null,
    turnLeaseGeneration: null,
    identityHash: "a".repeat(64),
    payloadHash: "b".repeat(64),
    payloadJson,
    status,
    historyJson: input.historyJson ?? (status === "prepared" ? '["prepared"]' : '["prepared","dispatched"]'),
    executionCount,
    effectCount,
    idempotencyProven: input.idempotencyProven ?? false,
    resultJson: input.resultJson ?? null,
    taskId: input.taskId ?? null,
    budgetAuthorityId: input.budgetAuthorityId ?? null,
    budgetReservationJson: input.budgetReservationJson ?? null,
    budgetSettlementJson: input.budgetSettlementJson ?? null,
    budgetState: input.budgetState ?? null,
    budgetStartedAt: input.budgetStartedAt ?? null,
    updatedAt: now,
  });
  return { runId: input.runId, adapterId, actionId: input.actionId };
}

export function bindProviderAgentOwner(database: Database.Database, ref: ProviderActionRef): void {
  const authority = database.prepare(`
    SELECT authority_id FROM agents WHERE run_id=? AND agent_id='leader'
  `).pluck().get(ref.runId);
  if (typeof authority !== "string") throw new Error("provider-agent fixture authority is unavailable");
  database.prepare(`
    INSERT INTO provider_agent_custody(
      run_id,action_id,operation,actor_agent_id,target_agent_id,authority_id,
      adapter_id,bridge_contract_digest,bridge_capable,capability_hash,
      capability_expires_at,principal_generation,requested_provider_session_ref,
      intent_digest,created_at
    ) VALUES (?,?,'attach','leader','leader',?,?,?,0,NULL,NULL,NULL,'fixture-session',?,1)
  `).run(
    ref.runId,
    ref.actionId,
    authority,
    ref.adapterId,
    "sha256:" + "1".repeat(64),
    "sha256:" + "2".repeat(64),
  );
}

export function bindMinimalLifecycleOwner(database: Database.Database, ref: ProviderActionRef): void {
  const projectSessionId = database.prepare("SELECT project_session_id FROM runs WHERE run_id=?").pluck().get(ref.runId);
  if (typeof projectSessionId !== "string") throw new Error("lifecycle fixture project session is unavailable");
  database.prepare(`
    INSERT INTO lifecycle_rotation_custodies(
    project_session_id,run_id,agent_id,custody_id,command_id,
      provider_action_adapter_id,provider_action_id,replacement_contract_digest,
      staged_capability_hash,target_principal_generation
    ) VALUES (?,?,?,?,?,?,?,?,?,?)
  `).run(
    projectSessionId,
    ref.runId,
    "leader",
    `fixture-lifecycle:${ref.actionId}`,
    `fixture-lifecycle:${ref.actionId}:command`,
    ref.adapterId,
    ref.actionId,
    "sha256:" + "3".repeat(64),
    "fixture-capability",
    1,
  );
}

export function corruptOwner(database: Database.Database, ref: ProviderActionRef): void {
  database.pragma("foreign_keys = OFF");
  database.prepare(`
    UPDATE provider_actions SET finding_capacity_reservation_digest=?
     WHERE run_id=? AND adapter_id=? AND action_id=?
  `).run("sha256:" + "f".repeat(64), ref.runId, ref.adapterId, ref.actionId);
}

export function actionSnapshot(databasePath: string, ref: ProviderActionRef): Record<string, unknown> {
  const database = new Database(databasePath, { readonly: true });
  try {
    return database.prepare(`
      SELECT run_id,adapter_id,action_id,operation,status,history_json,execution_count,
             effect_count,idempotency_proven,result_json,journal_revision,task_id,
             budget_authority_id,budget_reservation_json,budget_settlement_json,
             budget_state,budget_started_at,finding_capacity_reservation_digest
        FROM provider_actions WHERE run_id=? AND adapter_id=? AND action_id=?
    `).get(ref.runId, ref.adapterId, ref.actionId) as Record<string, unknown>;
  } finally {
    database.close();
  }
}

export function durableSnapshot(databasePath: string, runId: string, ref?: ProviderActionRef): Record<string, unknown> {
  const database = new Database(databasePath, { readonly: true });
  try {
    return {
      action: ref === undefined ? undefined : database.prepare(`
        SELECT * FROM provider_actions WHERE run_id=? AND adapter_id=? AND action_id=?
      `).get(ref.runId, ref.adapterId, ref.actionId),
      commands: database.prepare("SELECT * FROM commands WHERE run_id=? ORDER BY actor_agent_id,command_id").all(runId),
      events: database.prepare("SELECT * FROM events WHERE run_id=? ORDER BY event_id").all(runId),
      ledger: database.prepare("SELECT * FROM authority_budget WHERE authority_id IN (SELECT authority_id FROM agents WHERE run_id=?) ORDER BY authority_id,unit_key").all(runId),
    };
  } finally {
    database.close();
  }
}

export async function readFakeJournal(path: string): Promise<Record<string, unknown>> {
  try {
    return JSON.parse(await readFile(path, "utf8")) as Record<string, unknown>;
  } catch (error: unknown) {
    if (error instanceof Error && "code" in error && error.code === "ENOENT") {
      return { schemaVersion: 1, actions: {}, sessions: {} };
    }
    throw error;
  }
}

export function owner(databasePath: string, ref: ProviderActionRef): string {
  const database = new Database(databasePath, { readonly: true });
  try {
    return classifyProviderActionOwner(database, ref);
  } finally {
    database.close();
  }
}

export function closeFixture(cleanup: Array<() => Promise<void>>, fixture: LifecycleFixture): () => Promise<void> {
  let closed = false;
  const close = async (): Promise<void> => {
    if (closed) return;
    closed = true;
    await fixture.fabric.close();
  };
  cleanup.push(async () => {
    await close();
    const { rm } = await import("node:fs/promises");
    await rm(fixture.directory, { recursive: true, force: true });
  });
  return close;
}
