import Database from "better-sqlite3";

import { applyMigrations } from "../../../src/core/migrations.ts";
import type { AuthenticatedAgentContext } from "../../../src/project-session/contracts.ts";
import { canonicalJson, sha256 } from "../../../src/persistence/row-codec.ts";
import { TEST_AUTHORITY_V2_FIELDS } from "../../support/authority-v2-testkit.ts";

export function openSystemDatabase(filename = ":memory:"): Database.Database {
  const database = new Database(filename);
  database.pragma("foreign_keys = ON");
  applyMigrations(database);
  seedSystemRun(database);
  return database;
}

export function reopenSystemDatabase(filename: string): Database.Database {
  const database = new Database(filename);
  database.pragma("foreign_keys = ON");
  return database;
}

export function seedSystemRun(database: Database.Database): void {
  database.prepare(`
    INSERT INTO projects(project_id, canonical_root, revision, authority_generation, created_at, updated_at)
    VALUES ('project_01', '/project/one', 1, 1, 1, 1)
  `).run();
  database.prepare(`
    INSERT INTO project_sessions(
      project_session_id, project_id, mode, state, revision, generation,
      authority_ref, budget_ref, launch_packet_path, launch_packet_digest,
      membership_revision, origin_kind, origin_operator_id, created_at, updated_at
    ) VALUES (
      'session_01', 'project_01', 'coordinated', 'active', 1, 1,
      'authority-session', 'budget-session', 'docs/spec.md', 'sha256:${"a".repeat(64)}',
      1, 'operator-launch', 'operator_01', 1, 1
    )
  `).run();
  database.prepare(`
    INSERT INTO runs(
      run_id, chair_agent_id, workspace_root, project_run_directory,
      project_run_directory_basis, created_at, project_session_id,
      lifecycle_state, revision, chair_generation, chair_lease_id,
      authority_ref, budget_ref, dependency_revision, topology_slot
    ) VALUES (
      'run_01', 'chair_01', '/project/one', '.agent-run/run_01',
      'project-relative', 1, 'session_01',
      'active', 1, 1, 'chair:run_01:1', 'authority-run', 'budget-run', 1, 1
    )
  `).run();
  const authorityJson = canonicalJson({
    ...TEST_AUTHORITY_V2_FIELDS,
    workspaceRoots: ["."],
    sourcePaths: ["."],
    artifactPaths: [".agent-run/run_01"],
    actions: [],
    disclosure: { level: "scoped", scopes: ["local"] },
    expiresAt: "2099-01-01T00:00:00.000Z",
    budget: {},
  });
  database.prepare(`
    INSERT INTO authorities(authority_id, run_id, authority_json, authority_hash, created_at)
    VALUES ('authority_01', 'run_01', ?, ?, 1)
  `).run(authorityJson, sha256(authorityJson));
  for (const [agentId, providerRef] of [
    ["chair_01", "provider-chair"],
    ["worker_01", "provider-worker"],
    ["worker_02", "provider-worker-2"],
  ] as const) {
    database.prepare(`
      INSERT INTO agents(run_id, agent_id, authority_id, provider_session_ref, lifecycle)
      VALUES ('run_01', ?, 'authority_01', ?, 'ready')
    `).run(agentId, providerRef);
  }
  database.prepare(`
    INSERT INTO run_chair_leases(
      project_session_id, run_id, lease_id, holder_agent_id, generation, status, updated_at
    ) VALUES ('session_01', 'run_01', 'chair:run_01:1', 'chair_01', 1, 'active', 1)
  `).run();
  for (const [taskId, owner] of [
    ["task_root", "worker_01"],
    ["task_child", "worker_01"],
    ["task_grandchild", "worker_02"],
    ["task_sibling", "worker_02"],
  ] as const) {
    database.prepare(`
      INSERT INTO tasks(
        run_id, task_id, authority_id, objective, base_revision, state,
        owner_agent_id, revision, owner_lease_generation, created_by
      ) VALUES ('run_01', ?, 'authority_01', ?, 'base', 'ready', ?, 1, 1, 'chair_01')
    `).run(taskId, taskId, owner);
    database.prepare(`
      INSERT INTO task_owner_leases(
        project_session_id, run_id, task_id, lease_id, holder_agent_id,
        generation, status, updated_at
      ) VALUES ('session_01', 'run_01', ?, ?, ?, 1, 'active', 1)
    `).run(taskId, `owner:${taskId}:1`, owner);
  }
}

export const chairContext = {
  agentId: "chair_01",
  projectSessionId: "session_01",
  coordinationRunId: "run_01",
  principalGeneration: 1,
} as unknown as AuthenticatedAgentContext;

export const workerContext = {
  agentId: "worker_01",
  projectSessionId: "session_01",
  coordinationRunId: "run_01",
  principalGeneration: 1,
} as unknown as AuthenticatedAgentContext;

export const workerTwoContext = {
  agentId: "worker_02",
  projectSessionId: "session_01",
  coordinationRunId: "run_01",
  principalGeneration: 1,
} as unknown as AuthenticatedAgentContext;
