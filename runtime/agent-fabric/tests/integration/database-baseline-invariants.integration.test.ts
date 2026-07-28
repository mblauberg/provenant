import Database from "better-sqlite3";
import { createHash } from "node:crypto";
import { describe, expect, it } from "vitest";

import { applyMigrations } from "../../src/core/migrations.ts";
import { canonicalJson, digest } from "../../src/persistence/row-codec.ts";
import {
  admitProviderActionFixture,
  insertProviderActionPreflightParent,
} from "../support/provider-action-fixture.ts";

const SHA_A = `sha256:${"a".repeat(64)}`;
const EMPTY_FINDING_SET_JSON = '{"findingCount":0,"pages":[],"schemaVersion":1}';
const EMPTY_FINDING_SET_DIGEST = `sha256:${createHash("sha256")
  .update(EMPTY_FINDING_SET_JSON)
  .digest("hex")}`;
const RECOVERY_NOW = Date.now();
const RECOVERY_ISSUED_AT = RECOVERY_NOW - 60_000;
const RECOVERY_EXPIRES_AT = RECOVERY_NOW + 60_000;

function openDatabase(): Database.Database {
  const database = new Database(":memory:");
  database.pragma("foreign_keys = ON");
  applyMigrations(database);
  return database;
}

function foreignKeySignatures(database: Database.Database, table: string): string[] {
  const rows = database.prepare(`SELECT * FROM pragma_foreign_key_list(?)`).all(table) as Array<{
    id: number;
    seq: number;
    table: string;
    from: string;
    to: string;
  }>;
  const grouped = new Map<number, typeof rows>();
  for (const row of rows) grouped.set(row.id, [...(grouped.get(row.id) ?? []), row]);
  return [...grouped.values()].map((group) => {
    const ordered = group.toSorted((left, right) => left.seq - right.seq);
    return `${ordered[0]?.table}:${ordered.map((row) => `${row.from}->${row.to}`).join(",")}`;
  });
}

function seedRun(database: Database.Database): void {
  database.prepare(`
    INSERT INTO projects(
      project_id,canonical_root,revision,authority_generation,created_at,updated_at
    ) VALUES (?,?,?,?,?,?)
  `).run("project", "/tmp/project", 1, 1, 1, 1);
  database.prepare(`
    INSERT INTO project_sessions(
      project_session_id,project_id,mode,state,revision,generation,
      authority_ref,budget_ref,launch_packet_path,launch_packet_digest,
      membership_revision,origin_kind,origin_operator_id,created_at,updated_at
    ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
  `).run(
    "session", "project", "coordinated", "active", 1, 1,
    SHA_A, "budget-ref", "launch", "launch-digest",
    1, "operator-launch", "operator", 1, 1,
  );
  database.prepare(`
    INSERT INTO runs(
      run_id,chair_agent_id,workspace_root,created_at,project_session_id,
      lifecycle_state,revision,chair_generation,chair_lease_id,authority_ref,
      budget_ref,dependency_revision,topology_slot
    ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)
  `).run(
    "run", "chair", "/tmp", 1, "session", "active", 1, 1,
    "chair-lease", SHA_A, "budget-ref", 1, 1,
  );
  database.prepare(`
    INSERT INTO authorities(authority_id,run_id,authority_json,authority_hash,created_at)
    VALUES (?,?,?,?,?)
  `).run("authority", "run", "{}", "a".repeat(64), 1);
  database.prepare(`
    INSERT INTO agents(run_id,agent_id,authority_id,lifecycle) VALUES (?,?,?,?)
  `).run("run", "chair", "authority", "ready");
}

function seedTopologyParents(database: Database.Database): void {
  seedRun(database);
  database.prepare(`
    INSERT INTO tasks(
      run_id,task_id,authority_id,objective,base_revision,state,revision,created_by
    ) VALUES (?,?,?,?,?,?,?,?)
  `).run("run", "task", "authority", "test", "base", "active", 1, "chair");
  database.prepare(`
    INSERT INTO artifacts(
      artifact_id,project_id,publisher_kind,publisher_ref,source_kind,evidence_kind,
      relative_path,sha256,registry_state,revision,created_at
    ) VALUES (?,?,?,?,?,?,?,?,?,?,?)
  `).run(
    "rationale", "project", "operator", "operator", "project-file", "artifact",
    "rationale.txt", SHA_A, "active", 1, 1,
  );
  database.prepare(`
    INSERT INTO run_authority_revisions(
      project_session_id,coordination_run_id,authority_revision,authority_ref,
      git_allowlist_epoch,git_allowlist_digest,activated_at_run_revision,created_at
    ) VALUES (?,?,?,?,?,?,?,?)
  `).run("session", "run", 1, SHA_A, 1, null, 1, 1);
}

function seedTopologyCurrency(database: Database.Database): void {
  seedTopologyParents(database);
  database.prepare(`
    INSERT INTO run_chair_leases(
      project_session_id,run_id,lease_id,holder_agent_id,generation,status,updated_at
    ) VALUES (?,?,?,?,?,?,?)
  `).run("session", "run", "chair-lease", "chair", 1, "active", 1);
  database.prepare(`
    INSERT INTO agent_lifecycle_identity_high_water(
      run_id,agent_id,provider_generation,principal_generation,revision
    ) VALUES (?,?,?,?,?)
  `).run("run", "chair", 1, 1, 1);
  database.prepare(`
    INSERT INTO coordination_policy_revisions(
      project_session_id,coordination_run_id,policy_revision,
      policy_ref,policy_digest,created_at
    ) VALUES (?,?,?,?,?,?)
  `).run("session", "run", 1, SHA_A, SHA_A, 1);
  database.prepare(`
    INSERT INTO coordination_policy_current(
      project_session_id,coordination_run_id,policy_revision,
      policy_ref,policy_digest,revision
    ) VALUES (?,?,?,?,?,?)
  `).run("session", "run", 1, SHA_A, SHA_A, 1);
}

function topologyPlan(policyRevision = 1): Readonly<{ json: string; digest: string }> {
  const body = {
    authority: {
      authorityDigest: SHA_A,
      authorityRef: SHA_A,
      authorityRevision: 1,
    },
    budget: {
      maximumParallelAgents: 1,
      providerTurns: 10,
      toolCalls: 10,
      wallClockSeconds: 60,
    },
    chair: {
      agentId: "chair",
      chairLeaseGeneration: 1,
      principalGeneration: 1,
    },
    contention: {
      evidenceRef: SHA_A,
      mode: "none",
      serializationOwnerAgentId: null,
    },
    coordinationRunId: "run",
    createdAt: 1,
    decomposability: {
      evidenceRef: SHA_A,
      kind: "atomic",
    },
    dependencies: [],
    policy: {
      policyDigest: SHA_A,
      policyRef: SHA_A,
      policyRevision,
    },
    predecessor: null,
    projectSessionId: "session",
    rationaleRef: {
      evidenceId: "rationale",
      evidenceRevision: 1,
    },
    schemaVersion: 1,
    stageOwners: [{
      ownerAgentId: "chair",
      stageId: "stage",
      taskId: "task",
      writePartitionId: "partition",
    }],
    state: "approved",
    stopConditions: [{
      conditionId: "complete",
      kind: "objective-complete",
      predicateRef: SHA_A,
    }],
    taskId: "task",
    topology: {
      executionShape: "single-owner",
      maximumConcurrentAgents: 1,
      mode: "serial",
    },
    waveId: "wave",
    waveRevision: 1,
    writePartitions: [{
      authorityRef: SHA_A,
      mode: "exclusive-write",
      ownerAgentId: "chair",
      partitionId: "partition",
      pathSetDigest: SHA_A,
    }],
  };
  const planDigest = digest(body);
  return { json: canonicalJson({ ...body, planDigest }), digest: planDigest };
}

function insertTopologyPlan(
  database: Database.Database,
  plan: Readonly<{ json: string; digest: string }>,
  policyRevision = 1,
  rationaleEvidenceId = "rationale",
): void {
  database.prepare(`
    INSERT INTO topology_wave_plans(
      project_session_id,coordination_run_id,task_id,wave_id,wave_revision,
      chair_agent_id,principal_generation,chair_lease_generation,
      authority_revision,authority_ref,authority_digest,
      policy_revision,policy_ref,policy_digest,
      rationale_evidence_id,rationale_evidence_revision,state,
      plan_json,plan_digest,created_at
    ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
  `).run(
    "session", "run", "task", "wave", 1,
    "chair", 1, 1,
    1, SHA_A, SHA_A,
    policyRevision, SHA_A, SHA_A,
    rationaleEvidenceId, 1, "approved",
    plan.json, plan.digest, 1,
  );
}

function seedGenerationLossRecovery(database: Database.Database): void {
  database.prepare(`
    INSERT INTO operator_principals(
      operator_id,project_id,project_session_id,authenticated_subject_hash,
      project_authority_generation,principal_generation,state,created_at,updated_at
    ) VALUES (?,?,?,?,?,?,?,?,?)
  `).run("operator", "project", "session", "subject-hash", 1, 1, "active", 1, 1);
  database.prepare(`
    INSERT INTO operator_capabilities(
      capability_id,token_hash,operator_id,project_id,project_session_id,
      project_authority_generation,session_generation,principal_generation,
      kind,operations_json,issued_at,expires_at
    ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)
  `).run(
    "parent-capability", "b".repeat(64), "operator", "project", "session",
    1, 1, 1, "session", '["agent-lifecycle-recovery-issue"]',
    RECOVERY_NOW - 180_000, RECOVERY_NOW + 180_000,
  );
  database.prepare(`
    INSERT INTO scoped_gates(
      gate_id,project_session_id,coordination_run_id,dedupe_key,scope_kind,
      scope_task_id,dependency_revision,blocked_operation_ids_json,
      enforcement_points_json,question,reason,options_json,recommendation,
      consequences_json,evidence_refs_json,created_by_ref,expected_approver_ref,
      resolved_by_operator_id,resolution_json,status,human_required,
      release_binding_json,revision,created_at,updated_at
    ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
  `).run(
    "recovery-gate", "session", "run",
    "agent-lifecycle-recovery:run:chair:loss", "run", null, 1, "[]",
    '["agent-lifecycle-recovery-issue"]', "Recover chair?", "Stranded lifecycle",
    "[]", "fresh rotate", "[]", "[]", "fabric", "operator", "operator",
    "{}", "approved", 1, null, 1, 1, 1,
  );
  admitProviderActionFixture(database, {
    runId: "run",
    actionId: "source-action",
    adapterId: "adapter",
    operation: "lifecycle-source",
    targetAgentId: "chair",
    providerSessionGeneration: 1,
    identityHash: "identity",
    payloadHash: "payload",
    payloadJson: "{}",
    status: "terminal",
    historyJson: "[]",
    executionCount: 1,
    effectCount: 1,
    idempotencyProven: true,
    resultJson: "{}",
    updatedAt: 1,
  });
  database.prepare(`
    INSERT INTO lifecycle_generation_losses(
      project_session_id,run_id,agent_id,generation_loss_id,loss_kind,
      old_provider_session_ref,new_provider_session_ref,
      old_provider_generation,new_provider_generation,
      old_context_revision,new_context_revision,
      source_custody_action_id,source_adapter_id,source_adapter_contract_digest,
      source_principal_generation,source_bridge_generation,bridge_owner_kind,
      source_bridge_row_id,source_bridge_revision,source_capability_hash,
      source_project_session_generation,source_run_generation,
      source_chair_lease_generation,checkpoint_state,checkpoint_ref,
      checkpoint_digest,loss_evidence_digest,creation_json,creation_digest,created_at
    ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
  `).run(
    "session", "run", "chair", "loss", "generation-advance",
    "provider-session-1", "provider-session-2", 1, 2, 0, 0,
    "source-action", "adapter", SHA_A, 1, 1, "chair", "bridge-row", 1,
    "source-capability", 1, 1, 1, "last-validated", "checkpoint", SHA_A, SHA_A,
    "{}", "loss-creation", 1,
  );
  database.prepare(`
    INSERT INTO lifecycle_generation_loss_revisions(
      project_session_id,run_id,agent_id,generation_loss_id,revision,
      prior_revision,prior_journal_digest,state,abandon_kind_code,
      recovery_action_adapter_id,recovery_action_id,active_recovery_custody_id,
      terminal_evidence_digest,semantic_json,semantic_digest,source_ref_digest,
      origin_fresh_apply_id,origin_fresh_apply_digest,receipt_batch_id,
      receipt_apply_id,receipt_apply_digest,journal_json,journal_digest,recorded_at
    ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
  `).run(
    "session", "run", "chair", "loss", 1, null, null, "open", "none",
    null, null, null, null, "{}", "loss-semantic", "loss-source-ref",
    null, null, null, null, null, "{}", "loss-journal", 1,
  );
  database.prepare(`
    INSERT INTO lifecycle_generation_loss_heads(
      project_session_id,run_id,agent_id,generation_loss_id,current_revision,
      state,abandon_kind_code,semantic_digest,source_ref_digest,journal_digest,
      terminal,head_revision
    ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?)
  `).run(
    "session", "run", "chair", "loss", 1, "open", "none",
    "loss-semantic", "loss-source-ref", "loss-journal", 0, 1,
  );
}

function insertGenerationLossRecoveryIssue(
  database: Database.Database,
  generationLossRevision = 1,
  issuedAt = RECOVERY_ISSUED_AT,
  expiresAt = RECOVERY_EXPIRES_AT,
): void {
  database.prepare(`
    INSERT INTO agent_lifecycle_recovery_capability_issues(
      issue_id,capability_hash,operator_id,project_id,project_session_id,run_id,agent_id,
      session_revision,session_generation,run_revision,recovery_source_kind,
      old_custody_id,old_action_adapter_id,old_action_id,old_custody_revision,
      generation_loss_id,generation_loss_revision,recovery_source_ref_digest,
      source_journal_digest,checkpoint_digest,
      source_provider_session_ref,source_capability_hash,source_custody_action_id,
      source_adapter_id,source_adapter_contract_digest,source_bridge_row_id,
      source_bridge_revision,source_provider_generation,source_principal_generation,
      source_bridge_generation,source_project_session_generation,source_run_generation,
      source_chair_lease_generation,bridge_owner_kind,parent_capability_id,
      consequential_gate_id,path,issuance_json,issuance_digest,issued_at,expires_at
    ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
  `).run(
    "issue", "c".repeat(64), "operator", "project", "session", "run", "chair",
    1, 1, 1, "generation-loss", null, null, null, null,
    "loss", generationLossRevision, "loss-source-ref", "loss-journal", SHA_A,
    "provider-session-1", "source-capability", "source-action", "adapter", SHA_A,
    "bridge-row", 1, 1, 1, 1, 1, 1, 1, "chair",
    "parent-capability", "recovery-gate", "fresh-rotate", "{}", "issue-digest",
    issuedAt, expiresAt,
  );
}

function currentGenerationLossRecoveryIssues(database: Database.Database, now: number): unknown[] {
  return database.prepare(`
    SELECT head.issue_id
      FROM agent_lifecycle_recovery_source_heads head
      JOIN agent_lifecycle_recovery_capability_issues issue
        ON issue.issue_id=head.issue_id
      LEFT JOIN agent_lifecycle_recovery_issue_revocations revocation
        ON revocation.issue_id=issue.issue_id
      LEFT JOIN lifecycle_fresh_recovery_handoffs handoff
        ON handoff.issue_id=issue.issue_id
     WHERE issue.expires_at>? AND revocation.issue_id IS NULL
       AND handoff.issue_id IS NULL
     ORDER BY head.issue_id
  `).all(now);
}

function safeFinding(): Readonly<{ digest: string; json: string }> {
  const body = {
    evidence: "bounded evidence",
    findingId: "F-1",
    originActionRef: { actionId: "action", adapterId: "adapter" },
    originBundleDigest: SHA_A,
    originDeliveryManifestRef: "manifest",
    originDeliveryReviewBasisDigest: SHA_A,
    originResultDigest: SHA_A,
    originTargetGeneration: 1,
    repairCurrency: {
      evidenceRefs: [],
      kind: "repository-source",
      originRepositorySourceStateDigest: SHA_A,
    },
    severity: "P2",
    summary: "bounded summary",
  };
  const findingDigest = digest(body);
  return { digest: findingDigest, json: canonicalJson({ ...body, findingDigest }) };
}

function findingGraph(finding = safeFinding()): Readonly<{
  finding: ReturnType<typeof safeFinding>;
  pageDigest: string;
  pageByteLength: number;
  setDigest: string;
  setByteLength: number;
}> {
  const findingValue = JSON.parse(finding.json) as Record<string, unknown>;
  const page = { members: [findingValue], schemaVersion: 1 };
  const pageDigest = digest(page);
  const set = {
    findingCount: 1,
    pages: [{
      firstFindingDigest: finding.digest,
      lastFindingDigest: finding.digest,
      memberCount: 1,
      ordinal: 0,
      pageDigest,
    }],
    schemaVersion: 1,
  };
  return {
    finding,
    pageDigest,
    pageByteLength: Buffer.byteLength(canonicalJson(page)),
    setDigest: digest(set),
    setByteLength: Buffer.byteLength(canonicalJson(set)),
  };
}

function insertProviderAction(
  database: Database.Database,
  adapterId: string,
  actionId = "shared-action",
): void {
  admitProviderActionFixture(database, {
    runId: "run",
    actionId,
    adapterId,
    operation: "turn",
    targetAgentId: "chair",
    providerSessionGeneration: 1,
    identityHash: `identity-${adapterId}`,
    payloadHash: `payload-${adapterId}`,
    payloadJson: "{}",
    status: "terminal",
    historyJson: "[]",
    executionCount: 1,
    effectCount: 1,
    idempotencyProven: true,
    resultJson: "{}",
    updatedAt: 1,
  });
}

describe("current database baseline invariants", () => {
  it("keeps identical availability upserts stable while every stored change advances revision", () => {
    const database = openDatabase();
    const revision = (): number => (database.prepare(`
      SELECT revision FROM daemon_global_state WHERE singleton=1
    `).get() as { revision: number }).revision;

    database.prepare(`
      INSERT INTO integration_availability(
        integration_id,state,discovered_contract_json,checked_at
      ) VALUES (?,?,?,?)
    `).run("native-desktop", "available", '{"version":1}', 100);
    const afterInsert = revision();

    const refresh = database.prepare(`
      INSERT INTO integration_availability(
        integration_id,state,discovered_contract_json,checked_at
      ) VALUES (?,?,?,?)
      ON CONFLICT(integration_id) DO UPDATE SET
        state=excluded.state,
        discovered_contract_json=excluded.discovered_contract_json,
        checked_at=excluded.checked_at
      WHERE integration_availability.state IS NOT excluded.state
         OR integration_availability.discovered_contract_json IS NOT excluded.discovered_contract_json
    `);
    refresh.run("native-desktop", "available", '{"version":1}', 200);
    expect(revision()).toBe(afterInsert);
    expect(database.prepare(`
      SELECT checked_at FROM integration_availability WHERE integration_id=?
    `).get("native-desktop")).toEqual({ checked_at: 100 });

    refresh.run("native-desktop", "stale", '{"version":1}', 300);
    expect(revision()).toBe(afterInsert + 1);

    refresh.run("native-desktop", "stale", '{"version":2}', 400);
    expect(revision()).toBe(afterInsert + 2);

    database.prepare(`
      UPDATE integration_availability SET checked_at=? WHERE integration_id=?
    `).run(500, "native-desktop");
    expect(revision()).toBe(afterInsert + 3);

    database.close();
  });

  it("installs the complete strict lifecycle rotation persistence graph", () => {
    const database = openDatabase();
    const lifecycleTables = [
      "agent_lifecycle_bridge_high_water",
      "agent_lifecycle_context_high_water",
      "agent_lifecycle_identity_high_water",
      "agent_lifecycle_recovery_capability_issues",
      "agent_lifecycle_recovery_issue_revocations",
      "agent_lifecycle_recovery_retirements",
      "agent_lifecycle_recovery_source_heads",
      "lifecycle_admitted_run_scopes",
      "lifecycle_authority_receipts",
      "lifecycle_custody_adoption_deliveries",
      "lifecycle_fresh_recovery_handoffs",
      "lifecycle_fresh_rotation_commits",
      "lifecycle_fresh_rotation_preparations",
      "lifecycle_generation_loss_heads",
      "lifecycle_generation_loss_revisions",
      "lifecycle_generation_losses",
      "lifecycle_receipt_batch_authorizations",
      "lifecycle_receipt_batch_completions",
      "lifecycle_receipt_batches",
      "lifecycle_receipt_custody_effects",
      "lifecycle_receipt_fresh_origin_effects",
      "lifecycle_receipt_generation_loss_effects",
      "lifecycle_receipt_intents",
      "lifecycle_receipt_namespace_checkpoints",
      "lifecycle_receipt_namespace_heads",
      "lifecycle_receipt_namespace_members",
      "lifecycle_receipt_recovery_retirement_effects",
      "lifecycle_receipt_scope_checkpoints",
      "lifecycle_receipt_scope_heads",
      "lifecycle_recovery_retirement_plans",
      "lifecycle_review_adoption_reservations",
      "lifecycle_review_authority_bindings",
      "lifecycle_rotation_custodies",
      "lifecycle_rotation_custody_heads",
      "lifecycle_rotation_custody_revisions",
      "lifecycle_scope_admission_outbox",
      "lifecycle_scope_admission_resolutions",
      "lifecycle_transition_applies",
      "provider_context_observation_audit",
    ] as const;
    const installed = new Map((database.prepare(`
      SELECT name,sql FROM sqlite_schema WHERE type='table'
    `).all() as { name: string; sql: string }[]).map((row) => [row.name, row.sql]));

    expect(lifecycleTables.filter((table) => !installed.has(table))).toEqual([]);
    for (const table of lifecycleTables) {
      expect(installed.get(table)?.trimEnd().endsWith("STRICT"), table).toBe(true);
    }

    const custodyForeignKeys = foreignKeySignatures(database, "lifecycle_rotation_custody_revisions");
    expect(custodyForeignKeys).toEqual(expect.arrayContaining([
      "lifecycle_receipt_custody_effects:receipt_batch_id->batch_id,receipt_apply_id->planned_apply_id,project_session_id->project_session_id,run_id->run_id,agent_id->agent_id,custody_id->custody_id,revision->final_revision,semantic_digest->final_semantic_digest,source_ref_digest->final_source_ref_digest",
      "lifecycle_transition_applies:receipt_apply_id->apply_id,receipt_apply_digest->apply_digest,receipt_batch_id->receipt_batch_id",
    ]));
    expect(database.prepare(`
      SELECT name FROM sqlite_schema
       WHERE type='index' AND name='one_nonfinal_lifecycle_custody_per_agent'
    `).get()).toEqual({ name: "one_nonfinal_lifecycle_custody_per_agent" });
    expect(database.prepare(`
      SELECT name FROM sqlite_schema
       WHERE type='index' AND name='one_nonterminal_generation_loss_per_agent'
    `).get()).toEqual({ name: "one_nonterminal_generation_loss_per_agent" });

    database.close();
  });

  it("equality-binds preflight, action, route, reservation, and evidence to one exact run", () => {
    const database = openDatabase();

    expect(foreignKeySignatures(database, "provider_action_pair_preflights")).toContain(
      "runs:run_id->run_id",
    );
    expect(foreignKeySignatures(database, "provider_actions")).toContain(
      "provider_action_pair_preflights:run_id->run_id,adapter_id->adapter_id,action_id->action_id",
    );
    expect(foreignKeySignatures(database, "provider_actions")).toContain(
      "review_finding_capacity_reservations:run_id->run_id,adapter_id->adapter_id,action_id->action_id,finding_capacity_reservation_digest->reservation_digest",
    );
    expect(foreignKeySignatures(database, "provider_action_routes")).toEqual(expect.arrayContaining([
      "provider_action_pair_preflights:run_id->run_id,adapter_id->adapter_id,action_id->action_id",
      "provider_actions:run_id->run_id,adapter_id->adapter_id,action_id->action_id",
    ]));
    expect(foreignKeySignatures(database, "review_finding_capacity_reservations")).toContain(
      "provider_action_pair_preflights:run_id->run_id,adapter_id->adapter_id,action_id->action_id,owner_digest->owner_digest",
    );
    expect(foreignKeySignatures(database, "provider_review_evidence")).toContain(
      "review_finding_capacity_reservations:run_id->run_id,adapter_id->adapter_id,action_id->action_id,reservation_digest->reservation_digest",
    );

    database.close();
  });

  it("requires every run-action preflight to reference a run while provider smoke remains unscoped", () => {
    const database = openDatabase();

    expect(() => database.prepare(`
      INSERT INTO provider_action_pair_preflights(
        adapter_id,action_id,scope_kind,run_id,owner_digest,actor_principal_digest,
        input_digest,state,created_at,updated_at
      ) VALUES ('adapter','missing-run-action','run-action','missing-run','owner','principal','input','resolving',1,1)
    `).run()).toThrow("FOREIGN KEY constraint failed");
    expect(() => database.prepare(`
      INSERT INTO provider_action_pair_preflights(
        adapter_id,action_id,scope_kind,run_id,owner_digest,actor_principal_digest,
        input_digest,state,created_at,updated_at
      ) VALUES ('adapter','provider-smoke','provider-smoke',NULL,'smoke-owner','smoke-principal','smoke-input','resolving',1,1)
    `).run()).not.toThrow();

    database.close();
  });

  it("rejects action and reservation rows crossed onto another run", () => {
    const database = openDatabase();
    seedRun(database);
    database.prepare(`
      INSERT INTO projects(
        project_id,canonical_root,revision,authority_generation,created_at,updated_at
      ) VALUES (?,?,?,?,?,?)
    `).run("other-project", "/tmp/other-project", 1, 1, 1, 1);
    database.prepare(`
      INSERT INTO project_sessions(
        project_session_id,project_id,mode,state,revision,generation,
        authority_ref,budget_ref,launch_packet_path,launch_packet_digest,
        membership_revision,origin_kind,origin_operator_id,created_at,updated_at
      ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
    `).run(
      "other-session", "other-project", "coordinated", "active", 1, 1,
      SHA_A, "budget-ref", "launch", "launch-digest", 1,
      "operator-launch", "operator", 1, 1,
    );
    database.prepare(`
      INSERT INTO runs(
        run_id,chair_agent_id,workspace_root,created_at,project_session_id,
        lifecycle_state,revision,chair_generation,chair_lease_id,authority_ref,
        budget_ref,dependency_revision,topology_slot
      ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)
    `).run(
      "other-run", "other-chair", "/tmp/other", 1, "other-session", "active", 1, 1,
      "other-lease", SHA_A, "budget-ref", 1, 1,
    );
    database.prepare(`
      INSERT INTO authorities(authority_id,run_id,authority_json,authority_hash,created_at)
      VALUES (?,?,?,?,?)
    `).run("other-authority", "other-run", "{}", "a".repeat(64), 1);
    database.prepare(`
      INSERT INTO agents(run_id,agent_id,authority_id,lifecycle) VALUES (?,?,?,?)
    `).run("other-run", "other-chair", "other-authority", "ready");
    insertProviderActionPreflightParent(database, {
      runId: "run",
      adapterId: "adapter",
      actionId: "action",
      state: "resolving",
    });
    expect(() => database.prepare(`
      INSERT INTO provider_actions(
        run_id,action_id,adapter_id,operation,target_agent_id,
        provider_session_generation,identity_hash,payload_hash,payload_json,
        status,history_json,execution_count,effect_count,idempotency_proven,
        result_json,updated_at
      ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
    `).run(
      "other-run", "action", "adapter", "turn", "other-chair", 1,
      "identity", "payload", "{}", "prepared", "[]", 0, 0, 0, null, 1,
    )).toThrow("FOREIGN KEY constraint failed");

    database.prepare(`
      INSERT INTO review_finding_sets(
        finding_set_digest,finding_count,page_count,canonical_byte_length,created_at
      ) VALUES (?,?,?,?,?)
    `).run(EMPTY_FINDING_SET_DIGEST, 0, 0, Buffer.byteLength(EMPTY_FINDING_SET_JSON), 1);
    expect(() => database.prepare(`
      INSERT INTO review_finding_capacity_reservations(
        adapter_id,action_id,run_id,target_generation,slot,owner_digest,
        finding_window_mode,prior_open_finding_set_digest,
        maximum_new_findings,maximum_new_finding_bytes,reservation_digest,
        state,created_at,updated_at
      ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)
    `).run(
      "adapter", "action", "other-run", 1, "native", "owner", "normal",
      EMPTY_FINDING_SET_DIGEST, 32, 65_536, `sha256:${"b".repeat(64)}`,
      "preflight", 1, 1,
    )).toThrow("FOREIGN KEY constraint failed");

    database.close();
  });

  it("advances finding-capacity reservations only through the closed state graph", () => {
    const database = openDatabase();
    seedRun(database);
    database.prepare(`
      INSERT INTO review_finding_sets(
        finding_set_digest,finding_count,page_count,canonical_byte_length,created_at
      ) VALUES (?,?,?,?,?)
    `).run(
      EMPTY_FINDING_SET_DIGEST,
      0,
      0,
      Buffer.byteLength(EMPTY_FINDING_SET_JSON),
      1,
    );
    const reservationDigest = `sha256:${"b".repeat(64)}`;

    admitProviderActionFixture(database, {
      runId: "run",
      actionId: "action",
      adapterId: "adapter",
      operation: "turn",
      targetAgentId: "chair",
      providerSessionGeneration: 1,
      identityHash: "identity",
      payloadHash: "payload",
      payloadJson: "{}",
      status: "prepared",
      historyJson: "[]",
      executionCount: 0,
      updatedAt: 1,
    }, undefined, (ticket) => {
      database.prepare(`
        INSERT INTO review_finding_capacity_reservations(
          adapter_id,action_id,run_id,target_generation,slot,owner_digest,
          finding_window_mode,prior_open_finding_set_digest,
          maximum_new_findings,maximum_new_finding_bytes,reservation_digest,
          state,created_at,updated_at
        ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)
      `).run(
        "adapter", "action", "run", 1, "native", ticket.ownerDigest, "normal",
        EMPTY_FINDING_SET_DIGEST, 32, 65_536, reservationDigest,
        "preflight", 1, 1,
      );
      expect(() => database.prepare(`
        UPDATE review_finding_capacity_reservations
           SET state='settled',updated_at=2
         WHERE adapter_id='adapter' AND action_id='action'
      `).run()).toThrow("INVARIANT_review_finding_capacity_reservation_state");
    });
    database.prepare(`
      UPDATE provider_actions SET finding_capacity_reservation_digest=?
       WHERE adapter_id='adapter' AND action_id='action'
    `).run(reservationDigest);
    database.prepare(`
      UPDATE review_finding_capacity_reservations
         SET state='attached',updated_at=2
       WHERE adapter_id='adapter' AND action_id='action'
    `).run();
    expect(() => database.prepare(`
      UPDATE review_finding_capacity_reservations
         SET state='preflight',updated_at=3
       WHERE adapter_id='adapter' AND action_id='action'
    `).run()).toThrow("INVARIANT_review_finding_capacity_reservation_state");
    database.prepare(`
      UPDATE review_finding_capacity_reservations
         SET state='settled',updated_at=3
       WHERE adapter_id='adapter' AND action_id='action'
    `).run();
    expect(() => database.prepare(`
      UPDATE review_finding_capacity_reservations SET updated_at=4
       WHERE adapter_id='adapter' AND action_id='action'
    `).run()).toThrow("INVARIANT_review_finding_capacity_reservation_state");
    expect(() => database.prepare(`
      DELETE FROM review_finding_capacity_reservations
       WHERE adapter_id='adapter' AND action_id='action'
    `).run()).toThrow("INVARIANT_review_finding_capacity_reservation_state");

    database.close();
  });

  it("keys provider actions by the global adapter and action pair", () => {
    const database = openDatabase();
    seedRun(database);

    insertProviderAction(database, "adapter-a");
    insertProviderAction(database, "adapter-b");

    expect(database.prepare(`
      SELECT adapter_id FROM provider_actions
       WHERE run_id='run' AND action_id='shared-action'
       ORDER BY adapter_id
    `).all()).toEqual([{ adapter_id: "adapter-a" }, { adapter_id: "adapter-b" }]);

    database.close();
  });

  it("cannot bind a dependent provider action through the wrong adapter", () => {
    const database = openDatabase();
    seedRun(database);
    insertProviderAction(database, "adapter-a");

    expect(() => database.prepare(`
      INSERT INTO provider_lifecycle_intents(
        run_id,action_id,operation,actor_agent_id,target_agent_id,
        authority_id,adapter_id,intent_hash,status,created_at,updated_at
      ) VALUES (?,?,?,?,?,?,?,?,?,?,?)
    `).run(
      "run", "shared-action", "attach", "chair", "chair",
      "authority", "adapter-b", "intent", "prepared", 1, 1,
    )).toThrow("FOREIGN KEY constraint failed");

    database.close();
  });

  it("owns one immutable current coordination-policy revision", () => {
    const database = openDatabase();
    seedRun(database);

    database.prepare(`
      INSERT INTO coordination_policy_revisions(
        project_session_id,coordination_run_id,policy_revision,
        policy_ref,policy_digest,created_at
      ) VALUES (?,?,?,?,?,?)
    `).run("session", "run", 1, SHA_A, SHA_A, 1);
    database.prepare(`
      INSERT INTO coordination_policy_current(
        project_session_id,coordination_run_id,policy_revision,
        policy_ref,policy_digest,revision
      ) VALUES (?,?,?,?,?,?)
    `).run("session", "run", 1, SHA_A, SHA_A, 1);

    expect(() => database.prepare(`
      UPDATE coordination_policy_revisions SET policy_digest=?
       WHERE project_session_id=? AND coordination_run_id=? AND policy_revision=?
    `).run(`sha256:${"b".repeat(64)}`, "session", "run", 1))
      .toThrow("INVARIANT_coordination_policy_history_immutable");

    database.close();
  });

  it("admits at most one policy revision ahead of the current pointer", () => {
    const database = openDatabase();
    seedRun(database);
    database.prepare(`
      INSERT INTO coordination_policy_revisions(
        project_session_id,coordination_run_id,policy_revision,
        policy_ref,policy_digest,created_at
      ) VALUES (?,?,?,?,?,?)
    `).run("session", "run", 1, SHA_A, SHA_A, 1);
    database.prepare(`
      INSERT INTO coordination_policy_current(
        project_session_id,coordination_run_id,policy_revision,
        policy_ref,policy_digest,revision
      ) VALUES (?,?,?,?,?,?)
    `).run("session", "run", 1, SHA_A, SHA_A, 1);
    database.prepare(`
      INSERT INTO coordination_policy_revisions(
        project_session_id,coordination_run_id,policy_revision,
        policy_ref,policy_digest,created_at
      ) VALUES (?,?,?,?,?,?)
    `).run("session", "run", 2, SHA_A, SHA_A, 2);

    expect(() => database.prepare(`
      INSERT INTO coordination_policy_revisions(
        project_session_id,coordination_run_id,policy_revision,
        policy_ref,policy_digest,created_at
      ) VALUES (?,?,?,?,?,?)
    `).run("session", "run", 3, SHA_A, SHA_A, 3))
      .toThrow("INVARIANT_coordination_policy_revision_contiguous");

    expect(() => database.prepare(`
      UPDATE coordination_policy_current
         SET policy_revision=2,revision=2
       WHERE project_session_id='session' AND coordination_run_id='run'
    `).run()).not.toThrow();

    database.close();
  });

  it("rejects a topology plan bound to a noncurrent policy revision", () => {
    const database = openDatabase();
    seedTopologyCurrency(database);
    database.prepare(`
      INSERT INTO coordination_policy_revisions(
        project_session_id,coordination_run_id,policy_revision,
        policy_ref,policy_digest,created_at
      ) VALUES (?,?,?,?,?,?)
    `).run("session", "run", 2, SHA_A, SHA_A, 2);

    expect(() => insertTopologyPlan(database, topologyPlan(2), 2))
      .toThrow("INVARIANT_topology_wave_plan_currency");

    database.close();
  });

  it("requires the topology current pointer to start at the exact first plan", () => {
    const database = openDatabase();
    seedTopologyCurrency(database);
    const plan = topologyPlan();
    insertTopologyPlan(database, plan);

    expect(() => database.prepare(`
      INSERT INTO topology_wave_current(
        project_session_id,coordination_run_id,task_id,
        wave_id,wave_revision,plan_digest,revision
      ) VALUES (?,?,?,?,?,?,?)
    `).run("session", "run", "task", "wave", 1, plan.digest, 99))
      .toThrow("INVARIANT_topology_wave_current_cas");

    database.close();
  });

  it("rejects noncanonical or open nested topology-plan objects", () => {
    const database = openDatabase();
    seedTopologyCurrency(database);
    const value = JSON.parse(topologyPlan().json) as Record<string, unknown>;
    const chair = value.chair as Record<string, unknown>;
    chair.unowned = true;
    delete value.planDigest;
    const planDigest = digest(value);
    value.planDigest = planDigest;

    expect(() => insertTopologyPlan(database, {
      json: canonicalJson(value),
      digest: planDigest,
    })).toThrow("INVARIANT_topology_wave_plan_codec");

    database.close();
  });

  it("verifies the topology plan digest over canonical JCS without planDigest", () => {
    const database = openDatabase();
    seedTopologyCurrency(database);
    const valid = topologyPlan();
    const forgedDigest = `sha256:${"b".repeat(64)}`;
    const forgedBody = JSON.parse(valid.json) as Record<string, unknown>;
    forgedBody.planDigest = forgedDigest;

    expect(() => insertTopologyPlan(database, {
      json: canonicalJson(forgedBody),
      digest: forgedDigest,
    })).toThrow("INVARIANT_topology_wave_plan_codec");

    database.close();
  });

  it("rejects topology plans whose nested owner is not a current run agent", () => {
    const database = openDatabase();
    seedTopologyCurrency(database);
    const body = JSON.parse(topologyPlan().json) as Record<string, unknown>;
    delete body.planDigest;
    const stageOwners = body.stageOwners as Array<Record<string, unknown>>;
    const writePartitions = body.writePartitions as Array<Record<string, unknown>>;
    if (stageOwners[0] === undefined || writePartitions[0] === undefined) {
      throw new Error("topology fixture is incomplete");
    }
    stageOwners[0].ownerAgentId = "forged-agent";
    writePartitions[0].ownerAgentId = "forged-agent";
    const planDigest = digest(body);

    expect(() => insertTopologyPlan(database, {
      json: canonicalJson({ ...body, planDigest }),
      digest: planDigest,
    })).toThrow("INVARIANT_topology_wave_plan_currency");

    database.close();
  });

  it("rejects topology plans whose nested task is outside the current run", () => {
    const database = openDatabase();
    seedTopologyCurrency(database);
    const body = JSON.parse(topologyPlan().json) as Record<string, unknown>;
    delete body.planDigest;
    const stageOwners = body.stageOwners as Array<Record<string, unknown>>;
    if (stageOwners[0] === undefined) throw new Error("topology fixture is incomplete");
    stageOwners[0].taskId = "forged-task";
    const planDigest = digest(body);

    expect(() => insertTopologyPlan(database, {
      json: canonicalJson({ ...body, planDigest }),
      digest: planDigest,
    })).toThrow("INVARIANT_topology_wave_plan_currency");

    database.close();
  });

  it("rejects topology plans whose nested evidence is not active project evidence", () => {
    const database = openDatabase();
    seedTopologyCurrency(database);
    const body = JSON.parse(topologyPlan().json) as Record<string, unknown>;
    delete body.planDigest;
    const decomposability = body.decomposability as Record<string, unknown>;
    const contention = body.contention as Record<string, unknown>;
    decomposability.evidenceRef = `sha256:${"b".repeat(64)}`;
    contention.evidenceRef = `sha256:${"b".repeat(64)}`;
    const planDigest = digest(body);

    expect(() => insertTopologyPlan(database, {
      json: canonicalJson({ ...body, planDigest }),
      digest: planDigest,
    })).toThrow("INVARIANT_topology_wave_plan_currency");

    database.close();
  });

  it("rejects topology write owners outside the current plan authority", () => {
    const database = openDatabase();
    seedTopologyCurrency(database);
    database.prepare(`
      INSERT INTO authorities(authority_id,run_id,authority_json,authority_hash,created_at)
      VALUES (?,?,?,?,?)
    `).run("foreign-authority", "run", "{}", "b".repeat(64), 1);
    database.prepare(`
      INSERT INTO agents(run_id,agent_id,authority_id,lifecycle) VALUES (?,?,?,?)
    `).run("run", "foreign-owner", "foreign-authority", "ready");
    const body = JSON.parse(topologyPlan().json) as Record<string, unknown>;
    delete body.planDigest;
    const stageOwners = body.stageOwners as Array<Record<string, unknown>>;
    const writePartitions = body.writePartitions as Array<Record<string, unknown>>;
    if (stageOwners[0] === undefined || writePartitions[0] === undefined) {
      throw new Error("topology fixture is incomplete");
    }
    stageOwners[0].ownerAgentId = "foreign-owner";
    writePartitions[0].ownerAgentId = "foreign-owner";
    const planDigest = digest(body);

    expect(() => insertTopologyPlan(database, {
      json: canonicalJson({ ...body, planDigest }),
      digest: planDigest,
    })).toThrow("INVARIANT_topology_wave_plan_currency");

    database.close();
  });

  it("rejects topology dependencies whose required task state is not current", () => {
    const database = openDatabase();
    seedTopologyCurrency(database);
    database.prepare(`
      INSERT INTO tasks(
        run_id,task_id,authority_id,objective,base_revision,state,revision,created_by
      ) VALUES (?,?,?,?,?,?,?,?)
    `).run("run", "dependency", "authority", "dependency", "base", "active", 1, "chair");
    const body = JSON.parse(topologyPlan().json) as Record<string, unknown>;
    delete body.planDigest;
    body.dependencies = [{
      dependencyTaskId: "dependency",
      evidenceRef: SHA_A,
      requiredState: "ready",
    }];
    const planDigest = digest(body);

    expect(() => insertTopologyPlan(database, {
      json: canonicalJson({ ...body, planDigest }),
      digest: planDigest,
    })).toThrow("INVARIANT_topology_wave_plan_currency");

    database.close();
  });

  it("rejects a topology rationale artifact outside the current project", () => {
    const database = openDatabase();
    seedTopologyCurrency(database);
    database.prepare(`
      INSERT INTO projects(
        project_id,canonical_root,revision,authority_generation,created_at,updated_at
      ) VALUES (?,?,?,?,?,?)
    `).run("foreign-project", "/tmp/foreign", 1, 1, 1, 1);
    database.prepare(`
      INSERT INTO artifacts(
        artifact_id,project_id,publisher_kind,publisher_ref,source_kind,evidence_kind,
        relative_path,sha256,registry_state,revision,created_at
      ) VALUES (?,?,?,?,?,?,?,?,?,?,?)
    `).run(
      "foreign-rationale", "foreign-project", "operator", "operator",
      "project-file", "artifact", "foreign.txt", SHA_A, "active", 1, 1,
    );
    const body = JSON.parse(topologyPlan().json) as Record<string, unknown>;
    delete body.planDigest;
    body.rationaleRef = { evidenceId: "foreign-rationale", evidenceRevision: 1 };
    const planDigest = digest(body);

    expect(() => insertTopologyPlan(database, {
      json: canonicalJson({ ...body, planDigest }),
      digest: planDigest,
    }, 1, "foreign-rationale")).toThrow("INVARIANT_topology_wave_plan_currency");

    database.close();
  });

  it("rejects a recovery capability issue whose immutable source identity does not exist", () => {
    const database = openDatabase();
    seedRun(database);

    expect(() => database.prepare(`
      INSERT INTO agent_lifecycle_recovery_capability_issues(
        issue_id,capability_hash,operator_id,project_id,project_session_id,run_id,agent_id,
        session_revision,session_generation,run_revision,recovery_source_kind,
        old_custody_id,old_action_adapter_id,old_action_id,old_custody_revision,
        generation_loss_id,generation_loss_revision,recovery_source_ref_digest,
        source_journal_digest,checkpoint_digest,
        source_provider_session_ref,source_capability_hash,source_custody_action_id,
        source_adapter_id,source_adapter_contract_digest,source_bridge_row_id,
        source_bridge_revision,source_provider_generation,source_principal_generation,
        source_bridge_generation,source_project_session_generation,source_run_generation,
        source_chair_lease_generation,bridge_owner_kind,parent_capability_id,
        consequential_gate_id,path,issuance_json,issuance_digest,issued_at,expires_at
      ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
    `).run(
      "issue", "capability-hash", "operator", "forged-project", "forged-session",
      "run", "chair", 999, 999, 999, "generation-loss", null, null, null, null,
      "missing-loss", 999, "forged-source-ref", "forged-journal", SHA_A,
      "forged-provider-session", "forged-source-capability", "forged-action",
      "forged-adapter", SHA_A, "forged-bridge", 999, 999, 999, 999,
      999, 999, 999, "chair", "missing-parent", "missing-gate",
      "fresh-rotate", "{}", "forged-issue-digest", RECOVERY_ISSUED_AT,
      RECOVERY_EXPIRES_AT,
    )).toThrow("FOREIGN KEY constraint failed");

    database.close();
  });

  it("binds an immutable recovery issue to one exact loss and one revocation", () => {
    const database = openDatabase();
    seedRun(database);
    seedGenerationLossRecovery(database);

    expect(() => insertGenerationLossRecoveryIssue(database, 2))
      .toThrow("FOREIGN KEY constraint failed");
    insertGenerationLossRecoveryIssue(database);
    expect(database.prepare(`
      SELECT issue_id,head_revision FROM agent_lifecycle_recovery_source_heads
    `).get()).toEqual({ issue_id: "issue", head_revision: 1 });
    expect(() => database.prepare(`
      UPDATE agent_lifecycle_recovery_capability_issues SET expires_at=expires_at+1
       WHERE issue_id='issue'
    `).run()).toThrow("lifecycle-immutable-row");
    database.prepare(`
      INSERT INTO agent_lifecycle_recovery_issue_revocations(
        issue_id,revocation_kind,evidence_digest,revoked_at
      ) VALUES ('issue','operator-revoked',?,?)
    `).run(SHA_A, RECOVERY_NOW);
    expect(() => database.prepare(`
      INSERT INTO agent_lifecycle_recovery_issue_revocations(
        issue_id,revocation_kind,evidence_digest,revoked_at
      ) VALUES ('issue','source-stale',?,?)
    `).run(SHA_A, RECOVERY_NOW + 1)).toThrow("UNIQUE constraint failed");
    expect(() => database.prepare(`
      UPDATE agent_lifecycle_recovery_issue_revocations SET revoked_at=revoked_at+1
       WHERE issue_id='issue'
    `).run()).toThrow("lifecycle-immutable-row");

    database.close();
  });

  it("projects only active unexpired lifecycle recovery issues as current", () => {
    const database = openDatabase();
    seedRun(database);
    seedGenerationLossRecovery(database);
    insertGenerationLossRecoveryIssue(database);

    expect(currentGenerationLossRecoveryIssues(database, RECOVERY_NOW))
      .toEqual([{ issue_id: "issue" }]);

    database.prepare(`
      INSERT INTO agent_lifecycle_recovery_issue_revocations(
        issue_id,revocation_kind,evidence_digest,revoked_at
      ) VALUES ('issue','operator-revoked',?,?)
    `).run(SHA_A, RECOVERY_NOW);
    expect(currentGenerationLossRecoveryIssues(database, RECOVERY_NOW)).toEqual([]);

    database.close();

    const expiredDatabase = openDatabase();
    seedRun(expiredDatabase);
    seedGenerationLossRecovery(expiredDatabase);
    insertGenerationLossRecoveryIssue(
      expiredDatabase,
      1,
      RECOVERY_NOW - 120_000,
      RECOVERY_NOW - 60_000,
    );

    expect(currentGenerationLossRecoveryIssues(expiredDatabase, RECOVERY_NOW)).toEqual([]);
    expect(() => expiredDatabase.prepare(`
      INSERT INTO lifecycle_fresh_recovery_handoffs(issue_id,created_at)
      VALUES ('issue',?)
    `).run(RECOVERY_NOW)).toThrow("LIFECYCLE_RECOVERY_ISSUE_NOT_CURRENT");

    expiredDatabase.close();
  });

  it("refuses to consume a recovery issue after its parent capability and issue are revoked", () => {
    const database = openDatabase();
    seedRun(database);
    seedGenerationLossRecovery(database);
    insertGenerationLossRecoveryIssue(database);
    database.prepare(`
      UPDATE operator_capabilities SET revoked_at=11
       WHERE capability_id='parent-capability'
    `).run();
    database.prepare(`
      INSERT INTO agent_lifecycle_recovery_issue_revocations(
        issue_id,revocation_kind,evidence_digest,revoked_at
      ) VALUES ('issue','operator-revoked',?,11)
    `).run(SHA_A);

    expect(() => database.prepare(`
      INSERT INTO lifecycle_fresh_recovery_handoffs(issue_id,created_at)
      VALUES ('issue',12)
    `).run()).toThrow("LIFECYCLE_RECOVERY_ISSUE_NOT_CURRENT");

    database.close();
  });

  it("rejects lifecycle identity high-water rollback without a revision advance", () => {
    const database = openDatabase();
    seedRun(database);
    database.prepare(`
      INSERT INTO agent_lifecycle_identity_high_water(
        run_id,agent_id,provider_generation,principal_generation,revision
      ) VALUES (?,?,?,?,?)
    `).run("run", "chair", 10, 10, 1);

    expect(() => database.prepare(`
      UPDATE agent_lifecycle_identity_high_water
         SET provider_generation=1,principal_generation=1,revision=1
       WHERE run_id='run' AND agent_id='chair'
    `).run()).toThrow("INVARIANT_agent_lifecycle_identity_high_water_cas");

    database.close();
  });

  it("advances identity, bridge, and context high-water rows only through monotonic CAS", () => {
    const database = openDatabase();
    seedRun(database);
    database.prepare(`
      INSERT INTO agent_lifecycle_identity_high_water(
        run_id,agent_id,provider_generation,principal_generation,revision
      ) VALUES ('run','chair',10,10,1)
    `).run();
    database.prepare(`
      UPDATE agent_lifecycle_identity_high_water
         SET provider_generation=11,revision=2
       WHERE run_id='run' AND agent_id='chair'
    `).run();
    database.prepare(`
      UPDATE agent_lifecycle_identity_high_water
         SET provider_generation=13,revision=3
       WHERE run_id='run' AND agent_id='chair'
    `).run();
    expect(database.prepare(`
      SELECT provider_generation,revision
        FROM agent_lifecycle_identity_high_water
       WHERE run_id='run' AND agent_id='chair'
    `).get()).toEqual({ provider_generation: 13, revision: 3 });

    database.prepare(`
      INSERT INTO agent_lifecycle_bridge_high_water(
        run_id,agent_id,bridge_owner_kind,bridge_generation,revision
      ) VALUES ('run','chair','chair',5,1)
    `).run();
    database.prepare(`
      UPDATE agent_lifecycle_bridge_high_water
         SET bridge_generation=6,revision=2
       WHERE run_id='run' AND agent_id='chair' AND bridge_owner_kind='chair'
    `).run();
    expect(() => database.prepare(`
      UPDATE agent_lifecycle_bridge_high_water
         SET bridge_generation=6,revision=3
       WHERE run_id='run' AND agent_id='chair' AND bridge_owner_kind='chair'
    `).run()).toThrow("INVARIANT_agent_lifecycle_bridge_high_water_cas");

    database.prepare(`
      INSERT INTO agent_lifecycle_context_high_water(
        run_id,agent_id,provider_generation,context_revision,revision
      ) VALUES ('run','chair',11,5,1)
    `).run();
    database.prepare(`
      UPDATE agent_lifecycle_context_high_water
         SET context_revision=7,revision=2
       WHERE run_id='run' AND agent_id='chair' AND provider_generation=11
    `).run();
    expect(() => database.prepare(`
      UPDATE agent_lifecycle_context_high_water
         SET context_revision=6,revision=3
       WHERE run_id='run' AND agent_id='chair' AND provider_generation=11
    `).run()).toThrow("INVARIANT_agent_lifecycle_context_high_water_cas");

    database.close();
  });

  it("keeps committed finding members immutable under their content digest", () => {
    const database = openDatabase();
    const graph = findingGraph();
    const finding = graph.finding;
    database.prepare(`
      INSERT INTO review_finding_pages(
        page_digest,member_count,canonical_byte_length,private_page_path,created_at
      ) VALUES (?,?,?,?,?)
    `).run(graph.pageDigest, 1, graph.pageByteLength, "/private/page", 1);
    database.prepare(`
      INSERT INTO review_finding_members(
        page_digest,member_ordinal,finding_digest,finding_id,severity,safe_record_json
      ) VALUES (?,?,?,?,?,?)
    `).run(graph.pageDigest, 0, finding.digest, "F-1", "P2", finding.json);

    expect(() => database.prepare(`
      UPDATE review_finding_members
         SET severity='P0',safe_record_json='{"forged":true}'
       WHERE page_digest=? AND member_ordinal=0
    `).run(graph.pageDigest)).toThrow("INVARIANT_review_finding_graph_immutable");

    database.close();
  });

  it("closes finding pages with contiguous members and exact root ranges", () => {
    const database = openDatabase();
    const graph = findingGraph();
    const finding = graph.finding;
    database.prepare(`
      INSERT INTO review_finding_sets(
        finding_set_digest,finding_count,page_count,canonical_byte_length,created_at
      ) VALUES (?,?,?,?,?)
    `).run(graph.setDigest, 1, 1, graph.setByteLength, 1);
    database.prepare(`
      INSERT INTO review_finding_pages(
        page_digest,member_count,canonical_byte_length,private_page_path,created_at
      ) VALUES (?,?,?,?,?)
    `).run(graph.pageDigest, 1, graph.pageByteLength, "/private/page", 1);
    database.prepare(`
      INSERT INTO review_finding_members(
        page_digest,member_ordinal,finding_digest,finding_id,severity,safe_record_json
      ) VALUES (?,?,?,?,?,?)
    `).run(graph.pageDigest, 0, finding.digest, "F-1", "P2", finding.json);
    database.prepare(`
      INSERT INTO review_finding_set_pages(
        finding_set_digest,ordinal,page_digest,member_count,
        first_finding_digest,last_finding_digest
      ) VALUES (?,?,?,?,?,?)
    `).run(graph.setDigest, 0, graph.pageDigest, 1, finding.digest, finding.digest);

    expect(database.prepare(`
      SELECT finding_set_digest FROM review_finding_sets_complete
    `).all()).toEqual([{ finding_set_digest: graph.setDigest }]);

    expect(() => database.prepare(`
      INSERT INTO review_finding_members(
        page_digest,member_ordinal,finding_digest,finding_id,severity,safe_record_json
      ) VALUES (?,?,?,?,?,?)
    `).run(graph.pageDigest, 1, finding.digest, "F-2", "P2", finding.json))
      .toThrow("INVARIANT_review_finding_member_closed");

    database.close();
  });

  it("admits only the canonical empty finding-set root and byte length", () => {
    const database = openDatabase();

    expect(() => database.prepare(`
      INSERT INTO review_finding_sets(
        finding_set_digest,finding_count,page_count,canonical_byte_length,created_at
      ) VALUES (?,?,?,?,?)
    `).run(
      EMPTY_FINDING_SET_DIGEST,
      0,
      0,
      Buffer.byteLength(EMPTY_FINDING_SET_JSON),
      1,
    )).not.toThrow();

    expect(() => database.prepare(`
      INSERT INTO review_finding_sets(
        finding_set_digest,finding_count,page_count,canonical_byte_length,created_at
      ) VALUES (?,?,?,?,?)
    `).run(
      `sha256:${"0".repeat(64)}`,
      0,
      0,
      Buffer.byteLength(EMPTY_FINDING_SET_JSON),
      1,
    )).toThrow("CHECK constraint failed");
    expect(() => database.prepare(`
      INSERT INTO review_finding_sets(
        finding_set_digest,finding_count,page_count,canonical_byte_length,created_at
      ) VALUES (?,?,?,?,?)
    `).run(EMPTY_FINDING_SET_DIGEST, 0, 0, 0, 1)).toThrow("CHECK constraint failed");

    database.close();
  });

  it("rejects malformed finding digests and false canonical byte lengths", () => {
    const database = openDatabase();
    const graph = findingGraph();
    const malformed = JSON.parse(graph.finding.json) as Record<string, unknown>;
    malformed.originResultDigest = "not-a-digest";
    malformed.findingDigest = SHA_A;
    database.prepare(`
      INSERT INTO review_finding_pages(
        page_digest,member_count,canonical_byte_length,private_page_path,created_at
      ) VALUES (?,?,?,?,?)
    `).run(graph.pageDigest, 1, graph.pageByteLength, "/private/page", 1);

    expect(() => database.prepare(`
      INSERT INTO review_finding_members(
        page_digest,member_ordinal,finding_digest,finding_id,severity,safe_record_json
      ) VALUES (?,?,?,?,?,?)
    `).run(
      graph.pageDigest, 0, SHA_A, "F-1", "P2", canonicalJson(malformed),
    )).toThrow("INVARIANT_review_finding_member_closed");

    database.close();

    const lengthDatabase = openDatabase();
    lengthDatabase.prepare(`
      INSERT INTO review_finding_sets(
        finding_set_digest,finding_count,page_count,canonical_byte_length,created_at
      ) VALUES (?,?,?,?,?)
    `).run(graph.setDigest, 1, 1, graph.setByteLength, 1);
    lengthDatabase.prepare(`
      INSERT INTO review_finding_pages(
        page_digest,member_count,canonical_byte_length,private_page_path,created_at
      ) VALUES (?,?,?,?,?)
    `).run(graph.pageDigest, 1, graph.pageByteLength - 1, "/private/page", 1);
    lengthDatabase.prepare(`
      INSERT INTO review_finding_members(
        page_digest,member_ordinal,finding_digest,finding_id,severity,safe_record_json
      ) VALUES (?,?,?,?,?,?)
    `).run(
      graph.pageDigest, 0, graph.finding.digest, "F-1", "P2", graph.finding.json,
    );

    expect(() => lengthDatabase.prepare(`
      INSERT INTO review_finding_set_pages(
        finding_set_digest,ordinal,page_digest,member_count,
        first_finding_digest,last_finding_digest
      ) VALUES (?,?,?,?,?,?)
    `).run(
      graph.setDigest, 0, graph.pageDigest, 1,
      graph.finding.digest, graph.finding.digest,
    )).toThrow("INVARIANT_review_finding_set_page_closed");

    lengthDatabase.close();

    const rootLengthDatabase = openDatabase();
    rootLengthDatabase.prepare(`
      INSERT INTO review_finding_sets(
        finding_set_digest,finding_count,page_count,canonical_byte_length,created_at
      ) VALUES (?,?,?,?,?)
    `).run(graph.setDigest, 1, 1, graph.setByteLength - 1, 1);
    rootLengthDatabase.prepare(`
      INSERT INTO review_finding_pages(
        page_digest,member_count,canonical_byte_length,private_page_path,created_at
      ) VALUES (?,?,?,?,?)
    `).run(graph.pageDigest, 1, graph.pageByteLength, "/private/page", 1);
    rootLengthDatabase.prepare(`
      INSERT INTO review_finding_members(
        page_digest,member_ordinal,finding_digest,finding_id,severity,safe_record_json
      ) VALUES (?,?,?,?,?,?)
    `).run(
      graph.pageDigest, 0, graph.finding.digest, "F-1", "P2", graph.finding.json,
    );

    expect(() => rootLengthDatabase.prepare(`
      INSERT INTO review_finding_set_pages(
        finding_set_digest,ordinal,page_digest,member_count,
        first_finding_digest,last_finding_digest
      ) VALUES (?,?,?,?,?,?)
    `).run(
      graph.setDigest, 0, graph.pageDigest, 1,
      graph.finding.digest, graph.finding.digest,
    )).toThrow("INVARIANT_review_finding_set_page_closed");

    rootLengthDatabase.close();
  });

  it("rejects consumers of an incomplete finding-set graph", () => {
    const database = openDatabase();
    seedRun(database);
    const incompleteSet = `sha256:${"e".repeat(64)}`;
    database.prepare(`
      INSERT INTO review_finding_sets(
        finding_set_digest,finding_count,page_count,canonical_byte_length,created_at
      ) VALUES (?,?,?,?,?)
    `).run(incompleteSet, 1, 1, 1, 1);
    database.prepare(`
      INSERT INTO provider_action_pair_preflights(
        adapter_id,action_id,scope_kind,run_id,owner_digest,actor_principal_digest,
        input_digest,state,created_at,updated_at
      ) VALUES (?,?,'run-action',?,?,?,?,?,?,?)
    `).run(
      "adapter", "action", "run", "owner", "principal", "input",
      "resolving", 1, 1,
    );

    expect(() => database.prepare(`
      INSERT INTO review_finding_capacity_reservations(
        adapter_id,action_id,run_id,target_generation,slot,owner_digest,
        finding_window_mode,prior_open_finding_set_digest,
        maximum_new_findings,maximum_new_finding_bytes,reservation_digest,
        state,created_at,updated_at
      ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)
    `).run(
      "adapter", "action", "run", 1, "native", "owner", "normal",
      incompleteSet, 32, 65536, "reservation", "preflight", 1, 1,
    )).toThrow("INVARIANT_review_finding_set_incomplete");

    database.close();
  });

  it("rejects a topology plan whose closed body and digest are malformed", () => {
    const database = openDatabase();
    seedTopologyCurrency(database);

    expect(() => database.prepare(`
      INSERT INTO topology_wave_plans(
        project_session_id,coordination_run_id,task_id,wave_id,wave_revision,
        chair_agent_id,principal_generation,chair_lease_generation,
        authority_revision,authority_ref,authority_digest,
        policy_revision,policy_ref,policy_digest,
        rationale_evidence_id,rationale_evidence_revision,state,
        plan_json,plan_digest,created_at
      ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
    `).run(
      "session", "run", "task", "wave", 1,
      "chair", 1, 1,
      1, SHA_A, SHA_A,
      1, SHA_A, SHA_A,
      "rationale", 1, "approved",
      "{}", "not-a-digest", 1,
    )).toThrow("INVARIANT_topology_wave_plan_codec");

    database.close();
  });
});
