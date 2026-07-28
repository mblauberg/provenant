import Database from "better-sqlite3";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";

import {
  deriveFinalAcceptanceRef,
  parseOperatorCapabilityGrant,
  parseSha256Digest,
  type OperatorId,
  type ProjectId,
  type ProjectSessionCloseRequest,
  type ProjectSessionTransitionRequest,
  type Sha256Digest,
} from "@local/agent-fabric-protocol";
import { afterEach, describe, expect, it } from "vitest";

import { applyMigrations } from "../../../src/core/migrations.ts";
import { ProviderActionAdmissionCoordinator } from "../../../src/application/provider-action-admission.ts";
import { createProductionOperatorActionPorts } from "../../../src/operator/production-action-ports.ts";
import { OperatorStore } from "../../../src/operator/store.ts";
import { ProjectSessionStore } from "../../../src/project-session/store.ts";
import { ScopedGateStore } from "../../../src/gates/store.ts";
import { canonicalJson, sha256 } from "../../../src/persistence/row-codec.ts";
import { NotificationOutbox } from "../../../src/attention/outbox.ts";
import { admitProviderActionFixture } from "../../support/provider-action-fixture.ts";

const cleanup: Array<() => Promise<void>> = [];
const now = Date.parse("2027-01-01T00:00:00Z");
const digest = `sha256:${"a".repeat(64)}`;

afterEach(async () => {
  await Promise.allSettled(cleanup.splice(0).map(async (close) => close()));
});

function stateDigest(value: unknown): Sha256Digest {
  return parseSha256Digest(`sha256:${sha256(canonicalJson(value))}`, "test.stateDigest");
}

function operatorContext() {
  return {
    operatorId: "operator_01" as OperatorId,
    projectId: "project_01" as ProjectId,
    projectAuthorityGeneration: 1,
    principalGeneration: 1,
  };
}

function command(commandId: string, expectedRevision: number) {
  return {
    credential: { capabilityId: "cap_session", token: "session-secret" },
    commandId,
    expectedRevision,
    actor: "operator_01",
    provenance: { kind: "console-direct-input", clientId: "console_01", inputEventId: `${commandId}:input` },
    evidenceRefs: [{ path: "launch.json", digest }],
  } as const;
}

describe("accepted project-session closure", () => {
  it("moves a drained launched run through awaiting acceptance and closes it from source truth", async () => {
    const root = await mkdtemp(join(tmpdir(), "fabric-accepted-close-"));
    const databasePath = join(root, "fabric.sqlite3");
    let database = new Database(databasePath);
    cleanup.push(async () => {
      if (database.open) database.close();
      await rm(root, { recursive: true, force: true });
    });
    applyMigrations(database);
    database.exec(`
      INSERT INTO projects(project_id, canonical_root, trust_record_digest, revision, authority_generation, created_at, updated_at)
      VALUES ('project_01', '${root}', '${digest}', 1, 1, ${now}, ${now});
      INSERT INTO project_sessions(
        project_session_id, project_id, mode, state, revision, generation, authority_ref,
        budget_ref, launch_packet_path, launch_packet_digest, membership_revision,
        origin_kind, origin_operator_id, created_at, updated_at
      ) VALUES (
        'session_01', 'project_01', 'independent', 'active', 2, 1, '${digest}',
        'budget_01', 'launch.json', '${digest}', 1, 'operator-launch', 'operator_01', ${now}, ${now}
      );
      INSERT INTO runs(
        run_id, chair_agent_id, workspace_root, project_run_directory, created_at,
        project_session_id, lifecycle_state, revision, chair_generation, chair_lease_id,
        authority_ref, budget_ref, dependency_revision, topology_slot
      ) VALUES (
        'run_01', 'chair_01', '${root}', '.agent-run/AFAB-001', ${now},
        'session_01', 'active', 4, 1, 'chair:run_01:1', '${digest}', 'budget_01', 1, NULL
      );
      INSERT INTO runs(
        run_id, chair_agent_id, workspace_root, project_run_directory, created_at,
        project_session_id, lifecycle_state, revision, chair_generation, chair_lease_id,
        authority_ref, budget_ref, dependency_revision, topology_slot
      ) VALUES (
        'run_history', 'chair_history', '${root}', '.agent-run/AFAB-HISTORY', ${now},
        'session_01', 'cancelled', 2, 1, 'chair:run_history:1', '${digest}', 'budget_01', 1, NULL
      );
      INSERT INTO authorities(authority_id, run_id, authority_json, authority_hash, created_at)
      VALUES ('authority_01', 'run_01', '{}', '${"b".repeat(64)}', ${now});
      INSERT INTO authorities(authority_id, run_id, authority_json, authority_hash, created_at)
      VALUES ('authority_history', 'run_history', '{}', '${"c".repeat(64)}', ${now});
      INSERT INTO agents(run_id, agent_id, authority_id, provider_session_ref, lifecycle)
      VALUES ('run_01', 'chair_01', 'authority_01', 'provider_session_01', 'ready');
      INSERT INTO agents(run_id, agent_id, authority_id, provider_session_ref, lifecycle)
      VALUES
        ('run_01', 'agent_completion', 'authority_01', 'provider_session_02', 'completion-ready'),
        ('run_01', 'agent_paused', 'authority_01', 'provider_session_03', 'suspended');
      INSERT INTO agents(run_id, agent_id, authority_id, provider_session_ref, lifecycle)
      VALUES ('run_history', 'chair_history', 'authority_history', 'provider_session_history', 'archived');
      INSERT INTO tasks(
        run_id, task_id, authority_id, objective, base_revision, state,
        owner_agent_id, revision, owner_lease_generation, created_by
      ) VALUES (
        'run_01', 'task_01', 'authority_01', 'Completed delivery', 'base_01', 'complete',
        'chair_01', 4, 1, 'chair_01'
      );
      INSERT INTO run_chair_leases(
        project_session_id, run_id, lease_id, holder_agent_id, generation, status, updated_at
      ) VALUES ('session_01', 'run_01', 'chair:run_01:1', 'chair_01', 1, 'active', ${now});
      INSERT INTO run_chair_leases(
        project_session_id, run_id, lease_id, holder_agent_id, generation, status, updated_at
      ) VALUES ('session_01', 'run_history', 'chair:run_history:1', 'chair_history', 1, 'revoked', ${now});
      INSERT INTO capabilities(token_hash,run_id,agent_id,principal_generation,expires_at,revoked_at)
      VALUES
        ('cap_chair_bridge','run_01','chair_01',1,${now + 100_000},NULL),
        ('cap_child_bridge','run_01','agent_completion',1,${now + 100_000},NULL);
    `);
    admitProviderActionFixture(database, {
      runId: "run_01",
      adapterId: "adapter_test",
      actionId: "launch_chair",
      operation: "spawn",
      targetAgentId: "chair_01",
      providerSessionGeneration: 1,
      identityHash: "identity-chair",
      payloadHash: "payload-chair",
      payloadJson: "{}",
      status: "terminal",
      historyJson: "[]",
      executionCount: 1,
      effectCount: 1,
      idempotencyProven: true,
      resultJson: '{"outcome":{"kind":"terminal-success","providerSessionRef":"provider_session_01"}}',
      updatedAt: now,
    });
    admitProviderActionFixture(database, {
      runId: "run_01",
      adapterId: "adapter_test",
      actionId: "spawn_child",
      operation: "spawn",
      targetAgentId: "agent_completion",
      providerSessionGeneration: 1,
      identityHash: "identity-child",
      payloadHash: "payload-child",
      payloadJson: "{}",
      status: "terminal",
      historyJson: "[]",
      executionCount: 1,
      effectCount: 1,
      idempotencyProven: true,
      resultJson: '{"outcome":{"kind":"terminal-success","providerSessionRef":"provider_session_02"}}',
      updatedAt: now,
    });
    database.exec(`
      INSERT INTO provider_agent_custody(
        run_id,action_id,operation,actor_agent_id,target_agent_id,authority_id,adapter_id,
        bridge_contract_digest,bridge_capable,capability_hash,capability_expires_at,
        principal_generation,requested_provider_session_ref,intent_digest,created_at
      ) VALUES (
        'run_01','spawn_child','spawn','chair_01','agent_completion','authority_01','adapter_test',
        '${digest}',1,'cap_child_bridge',${now + 100_000},1,NULL,'${digest}',${now}
      );
      INSERT INTO launched_chair_bridge_state(
        project_session_id,coordination_run_id,chair_agent_id,provider_adapter_id,
        provider_action_id,provider_contract_digest,provider_session_ref,
        provider_session_generation,principal_generation,bridge_generation,
        capability_hash,activation_evidence_digest,state,revision,created_at,updated_at
      ) VALUES (
        'session_01','run_01','chair_01','adapter_test','launch_chair','${digest}',
        'provider_session_01',1,1,1,'cap_chair_bridge','${digest}','active',1,${now},${now}
      );
      INSERT INTO agent_bridge_state(
        run_id,agent_id,adapter_id,action_id,provider_session_ref,
        provider_session_generation,bridge_state,bridge_generation,capability_hash,
        activation_evidence_digest,revision,created_at,updated_at
      ) VALUES (
        'run_01','agent_completion','adapter_test','spawn_child','provider_session_02',
        1,'active',1,'cap_child_bridge','${digest}',1,${now},${now}
      );
      INSERT INTO project_session_memberships(
        project_session_id, coordination_run_id, member_kind, member_id,
        required, state, revision, abandoned_reason, created_at, updated_at
      ) VALUES
        ('session_01', 'run_01', 'coordination-run', 'run_01', 1, 'active', 1, NULL, ${now}, ${now}),
        ('session_01', 'run_01', 'lease', 'chair:run_01:1', 1, 'active', 1, NULL, ${now}, ${now}),
        ('session_01', 'run_01', 'gate', 'gate_final_acceptance', 1, 'active', 1, NULL, ${now}, ${now}),
        ('session_01', 'run_history', 'coordination-run', 'run_history', 1, 'abandoned', 1, 'historical cancellation', ${now}, ${now}),
        ('session_01', 'run_history', 'lease', 'chair:run_history:1', 1, 'abandoned', 1, 'historical cancellation', ${now}, ${now});
      INSERT INTO scoped_gates(
        gate_id, project_session_id, coordination_run_id, dedupe_key, scope_kind,
        scope_task_id, dependency_revision, blocked_operation_ids_json,
        enforcement_points_json, question, reason, options_json, recommendation,
        consequences_json, evidence_refs_json, created_by_ref, expected_approver_ref,
        status, human_required, revision, created_at, updated_at
      ) VALUES (
        'gate_final_acceptance', 'session_01', 'run_01', 'final-acceptance', 'run',
        NULL, 1, '["fabric.v1.project-session.close"]', '["operation"]',
        'Accept this run?', 'Final acceptance', '["approve","reject"]', 'approve',
        '["Closes the accepted session"]',
        '[{"path":"launch.json","digest":"${digest}"}]',
        'operator:operator_01', 'authenticated-human-operator',
        'pending', 1, 1, ${now}, ${now}
      );
      INSERT INTO scoped_gate_operations(gate_id, operation_id)
      VALUES ('gate_final_acceptance', 'fabric.v1.project-session.close');
    `);
    const acceptanceNotifications = new NotificationOutbox({ database, clock: () => now });
    const acceptanceAttention = acceptanceNotifications.upsertAttention({
      producerId: "operator:operator_01",
      projectId: "project_01",
      projectSessionId: "session_01",
      coordinationRunId: "run_01",
      principalGeneration: 1,
    }, {
      dedupeKey: "scoped-gate:gate_final_acceptance",
      kind: "consequential-gate",
      severity: "critical",
      payload: {
        gateId: "gate_final_acceptance",
        title: "Accept this run?",
        summary: "Final acceptance",
        priority: "critical-path",
        duplicateCount: 1,
      },
    });
    acceptanceNotifications.enqueue({
      producerId: "operator:operator_01",
      projectId: "project_01",
      projectSessionId: "session_01",
      coordinationRunId: "run_01",
      principalGeneration: 1,
    }, {
      itemId: acceptanceAttention.itemId,
      expectedItemRevision: acceptanceAttention.revision,
      targetIntegration: "native-desktop",
    });
    const operatorStore = new OperatorStore({ database, clock: () => now });
    operatorStore.registerPrincipal({
      operatorId: "operator_01",
      projectId: "project_01",
      authenticatedSubjectHash: "subject-hash",
      projectAuthorityGeneration: 1,
    });
    operatorStore.issueCapability(parseOperatorCapabilityGrant({
      capabilityId: "cap_session",
      operatorId: "operator_01",
      projectId: "project_01",
      projectAuthorityGeneration: 1,
      principalGeneration: 1,
      issuedAt: "2026-01-01T00:00:00Z",
      expiresAt: "2099-01-01T00:00:00Z",
      status: "active",
      kind: "session",
      projectSessionId: "session_01",
      sessionGeneration: 1,
      actions: ["read", "decide"],
    }), "session-secret");
    const gates = new ScopedGateStore({ database, operatorStore, clock: () => now + 1 });
    expect(() => gates.resolveGate(operatorContext(), {
      command: command("reject_active_final_acceptance", 1),
      gateId: "gate_final_acceptance",
      status: "approved",
      decisionEvidence: { kind: "typed-console", confirmationCommandId: "reject_active_final_acceptance" },
    } as never)).toThrowError(expect.objectContaining({ code: "LIFECYCLE_PRECONDITION_FAILED" }));

    const ports = createProductionOperatorActionPorts({
      database,
      clock: () => now,
      providerActionAdmission: new ProviderActionAdmissionCoordinator({ database, clock: () => now }),
      adapter: {
        capabilities: async () => { throw new Error("drain must not inspect a provider adapter"); },
        dispatch: async () => { throw new Error("drain must not dispatch a provider adapter"); },
        lookup: async () => { throw new Error("drain must not look up a provider adapter"); },
      },
    });
    const global = database.prepare("SELECT revision FROM daemon_global_state WHERE singleton=1")
      .get() as { revision: number };
    const intent = {
      kind: "project-session-drain" as const,
      projectSessionId: "session_01" as never,
      expectedSessionRevision: 2,
      expectedSessionGeneration: 1,
      expectedGlobalStateRevision: global.revision,
    };
    const before = await ports.statePort.read(intent);
    const drainRequest = {
      commandId: "drain_for_acceptance",
      operatorId: "operator_01",
      projectId: "project_01",
      projectSessionId: "session_01",
      principalGeneration: 1,
      operation: "project-session-drain",
      intent,
      intentDigest: stateDigest(intent),
      beforeStateDigest: stateDigest(before),
      attemptGeneration: 1,
    } as const;
    ports.effectPort.prepare?.(drainRequest);
    const draining = await ports.effectPort.dispatch(drainRequest);
    expect(draining).toEqual({ status: "pending", phase: "accepted" });

    database.prepare(`
      INSERT INTO operator_effect_custody(
        custody_id,operator_id,project_id,project_session_id,principal_generation,
        command_id,operation,intent_digest,before_state_digest,intent_json,state,
        created_at,updated_at
      ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?)
    `).run(
      "git-quarantine-acceptance",
      "operator_01",
      "project_01",
      "session_01",
      1,
      "git-quarantine-command",
      "git-mutation",
      digest,
      digest,
      JSON.stringify({ kind: "git-mutation" }),
      "quarantined",
      now,
      now,
    );
    expect(() => gates.resolveGate(operatorContext(), {
      command: command("reject_quarantined_git", 1),
      gateId: "gate_final_acceptance",
      status: "approved",
      decisionEvidence: { kind: "typed-console", confirmationCommandId: "reject_quarantined_git" },
    } as never)).toThrowError(expect.objectContaining({ code: "BARRIER_PRECONDITION_FAILED" }));
    await expect(ports.effectPort.observe({ ...drainRequest, effectRef: null }))
      .resolves.toEqual({ status: "pending", phase: "observing" });
    database.prepare(`
      UPDATE operator_effect_custody SET state='no-effect',updated_at=?
       WHERE custody_id='git-quarantine-acceptance' AND state='quarantined'
    `).run(now + 1);

    const acceptedGate = gates.resolveGate(operatorContext(), {
      command: command("confirm_final_acceptance", 1),
      gateId: "gate_final_acceptance",
      status: "approved",
      decisionEvidence: { kind: "typed-console", confirmationCommandId: "confirm_final_acceptance" },
    } as never);
    if (acceptedGate.status !== "approved") throw new Error("expected approved final-acceptance gate");
    const acceptanceRef = deriveFinalAcceptanceRef({
      projectSessionId: "session_01" as never,
      gates: [{
        gateId: acceptedGate.gateId,
        coordinationRunId: acceptedGate.coordinationRunId,
        gateRevision: acceptedGate.revision,
        status: "approved",
        resolution: acceptedGate.resolution,
        evidenceRefs: acceptedGate.evidenceRefs,
      }],
    });
    expect(database.prepare(`
      SELECT
        (SELECT COUNT(*) FROM scoped_gates WHERE project_session_id='session_01' AND status IN ('pending','deferred')) AS gates,
        (SELECT COUNT(*) FROM project_session_memberships WHERE project_session_id='session_01' AND state='active'
          AND NOT (member_kind='coordination-run' OR (member_kind='lease' AND member_id='chair:run_01:1'))) AS memberships
    `).get()).toEqual({ gates: 0, memberships: 0 });
    expect(database.prepare(`
      SELECT
        (SELECT COUNT(*) FROM tasks t JOIN runs r ON r.run_id=t.run_id WHERE r.project_session_id='session_01' AND t.state NOT IN ('complete','cancelled','degraded')) AS tasks,
        (SELECT COUNT(*) FROM leases l JOIN runs r ON r.run_id=l.run_id WHERE r.project_session_id='session_01' AND l.status IN ('active','quarantined')) AS leases,
        (SELECT COUNT(*) FROM task_owner_leases WHERE project_session_id='session_01' AND status IN ('active','frozen')) AS owners,
        (SELECT COUNT(*) FROM provider_actions a JOIN runs r ON r.run_id=a.run_id WHERE r.project_session_id='session_01' AND a.status IN ('prepared','dispatched','accepted','ambiguous','quarantined')) AS actions,
        (SELECT COUNT(*) FROM result_deliveries WHERE project_session_id='session_01' AND required=1 AND state NOT IN ('consumed','abandoned')) AS deliveries,
        (SELECT COUNT(*) FROM barriers b JOIN runs r ON r.run_id=b.run_id WHERE r.project_session_id='session_01' AND b.state<>'closed') AS barriers
    `).get()).toEqual({ tasks: 0, leases: 0, owners: 0, actions: 0, deliveries: 0, barriers: 0 });
    const drained = await ports.effectPort.observe({ ...drainRequest, effectRef: null });
    expect(drained).toMatchObject({
      status: "committed",
      afterState: { lifecycleState: "quiescing", obligationsSettled: true },
    });
    if (drained.status !== "committed" || drained.effectRef === undefined) throw new Error("expected drain receipt");
    const drainReceipt = drained.effectRef;

    let sessions = new ProjectSessionStore({ database, operatorStore, clock: () => now + 1 });
    expect(() => sessions.transitionProjectSession(operatorContext(), {
      command: command("await_wrong_receipt", 4),
      projectSessionId: "session_01",
      expectedGeneration: 1,
      transition: {
        to: "awaiting_acceptance",
        closureEvidence: { path: drainReceipt.path, digest: `sha256:${"f".repeat(64)}` },
      },
    } as unknown as ProjectSessionTransitionRequest)).toThrowError(expect.objectContaining({ code: "NOT_FOUND" }));
    database.prepare("UPDATE tasks SET state='active', revision=revision+1 WHERE run_id='run_01' AND task_id='task_01'").run();
    expect(() => sessions.transitionProjectSession(operatorContext(), {
      command: command("await_with_new_work", 4),
      projectSessionId: "session_01",
      expectedGeneration: 1,
      transition: { to: "awaiting_acceptance", closureEvidence: drainReceipt },
    } as unknown as ProjectSessionTransitionRequest)).toThrowError(
      expect.objectContaining({ code: "BARRIER_PRECONDITION_FAILED" }),
    );
    expect(database.prepare("SELECT state, revision FROM project_sessions WHERE project_session_id='session_01'").get())
      .toEqual({ state: "quiescing", revision: 4 });
    database.prepare("UPDATE tasks SET state='complete', revision=revision+1 WHERE run_id='run_01' AND task_id='task_01'").run();
    database.prepare("UPDATE agents SET lifecycle='context-unreconciled' WHERE run_id='run_01' AND agent_id='agent_paused'").run();
    expect(() => sessions.transitionProjectSession(operatorContext(), {
      command: command("await_with_unreconciled_context", 4),
      projectSessionId: "session_01",
      expectedGeneration: 1,
      transition: { to: "awaiting_acceptance", closureEvidence: drainReceipt },
    } as unknown as ProjectSessionTransitionRequest)).toThrowError(
      expect.objectContaining({ code: "BARRIER_PRECONDITION_FAILED" }),
    );
    database.prepare("UPDATE agents SET lifecycle='suspended' WHERE run_id='run_01' AND agent_id='agent_paused'").run();
    const crashingSessions = new ProjectSessionStore({
      database,
      operatorStore,
      clock: () => now + 1,
      fault: (label) => {
        if (label === "session:acceptance:after-memberships") throw new Error("acceptance crash");
      },
    });
    expect(() => crashingSessions.transitionProjectSession(operatorContext(), {
      command: command("await_acceptance_crash", 4),
      projectSessionId: "session_01",
      expectedGeneration: 1,
      transition: { to: "awaiting_acceptance", closureEvidence: drainReceipt },
    } as unknown as ProjectSessionTransitionRequest)).toThrow("acceptance crash");
    expect(database.prepare("SELECT state, revision, membership_revision FROM project_sessions WHERE project_session_id='session_01'").get())
      .toEqual({ state: "quiescing", revision: 4, membership_revision: 2 });
    expect(database.prepare("SELECT lifecycle_state, revision FROM runs WHERE run_id='run_01'").get())
      .toEqual({ lifecycle_state: "quiescing", revision: 5 });
    expect(database.prepare("SELECT status FROM run_chair_leases WHERE lease_id='chair:run_01:1'").get())
      .toEqual({ status: "active" });
    const awaiting = sessions.transitionProjectSession(operatorContext(), {
      command: command("await_acceptance", 4),
      projectSessionId: "session_01",
      expectedGeneration: 1,
      transition: { to: "awaiting_acceptance", closureEvidence: drainReceipt },
    } as unknown as ProjectSessionTransitionRequest);
    expect(awaiting).toMatchObject({ state: "awaiting_acceptance", revision: 5, membershipRevision: 3 });
    expect(database.prepare("SELECT lifecycle_state, revision FROM runs WHERE run_id='run_01'").get())
      .toEqual({ lifecycle_state: "awaiting_acceptance", revision: 6 });
    expect(database.prepare("SELECT status FROM run_chair_leases WHERE lease_id='chair:run_01:1'").get())
      .toEqual({ status: "frozen" });
    expect(database.prepare("SELECT agent_id, lifecycle FROM agents WHERE run_id='run_01' ORDER BY agent_id").all())
      .toEqual([
        { agent_id: "agent_completion", lifecycle: "completion-ready" },
        { agent_id: "agent_paused", lifecycle: "suspended" },
        { agent_id: "chair_01", lifecycle: "ready" },
      ]);
    expect(database.prepare(`
      SELECT member_kind, state FROM project_session_memberships
       WHERE coordination_run_id='run_01' ORDER BY member_kind
    `).all()).toEqual([
      { member_kind: "coordination-run", state: "reconciled" },
      { member_kind: "gate", state: "reconciled" },
      { member_kind: "lease", state: "reconciled" },
    ]);

    const crashingDiversion = new ProjectSessionStore({
      database,
      operatorStore,
      clock: () => now + 1,
      fault: (label) => {
        if (label === "session:quiesce-exit:after-runs") throw new Error("diversion crash");
      },
    });
    expect(() => crashingDiversion.transitionProjectSession(operatorContext(), {
      command: command("divert_acceptance_crash", 5),
      projectSessionId: "session_01",
      expectedGeneration: 1,
      transition: { to: "reconciling", reason: "Reconcile changed delivery state" },
    } as unknown as ProjectSessionTransitionRequest)).toThrow("diversion crash");
    expect(database.prepare(`
      SELECT state, revision, membership_revision FROM project_sessions
       WHERE project_session_id='session_01'
    `).get()).toEqual({ state: "awaiting_acceptance", revision: 5, membership_revision: 3 });
    expect(database.prepare("SELECT lifecycle_state, revision FROM runs WHERE run_id='run_01'").get())
      .toEqual({ lifecycle_state: "awaiting_acceptance", revision: 6 });
    expect(database.prepare("SELECT status, revision FROM scoped_gates WHERE gate_id='gate_final_acceptance'").get())
      .toEqual({ status: "approved", revision: 2 });
    expect(database.prepare("SELECT status FROM run_chair_leases WHERE lease_id='chair:run_01:1'").get())
      .toEqual({ status: "frozen" });
    expect(database.prepare(`
      SELECT member_kind, state FROM project_session_memberships
       WHERE coordination_run_id='run_01' ORDER BY member_kind
    `).all()).toEqual([
      { member_kind: "coordination-run", state: "reconciled" },
      { member_kind: "gate", state: "reconciled" },
      { member_kind: "lease", state: "reconciled" },
    ]);

    database.close();
    database = new Database(databasePath);
    const restartedOperatorStore = new OperatorStore({ database, clock: () => now + 2 });
    sessions = new ProjectSessionStore({
      database,
      operatorStore: restartedOperatorStore,
      clock: () => now + 2,
    });
    const reopened = sessions.transitionProjectSession(operatorContext(), {
      command: command("reopen_after_acceptance", 5),
      projectSessionId: "session_01",
      expectedGeneration: 1,
      transition: { to: "active", reason: "More work requested" },
    } as unknown as ProjectSessionTransitionRequest);
    expect(reopened).toMatchObject({ state: "active", revision: 6, membershipRevision: 4 });
    expect(database.prepare("SELECT status, revision FROM scoped_gates WHERE gate_id='gate_final_acceptance'").get())
      .toEqual({ status: "superseded", revision: 3 });
    expect(database.prepare("SELECT agent_id, lifecycle FROM agents WHERE run_id='run_01' ORDER BY agent_id").all())
      .toEqual([
        { agent_id: "agent_completion", lifecycle: "completion-ready" },
        { agent_id: "agent_paused", lifecycle: "suspended" },
        { agent_id: "chair_01", lifecycle: "ready" },
      ]);

    const changedEvidenceDigest = `sha256:${"d".repeat(64)}`;
    database.prepare(`
      INSERT INTO artifacts(
        artifact_id, project_id, project_session_id, run_id, task_id,
        publisher_kind, publisher_ref, publisher_agent_id, source_kind, evidence_kind,
        relative_path, sha256, registry_state, revision, created_at
      ) VALUES (
        'artifact_after_reopen', 'project_01', 'session_01', 'run_01', 'task_01',
        'agent', 'chair_01', 'chair_01', 'project-file', 'artifact',
        'evidence/after-reopen.md', ?, 'active', 1, ?
      )
    `).run(changedEvidenceDigest, now + 2);
    const restartedGates = new ScopedGateStore({
      database,
      operatorStore: restartedOperatorStore,
      clock: () => now + 2,
    });
    const freshGate = restartedGates.createGate(operatorContext(), {
      origin: "operator",
      command: command("create_fresh_final_acceptance", 1),
      intent: {
        projectSessionId: "session_01",
        coordinationRunId: "run_01",
        dedupeKey: "final-acceptance-after-reopen",
        scope: { kind: "run" },
        blockedOperationIds: ["fabric.v1.project-session.close"],
        enforcementPoints: ["operation"],
        question: "Accept the work changed after reopen?",
        reason: "Fresh final acceptance cycle",
        options: ["approve", "reject"],
        recommendation: "approve",
        consequences: ["Closes the newly accepted session"],
        evidenceRefs: [{ path: "evidence/after-reopen.md", digest: changedEvidenceDigest }],
      },
    } as never);

    const restartedPorts = createProductionOperatorActionPorts({
      database,
      clock: () => now + 2,
      providerActionAdmission: new ProviderActionAdmissionCoordinator({ database, clock: () => now + 2 }),
      adapter: {
        capabilities: async () => { throw new Error("drain must not inspect a provider adapter"); },
        dispatch: async () => { throw new Error("drain must not dispatch a provider adapter"); },
        lookup: async () => { throw new Error("drain must not look up a provider adapter"); },
      },
    });
    const secondGlobal = database.prepare("SELECT revision FROM daemon_global_state WHERE singleton=1")
      .get() as { revision: number };
    const secondIntent = {
      kind: "project-session-drain" as const,
      projectSessionId: "session_01" as never,
      expectedSessionRevision: 7,
      expectedSessionGeneration: 1,
      expectedGlobalStateRevision: secondGlobal.revision,
    };
    const secondBefore = await restartedPorts.statePort.read(secondIntent);
    const secondDrainRequest = {
      ...drainRequest,
      commandId: "drain_after_reopen",
      intent: secondIntent,
      intentDigest: stateDigest(secondIntent),
      beforeStateDigest: stateDigest(secondBefore),
    };
    restartedPorts.effectPort.prepare?.(secondDrainRequest);
    expect(await restartedPorts.effectPort.dispatch(secondDrainRequest))
      .toEqual({ status: "pending", phase: "accepted" });
    const freshAcceptedGate = restartedGates.resolveGate(operatorContext(), {
      command: command("confirm_fresh_final_acceptance", freshGate.revision),
      gateId: freshGate.gateId,
      status: "approved",
      decisionEvidence: { kind: "typed-console", confirmationCommandId: "confirm_fresh_final_acceptance" },
    } as never);
    if (freshAcceptedGate.status !== "approved") throw new Error("expected fresh approved gate");
    const freshAcceptanceRef = deriveFinalAcceptanceRef({
      projectSessionId: "session_01" as never,
      gates: [{
        gateId: freshAcceptedGate.gateId,
        coordinationRunId: freshAcceptedGate.coordinationRunId,
        gateRevision: freshAcceptedGate.revision,
        status: "approved",
        resolution: freshAcceptedGate.resolution,
        evidenceRefs: freshAcceptedGate.evidenceRefs,
      }],
    });
    const secondDrained = await restartedPorts.effectPort.observe({ ...secondDrainRequest, effectRef: null });
    if (secondDrained.status !== "committed" || secondDrained.effectRef === undefined) {
      throw new Error("expected second drain receipt");
    }
    const reawaiting = sessions.transitionProjectSession(operatorContext(), {
      command: command("await_after_reopen", 9),
      projectSessionId: "session_01",
      expectedGeneration: 1,
      transition: { to: "awaiting_acceptance", closureEvidence: secondDrained.effectRef },
    } as unknown as ProjectSessionTransitionRequest);
    expect(reawaiting).toMatchObject({ state: "awaiting_acceptance", revision: 10, membershipRevision: 7 });
    expect(() => sessions.closeProjectSession(operatorContext(), {
      command: command("reject_stale_acceptance_cycle", 10),
      projectSessionId: "session_01",
      expectedGeneration: 1,
      terminalPath: { kind: "accepted", acceptanceRef },
    } as unknown as ProjectSessionCloseRequest)).toThrowError(
      expect.objectContaining({ code: "CAPABILITY_FORBIDDEN" }),
    );
    expect(() => sessions.closeProjectSession(operatorContext(), {
      command: command("reject_arbitrary_acceptance", 10),
      projectSessionId: "session_01",
      expectedGeneration: 1,
      terminalPath: { kind: "accepted", acceptanceRef: drainReceipt.digest },
    } as unknown as ProjectSessionCloseRequest)).toThrowError(
      expect.objectContaining({ code: "CAPABILITY_FORBIDDEN" }),
    );
    const crashingClose = new ProjectSessionStore({
      database,
      operatorStore: restartedOperatorStore,
      clock: () => now + 2,
      fault: (label) => {
        if (label === "session:close:after-bridges") throw new Error("bridge retirement crash");
      },
    });
    expect(() => crashingClose.closeProjectSession(operatorContext(), {
      command: command("accept_session_crash", 10),
      projectSessionId: "session_01",
      expectedGeneration: 1,
      terminalPath: { kind: "accepted", acceptanceRef: freshAcceptanceRef },
    } as unknown as ProjectSessionCloseRequest)).toThrow("bridge retirement crash");
    expect(database.prepare(`
      SELECT state,revision FROM project_sessions WHERE project_session_id='session_01'
    `).get()).toEqual({ state: "awaiting_acceptance", revision: 10 });
    expect(database.prepare(`
      SELECT lifecycle_state,revision FROM runs WHERE run_id='run_01'
    `).get()).toEqual({ lifecycle_state: "awaiting_acceptance", revision: 9 });
    expect(database.prepare(`
      SELECT state FROM launched_chair_bridge_state
       WHERE project_session_id='session_01' AND coordination_run_id='run_01'
    `).get()).toEqual({ state: "active" });
    expect(database.prepare(`
      SELECT bridge_state FROM agent_bridge_state WHERE run_id='run_01' AND agent_id='agent_completion'
    `).get()).toEqual({ bridge_state: "active" });
    expect(database.prepare(`
      SELECT 1 FROM launched_chair_bridge_retirements
       WHERE project_session_id='session_01' AND coordination_run_id='run_01'
    `).get()).toBeUndefined();
    const closed = sessions.closeProjectSession(operatorContext(), {
      command: command("accept_session", 10),
      projectSessionId: "session_01",
      expectedGeneration: 1,
      terminalPath: { kind: "accepted", acceptanceRef: freshAcceptanceRef },
    } as unknown as ProjectSessionCloseRequest);
    expect(closed).toMatchObject({ state: "closed", revision: 11, terminalPath: { kind: "accepted" } });
    expect(database.prepare("SELECT lifecycle_state, revision FROM runs WHERE run_id='run_01'").get())
      .toEqual({ lifecycle_state: "closed", revision: 10 });
    expect(database.prepare("SELECT lifecycle_state, revision FROM runs WHERE run_id='run_history'").get())
      .toEqual({ lifecycle_state: "cancelled", revision: 2 });
    expect(database.prepare("SELECT status FROM run_chair_leases WHERE lease_id='chair:run_01:1'").get())
      .toEqual({ status: "revoked" });
    expect(database.prepare("SELECT lifecycle FROM agents WHERE run_id='run_01' AND agent_id='chair_01'").get())
      .toEqual({ lifecycle: "archived" });
    expect(database.prepare(`
      SELECT source_kind,terminal_kind,terminal_ref,owner_ref
        FROM launched_chair_bridge_retirements
       WHERE project_session_id='session_01' AND coordination_run_id='run_01'
    `).get()).toEqual({
      source_kind: "project-session-close",
      terminal_kind: "accepted",
      terminal_ref: canonicalJson({ kind: "accepted", acceptanceRef: freshAcceptanceRef }),
      owner_ref: "accept_session",
    });
    expect(database.prepare(`
      SELECT bridge_state,provider_session_ref,provider_session_generation,
             capability_hash,activation_evidence_digest,revision
        FROM agent_bridge_state WHERE run_id='run_01' AND agent_id='agent_completion'
    `).get()).toEqual({
      bridge_state: "none",
      provider_session_ref: null,
      provider_session_generation: null,
      capability_hash: null,
      activation_evidence_digest: null,
      revision: 2,
    });

    database.close();
    database = new Database(databasePath);
    expect(database.prepare(`
      SELECT source_kind,terminal_kind FROM launched_chair_bridge_retirements
       WHERE project_session_id='session_01' AND coordination_run_id='run_01'
    `).get()).toEqual({ source_kind: "project-session-close", terminal_kind: "accepted" });
    expect(database.prepare(`
      SELECT bridge_state FROM agent_bridge_state WHERE run_id='run_01' AND agent_id='agent_completion'
    `).get()).toEqual({ bridge_state: "none" });
  });
});
