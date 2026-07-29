import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { applyMigrations } from "../../src/core/migrations.ts";

const openDatabases: Database.Database[] = [];

function openDatabase(): Database.Database {
  const database = new Database(":memory:");
  openDatabases.push(database);
  return database;
}

afterEach(() => {
  for (const database of openDatabases.splice(0)) database.close();
});

describe("current schema baseline", () => {
  it("installs the complete Spec 04 review, lifecycle, route and topology catalogue without predecessor review stores", () => {
    const database = openDatabase();
    applyMigrations(database);

    const tableNames = new Set((database.prepare(`
      SELECT name FROM sqlite_schema WHERE type = 'table'
    `).all() as { name: string }[]).map(({ name }) => name));
    const requiredTables = [
      "adapter_activation_subjects",
      "adapter_capability_current",
      "adapter_capability_snapshots",
      "adapter_effective_configurations",
      "adapter_provider_smoke_subjects",
      "agent_lifecycle_bridge_high_water",
      "agent_lifecycle_context_high_water",
      "agent_lifecycle_identity_high_water",
      "agent_lifecycle_recovery_capability_issues",
      "agent_lifecycle_recovery_issue_revocations",
      "agent_lifecycle_recovery_retirements",
      "agent_lifecycle_recovery_source_heads",
      "artifact_publication_lineage",
      "coordination_policy_current",
      "coordination_policy_revisions",
      "coordination_gate_snapshots",
      "delivery_requirement_maps",
      "delivery_review_bases",
      "delivery_run_starts",
      "discovery_surface_manifests",
      "implementation_delivery_manifests",
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
      "provider_action_pair_preflights",
      "provider_action_route_dispatches",
      "provider_action_route_observations",
      "provider_action_routes",
      "provider_context_observation_audit",
      "provider_context_pressure_current",
      "provider_failure_substitution_events",
      "provider_review_evidence",
      "provider_review_results",
      "provider_review_terminal_journal",
      "provider_session_lineage",
      "review_bundle_chunks",
      "review_bundle_manifest_pages",
      "review_bundle_objects",
      "review_bundles",
      "review_certification_cuts",
      "review_certifying_slot_availability_heads",
      "review_certifying_slot_availability_revisions",
      "review_completion_targets",
      "review_evidence_annotation_heads",
      "review_evidence_annotations",
      "review_finding_capacity_reservations",
      "review_finding_members",
      "review_finding_pages",
      "review_finding_set_pages",
      "review_finding_sets",
      "review_profile_slots",
      "review_profile_snapshots",
      "review_slot_heads",
      "review_target_chair_binding_heads",
      "review_target_chair_bindings",
      "review_target_preparation_high_water",
      "review_target_preparations",
      "review_target_rebind_receipts",
      "review_terminal_sequence_high_water",
      "route_integrity_recoveries",
      "topology_wave_append_receipts",
      "topology_wave_current",
      "topology_wave_plans",
    ] as const;
    expect(requiredTables.filter((table) => !tableNames.has(table))).toStrictEqual([]);

    expect(database.prepare(`
      SELECT name FROM sqlite_schema
       WHERE type = 'view' AND name = 'review_finding_sets_complete'
    `).get()).toEqual({ name: "review_finding_sets_complete" });

    for (const predecessorTable of [
      "cross_family_review_evidence",
      "cross_family_reviews",
      "model_routing_evidence",
      "model_routing_receipts",
      "provider_review_packets",
    ]) {
      expect(tableNames.has(predecessorTable), predecessorTable).toBe(false);
    }

    const indexNames = new Set((database.prepare(`
      SELECT name FROM sqlite_schema WHERE type = 'index'
    `).all() as { name: string }[]).map(({ name }) => name));
    for (const requiredIndex of [
      "one_active_review_target_preparation_per_run",
      "one_effective_configuration_per_activation_subject",
      "one_effective_configuration_per_provider_action",
      "one_effective_configuration_per_smoke_subject",
      "one_nonfinal_custody_per_delivery_generation",
      "one_nonfinal_lifecycle_custody_per_agent",
      "one_nonterminal_generation_loss_per_agent",
    ]) {
      expect(indexNames.has(requiredIndex), requiredIndex).toBe(true);
    }
    expect(indexNames.has("lifecycle_rotation_one_active_per_agent")).toBe(false);
  });

  it("makes immutable review, capability, route and topology history insert-only", () => {
    const database = openDatabase();
    applyMigrations(database);

    const triggerNames = new Set((database.prepare(`
      SELECT name FROM sqlite_schema WHERE type = 'trigger'
    `).all() as { name: string }[]).map(({ name }) => name));
    for (const table of [
      "adapter_capability_snapshots",
      "adapter_effective_configurations",
      "agent_lifecycle_recovery_capability_issues",
      "agent_lifecycle_recovery_issue_revocations",
      "artifact_publication_lineage",
      "coordination_policy_revisions",
      "coordination_gate_snapshots",
      "discovery_surface_manifests",
      "implementation_delivery_manifests",
      "lifecycle_generation_loss_revisions",
      "lifecycle_generation_losses",
      "lifecycle_rotation_custodies",
      "lifecycle_rotation_custody_revisions",
      "provider_action_route_dispatches",
      "provider_action_route_observations",
      "provider_action_routes",
      "provider_context_observation_audit",
      "provider_review_evidence",
      "provider_review_results",
      "provider_review_terminal_journal",
      "review_bundles",
      "review_certification_cuts",
      "review_evidence_annotations",
      "review_finding_members",
      "review_finding_pages",
      "review_finding_set_pages",
      "review_finding_sets",
      "review_target_chair_bindings",
      "topology_wave_append_receipts",
      "topology_wave_plans",
    ]) {
      expect(triggerNames.has(`${table}_immutable_update`), table).toBe(true);
      expect(triggerNames.has(`${table}_immutable_delete`), table).toBe(true);
    }
  });

  it("installs point-of-use route equality and contiguous-dispatch guards", () => {
    const database = openDatabase();
    applyMigrations(database);
    const triggerNames = new Set((database.prepare(`
      SELECT name FROM sqlite_schema WHERE type = 'trigger'
    `).all() as { name: string }[]).map(({ name }) => name));
    for (const trigger of [
      "provider_action_route_dispatches_point_of_use",
      "provider_action_route_observations_parent_equality",
    ]) expect(triggerNames.has(trigger), trigger).toBe(true);
  });

  it("uses the exact current capability-source and topology-state catalogues", () => {
    const database = openDatabase();
    applyMigrations(database);
    const tableSql = (table: string): string => (database.prepare(`
      SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?
    `).pluck().get(table) as string).replaceAll(/\s+/gu, "");

    expect(tableSql("adapter_capability_snapshots")).toContain(
      "'runtime-discovery','version-pinned-conformance','unavailable'",
    );
    expect(tableSql("topology_wave_plans")).toContain(
      "'proposed','approved','started','completed','superseded','cancelled'",
    );
  });

  it("stores the complete immutable lifecycle terminal and generation-loss evidence arms", () => {
    const database = openDatabase();
    applyMigrations(database);
    const tableSql = (table: string): string => (database.prepare(`
      SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?
    `).pluck().get(table) as string).replaceAll(/\s+/gu, "");
    expect(tableSql("lifecycle_rotation_custody_revisions")).toContain(
      "'awaiting-boundary','prepared','dispatched','accepted','ambiguous','provider-terminal','committing','finalized'",
    );
    const lossRevisionColumns = database.prepare(`
      SELECT name, "notnull" AS is_not_null
        FROM pragma_table_info('lifecycle_generation_loss_revisions')
    `).all() as { name: string; is_not_null: 0 | 1 }[];
    expect(lossRevisionColumns).toContainEqual({ name: "terminal_evidence_digest", is_not_null: 0 });
    expect(tableSql("lifecycle_generation_loss_revisions")).toContain(
      "'open','recovery-in-progress','recovered-adopted','abandoned'",
    );
    const lossIdentityColumns = database.prepare(`
      SELECT name, "notnull" AS is_not_null
        FROM pragma_table_info('lifecycle_generation_losses')
    `).all() as { name: string; is_not_null: 0 | 1 }[];
    expect(lossIdentityColumns).toContainEqual({ name: "old_context_revision", is_not_null: 0 });
  });

  it("closes route-integrity reason and terminal-disposition vocabularies", () => {
    const database = openDatabase();
    applyMigrations(database);
    const sql = (database.prepare(`
      SELECT sql FROM sqlite_schema
       WHERE type = 'table' AND name = 'route_integrity_recoveries'
    `).pluck().get() as string).replaceAll(/\s+/gu, "");
    expect(sql).toContain(
      "'intact-effect-ambiguity','route-row-missing','route-row-conflict','route-receipt-mismatch','target-binding-invalid','bundle-binding-invalid','prompt-binding-invalid','profile-binding-invalid','lineage-binding-invalid'",
    );
    expect(sql).toContain(
      "'proved-no-effect-release','exact-usage-settled','conservative-full-ceiling-settled','full-ceiling-retired'",
    );
  });

  it("uses the exact provider-failure and reviewer-family vocabularies", () => {
    const database = openDatabase();
    applyMigrations(database);
    const tableSql = (table: string): string => (database.prepare(`
      SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = ?
    `).pluck().get(table) as string).replaceAll(/\s+/gu, "");
    expect(tableSql("provider_failure_substitution_events")).toContain(
      "'adapter-unavailable','adapter-contract-mismatch','provider-unavailable','provider-timeout','provider-rejected','provider-response-invalid','route-rejected','model-unavailable','capability-unavailable','quota-exhausted','substitution-applied','optional-leg-degraded'",
    );
    expect(tableSql("provider_review_evidence")).toContain(
      "'same-family-exempt','distinct-family-proved','same-family-forbidden','family-unproved'",
    );
  });

  it("initialises exactly one current baseline and treats a second application as a no-op", () => {
    const database = openDatabase();

    expect(applyMigrations(database)).toEqual({ applied: [1], currentVersion: 1 });
    expect(applyMigrations(database)).toEqual({ applied: [], currentVersion: 1 });
    expect(database.prepare(`
      SELECT epoch, length(baseline_sha256) AS baseline_length,
             length(catalog_sha256) AS catalog_length
        FROM fabric_schema
    `).get()).toEqual({
      epoch: "agent-fabric-pre-release-v1",
      baseline_length: 64,
      catalog_length: 64,
    });
    for (const table of [
      "lifecycle_checkpoints",
      "teams",
      "projects",
      "project_sessions",
      "project_session_launch_custody",
      "operator_effect_custody",
      "launched_chair_bridge_retirements",
      "operator_commands",
      "scoped_gates",
      "resource_reservations",
      "task_requests",
      "attention_items",
      "daemon_runtime_epochs",
      "workstreams",
      "chair_live_handoff_custody",
    ]) {
      expect(database.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
      ).get(table)).toEqual({ name: table });
    }
  });

  it("contains only the current operator-launched session and scoped-gate model", () => {
    const database = openDatabase();
    applyMigrations(database);

    expect(database.prepare(
      "SELECT name FROM sqlite_schema WHERE type = 'table' AND name = 'task_human_gates'",
    ).get()).toBeUndefined();

    const columns = (table: string): string[] => database.prepare(
      `SELECT name FROM pragma_table_info(?) ORDER BY cid`,
    ).all(table).map((row) => (row as { name: string }).name);
    expect(columns("project_sessions")).not.toContain("migration_manifest_ref");
    expect(columns("scoped_gates")).not.toContain("legacy_status");
    expect(columns("scoped_gates")).not.toContain("legacy_evidence");

    const projectSessionSql = database.prepare(
      "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'project_sessions'",
    ).pluck().get() as string;
    expect(projectSessionSql).toContain("origin_kind='operator-launch'");
    expect(projectSessionSql).not.toContain("legacy-migration");

    const artifactSql = database.prepare(
      "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'artifacts'",
    ).pluck().get() as string;
    expect(artifactSql).not.toContain("'migration'");
  });

  it("installs the closed typed-Git variants and single-owner lease constraints", () => {
    const database = openDatabase();
    applyMigrations(database);

    expect(() => database.prepare(`
      INSERT INTO run_git_allowlist_variants(
        project_session_id,coordination_run_id,authority_revision,git_allowlist_epoch,operation_variant
      ) VALUES ('session_x','run_x',1,1,'branch-delete-force')
    `).run()).toThrow(/operation_variant|CHECK constraint/iu);

    database.exec(`
      INSERT INTO projects(project_id,canonical_root,revision,authority_generation,created_at,updated_at)
      VALUES ('project_01','/project/one',1,1,1,1);
      INSERT INTO project_sessions(
        project_session_id,project_id,mode,state,revision,generation,authority_ref,budget_ref,
        launch_packet_path,launch_packet_digest,membership_revision,origin_kind,origin_operator_id,
        created_at,updated_at
      ) VALUES (
        'session_01','project_01','coordinated','active',1,1,'authority-session','budget-session',
        'launch.json','sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
        1,'operator-launch','operator_01',1,1
      );
      INSERT INTO runs(
        run_id,chair_agent_id,workspace_root,project_run_directory,project_run_directory_basis,
        created_at,project_session_id,lifecycle_state,revision,chair_generation,chair_lease_id,
        authority_ref,budget_ref,dependency_revision,topology_slot
      ) VALUES (
        'run_01','chair_01','/project/one','.agent-run/current','project-relative',1,
        'session_01','active',1,1,'chair:run_01:1','authority-run','budget-run',1,1
      );
      INSERT INTO authorities(authority_id,run_id,authority_json,authority_hash,created_at)
      VALUES ('authority_01','run_01','{}','authority-hash',1);
      INSERT INTO agents(run_id,agent_id,authority_id,provider_session_ref,lifecycle)
      VALUES ('run_01','chair_01','authority_01','provider-chair','ready');
      INSERT INTO leases(lease_id,run_id,kind,holder_agent_id,generation,status,expires_at,updated_at)
      VALUES ('lease_shared','run_01','write','chair_01',1,'active',9999999999999,1);
    `);
    expect(() => database.prepare(`
      INSERT INTO run_chair_leases(
        project_session_id,run_id,lease_id,holder_agent_id,generation,status,updated_at
      ) VALUES ('session_01','run_01','lease_shared','chair_01',1,'active',1)
    `).run()).toThrow(/INVARIANT_lease_identity_single_owner/u);
  });
});
