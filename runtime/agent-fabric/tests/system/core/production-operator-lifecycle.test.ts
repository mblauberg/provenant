import Database from "better-sqlite3";
import type { OperatorActionIntent, Sha256Digest } from "@local/agent-fabric-protocol";
import { parseSha256Digest } from "@local/agent-fabric-protocol";
import { afterEach, describe, expect, it } from "vitest";

import { applyMigrations } from "../../../src/core/migrations.ts";
import { ProviderActionAdmissionCoordinator } from "../../../src/application/provider-action-admission.ts";
import {
  createProductionOperatorActionPorts,
} from "../../../src/operator/production-action-ports.ts";
import {
  assertOperatorTaskRunnable,
  assertRunAcceptingWork,
} from "../../../src/operator/task-run-admission.ts";
import { canonicalJson, sha256 } from "../../../src/persistence/row-codec.ts";
import {
  admitProviderActionFixture,
} from "../../support/provider-action-fixture.ts";

const databases: Database.Database[] = [];
const digest = `sha256:${"a".repeat(64)}`;
const now = Date.parse("2027-01-01T00:00:00Z");

function stateDigest(value: unknown): Sha256Digest {
  return parseSha256Digest(`sha256:${sha256(canonicalJson(value))}`, "test.stateDigest");
}

function scopedEffectRequest(
  commandId: string,
  intent: OperatorActionIntent,
  beforeStateDigest: Sha256Digest,
) {
  return {
    commandId,
    operatorId: "operator_01",
    projectId: "project_01",
    projectSessionId: "session_01",
    principalGeneration: 1,
    operation: intent.kind === "control" ? intent.action : intent.kind,
    intent,
    intentDigest: stateDigest(intent),
    beforeStateDigest,
    attemptGeneration: 1,
  } as const;
}

function fixture(
  adapter: Parameters<typeof createProductionOperatorActionPorts>[0]["adapter"] = {
    capabilities: async () => { throw new Error("idle control must not inspect an adapter"); },
    dispatch: async () => { throw new Error("idle control must not dispatch an adapter effect"); },
    lookup: async () => { throw new Error("idle control must not look up an adapter effect"); },
  },
  configureAdmission?: (coordinator: ProviderActionAdmissionCoordinator) => void,
  admissionFault?: (label: string) => void,
): {
  database: Database.Database;
  ports: ReturnType<typeof createProductionOperatorActionPorts>;
} {
  const database = new Database(":memory:");
  databases.push(database);
  applyMigrations(database);
  database.exec(`
    INSERT INTO projects(project_id, canonical_root, trust_record_digest, revision, authority_generation, created_at, updated_at)
    VALUES ('project_01', '/project/one', '${digest}', 1, 1, ${now}, ${now});
    INSERT INTO operator_principals(
      operator_id, project_id, project_session_id, authenticated_subject_hash,
      project_authority_generation, principal_generation, state, created_at, updated_at
    ) VALUES (
      'operator_direct_test', 'project_01', NULL, 'direct-test-subject',
      1, 1, 'active', ${now}, ${now}
    );
    INSERT INTO project_sessions(
      project_session_id, project_id, mode, state, revision, generation, authority_ref,
      budget_ref, launch_packet_path, launch_packet_digest, membership_revision,
      origin_kind, origin_operator_id, created_at, updated_at
    ) VALUES (
      'session_01', 'project_01', 'coordinated', 'active', 2, 1, '${digest}',
      'budget_01', 'launch.json', '${digest}', 1, 'operator-launch', 'operator_01', ${now}, ${now}
    );
    INSERT INTO runs(
      run_id, chair_agent_id, workspace_root, project_run_directory, created_at,
      project_session_id, lifecycle_state, revision, chair_generation, chair_lease_id,
      authority_ref, budget_ref, dependency_revision, topology_slot
    ) VALUES (
      'run_01', 'chair_01', '/project/one', '.agent-run/AFAB-001', ${now},
      'session_01', 'active', 4, 1, 'chair:run_01:1', '${digest}', 'budget_01', 1, 1
    );
    INSERT INTO authorities(authority_id, run_id, authority_json, authority_hash, created_at)
    VALUES ('authority_01', 'run_01', '{}', '${"b".repeat(64)}', ${now});
    INSERT INTO agents(run_id, agent_id, authority_id, provider_session_ref, lifecycle)
    VALUES ('run_01', 'chair_01', 'authority_01', 'provider_session_01', 'ready');
    INSERT INTO provider_state(run_id, agent_id, provider_session_generation, context_revision)
    VALUES ('run_01', 'chair_01', 2, 'context_01');
    INSERT INTO agent_adapter_bindings(run_id, agent_id, adapter_id, bound_at)
    VALUES ('run_01', 'chair_01', 'fake', ${now});
    INSERT INTO tasks(
      run_id, task_id, authority_id, objective, base_revision, state,
      owner_agent_id, revision, owner_lease_generation, created_by
    ) VALUES (
      'run_01', 'task_01', 'authority_01', 'Implement lifecycle', 'base_01', 'active',
      'chair_01', 3, 1, 'chair_01'
    );
  `);
  const providerActionAdmission = new ProviderActionAdmissionCoordinator({
    database,
    clock: () => now,
    ...(admissionFault === undefined ? {} : { fault: admissionFault }),
  });
  configureAdmission?.(providerActionAdmission);
  return {
    database,
    ports: createProductionOperatorActionPorts({
      database,
      clock: () => now,
      providerActionAdmission,
      adapter,
    }),
  };
}

function seedProviderAction(
  database: Database.Database,
  input: Readonly<{
    actionId: string;
    adapterId?: string;
    targetAgentId?: string;
    providerSessionGeneration?: number;
    turnLeaseGeneration?: number;
    payloadJson: string;
    status: "prepared" | "dispatched" | "accepted" | "terminal";
    historyJson: string;
    executionCount?: number;
    effectCount?: number;
    resultJson?: string;
  }>,
): void {
  admitProviderActionFixture(database, {
    runId: "run_01",
    actionId: input.actionId,
    adapterId: input.adapterId ?? "fake",
    operation: "send_turn",
    targetAgentId: input.targetAgentId ?? "chair_01",
    providerSessionGeneration: input.providerSessionGeneration ?? 2,
    turnLeaseGeneration: input.turnLeaseGeneration ?? 1,
    identityHash: "c".repeat(64),
    payloadHash: "d".repeat(64),
    payloadJson: input.payloadJson,
    status: input.status,
    historyJson: input.historyJson,
    executionCount: input.executionCount ?? 1,
    effectCount: input.effectCount ?? 0,
    resultJson: input.resultJson ?? null,
    updatedAt: now,
  });
}

afterEach(() => {
  for (const database of databases.splice(0)) database.close();
});

describe("production operator lifecycle ports", () => {
  it.each(["draft", "awaiting_launch"] as const)(
    "cancels an effect-free %s session without fabricating task cancellation",
    async (state) => {
      const database = new Database(":memory:");
      databases.push(database);
      applyMigrations(database);
      database.exec(`
        INSERT INTO projects(project_id, canonical_root, trust_record_digest, revision, authority_generation, created_at, updated_at)
        VALUES ('project_01', '/project/one', '${digest}', 1, 1, ${now}, ${now});
        INSERT INTO project_sessions(
          project_session_id, project_id, mode, state, revision, generation, authority_ref,
          budget_ref, launch_packet_path, launch_packet_digest, membership_revision,
          origin_kind, origin_operator_id, created_at, updated_at
        ) VALUES (
          'session_01', 'project_01', 'coordinated', '${state}', 2, 1, '${digest}',
          'budget_01', 'launch.json', '${digest}', 1, 'operator-launch', 'operator_01', ${now}, ${now}
        );
        INSERT INTO operator_principals(
          operator_id, project_id, project_session_id, authenticated_subject_hash,
          project_authority_generation, principal_generation, state, created_at, updated_at
        ) VALUES ('operator_01', 'project_01', NULL, '${digest}', 1, 1, 'active', ${now}, ${now});
        INSERT INTO operator_client_attachments(
          attachment_id, operator_id, project_id, project_authority_generation, project_session_id,
          session_generation, daemon_instance_generation, lease_generation, state, expires_at,
          revision, created_at, updated_at
        ) VALUES (
          'attachment_01', 'operator_01', 'project_01', 1, 'session_01',
          1, 1, 1, 'active', ${now + 60_000}, 1, ${now}, ${now}
        );
      `);
      const ports = createProductionOperatorActionPorts({
        database,
        clock: () => now,
        providerActionAdmission: new ProviderActionAdmissionCoordinator({ database, clock: () => now }),
        adapter: {
          capabilities: async () => { throw new Error("effect-free cancel must not inspect an adapter"); },
          dispatch: async () => { throw new Error("effect-free cancel must not dispatch an adapter"); },
          lookup: async () => { throw new Error("effect-free cancel must not look up an adapter"); },
        },
      });
      const intent = {
        kind: "control" as const,
        action: "cancel" as const,
        target: {
          kind: "session" as const,
          projectSessionId: "session_01" as never,
          expectedRevision: 2,
          expectedGeneration: 1,
        },
        reason: "operator cancelled effect-free retry",
      };

      const before = await ports.statePort.read(intent);
      await expect(ports.effectPort.dispatch(scopedEffectRequest(
        `cancel_${state}`,
        intent,
        stateDigest(before),
      ))).resolves.toMatchObject({
        status: "committed",
        afterState: { lifecycleState: "cancelled", cancelledTasks: 0 },
      });
      expect(database.prepare(`
        SELECT state, revision, terminal_path_json FROM project_sessions WHERE project_session_id='session_01'
      `).get()).toEqual({
        state: "cancelled",
        revision: 3,
        terminal_path_json: canonicalJson({ kind: "cancelled", reason: "operator cancelled effect-free retry" }),
      });

      database.prepare(`
        UPDATE operator_effect_custody SET state='no-effect' WHERE command_id=?
      `).run(`cancel_${state}`);
      database.prepare(`
        UPDATE project_sessions SET state=?, revision=4, terminal_path_json=NULL WHERE project_session_id='session_01'
      `).run(state);
      const retryIntent = {
        ...intent,
        target: { ...intent.target, expectedRevision: 4 },
      };
      const retryBefore = await ports.statePort.read(retryIntent);
      await expect(ports.effectPort.dispatch(scopedEffectRequest(
        `cancel_${state}_with_history`,
        retryIntent,
        stateDigest(retryBefore),
      ))).rejects.toMatchObject({ code: "LIFECYCLE_PRECONDITION_FAILED" });
      expect(database.prepare(`
        SELECT state, revision, terminal_path_json FROM project_sessions WHERE project_session_id='session_01'
      `).get()).toEqual({ state, revision: 4, terminal_path_json: null });
    },
  );

  it("rehydrates committed effect-free cancellation after a post-mutation crash", async () => {
    const database = new Database(":memory:");
    databases.push(database);
    applyMigrations(database);
    database.exec(`
      INSERT INTO projects(project_id, canonical_root, trust_record_digest, revision, authority_generation, created_at, updated_at)
      VALUES ('project_01', '/project/one', '${digest}', 1, 1, ${now}, ${now});
      INSERT INTO operator_principals(
        operator_id, project_id, project_session_id, authenticated_subject_hash,
        project_authority_generation, principal_generation, state, created_at, updated_at
      ) VALUES ('operator_01', 'project_01', NULL, '${digest}', 1, 1, 'active', ${now}, ${now});
      INSERT INTO project_sessions(
        project_session_id, project_id, mode, state, revision, generation, authority_ref,
        budget_ref, launch_packet_path, launch_packet_digest, membership_revision,
        origin_kind, origin_operator_id, created_at, updated_at
      ) VALUES (
        'session_01', 'project_01', 'coordinated', 'draft', 2, 1, '${digest}',
        'budget_01', 'launch.json', '${digest}', 1, 'operator-launch', 'operator_01', ${now}, ${now}
      );
    `);
    const actionPorts = (fault?: (label: string) => void) => createProductionOperatorActionPorts({
      database,
      clock: () => now,
      providerActionAdmission: new ProviderActionAdmissionCoordinator({ database, clock: () => now }),
      adapter: {
        capabilities: async () => { throw new Error("effect-free cancel must not inspect an adapter"); },
        dispatch: async () => { throw new Error("effect-free cancel must not dispatch an adapter"); },
        lookup: async () => { throw new Error("effect-free cancel must not look up an adapter"); },
      },
      ...(fault === undefined ? {} : { fault }),
    });
    const crashing = actionPorts((label) => {
      if (label === "operator-effect:after-owned-dispatch") throw new Error("crash after effect-free cancellation");
    });
    const intent = {
      kind: "control" as const,
      action: "cancel" as const,
      target: {
        kind: "session" as const,
        projectSessionId: "session_01" as never,
        expectedRevision: 2,
        expectedGeneration: 1,
      },
      reason: "cancel unused draft",
    };
    const request = scopedEffectRequest(
      "cancel_effect_free_crash",
      intent,
      stateDigest(await crashing.statePort.read(intent)),
    );

    await expect(crashing.effectPort.dispatch(request)).rejects.toThrow("crash after effect-free cancellation");
    expect(database.prepare(`
      SELECT state, outcome_json FROM operator_effect_custody WHERE command_id='cancel_effect_free_crash'
    `).get()).toMatchObject({ state: "terminal", outcome_json: expect.stringContaining('"status":"committed"') });
    expect(database.prepare("SELECT state, revision FROM project_sessions WHERE project_session_id='session_01'").get())
      .toEqual({ state: "cancelled", revision: 3 });

    await expect(actionPorts().effectPort.observe({ ...request, attemptGeneration: 2, effectRef: null }))
      .resolves.toMatchObject({
        status: "committed",
        afterState: { lifecycleState: "cancelled", cancelledTasks: 0 },
      });
  });

  it("rejects run cancellation when no task changed and leaves the session active", async () => {
    const { database, ports } = fixture();
    database.prepare("UPDATE tasks SET state='complete' WHERE run_id='run_01'").run();
    const intent = {
      kind: "control" as const,
      action: "cancel" as const,
      target: {
        kind: "run" as const,
        projectSessionId: "session_01" as never,
        coordinationRunId: "run_01" as never,
        expectedRevision: 4,
      },
      reason: "nothing remains to cancel",
    };

    const before = await ports.statePort.read(intent);
    await expect(ports.effectPort.dispatch(scopedEffectRequest(
      "cancel_zero_tasks",
      intent,
      stateDigest(before),
    ))).resolves.toEqual({ status: "rejected", code: "state-changed", evidenceRefs: [] });
    expect(database.prepare("SELECT state FROM project_sessions WHERE project_session_id='session_01'").get())
      .toEqual({ state: "active" });
  });

  it("pauses and resumes an idle exact-revision task without provider I/O", async () => {
    const { database, ports } = fixture();
    database.exec(`
      INSERT INTO tasks(
        run_id, task_id, authority_id, objective, base_revision, state,
        owner_agent_id, revision, owner_lease_generation, created_by
      ) VALUES (
        'run_01', 'task_02', 'authority_01', 'Unaffected sibling', 'base_02', 'active',
        'chair_01', 7, 1, 'chair_01'
      );
    `);
    const target = {
      kind: "task" as const,
      projectSessionId: "session_01" as never,
      coordinationRunId: "run_01" as never,
      taskId: "task_01" as never,
      expectedRevision: 3,
    };
    const pause = { kind: "control" as const, action: "pause" as const, target };

    await expect(ports.statePort.read(pause)).resolves.toMatchObject({
      kind: "control",
      revision: 3,
      lifecycleState: "active",
      eligibleActions: expect.arrayContaining(["pause", "cancel"]),
    });
    await expect(ports.effectPort.dispatch({
      commandId: "pause_01",
      intent: pause,
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    })).resolves.toMatchObject({
      status: "committed",
      afterState: { lifecycleState: "paused" },
    });
    expect(database.prepare("SELECT lifecycle FROM agents WHERE run_id='run_01' AND agent_id='chair_01'").get())
      .toEqual({ lifecycle: "ready" });
    expect(database.prepare("SELECT task_id, state FROM operator_control_fences").all())
      .toEqual([{ task_id: "task_01", state: "paused" }]);
    expect(() => assertOperatorTaskRunnable(database, "run_01", "task_01"))
      .toThrow(/task is paused/u);
    expect(() => assertOperatorTaskRunnable(database, "run_01", "task_02"))
      .not.toThrow();
    expect(() => assertRunAcceptingWork(database, "run_01")).not.toThrow();

    const resume = { kind: "control" as const, action: "resume" as const, target };
    await expect(ports.statePort.read(resume)).resolves.toMatchObject({
      kind: "control",
      revision: 3,
      lifecycleState: "paused",
      eligibleActions: expect.arrayContaining(["resume", "cancel"]),
    });
    await expect(ports.effectPort.dispatch({
      commandId: "resume_01",
      intent: resume,
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    })).resolves.toMatchObject({
      status: "committed",
      afterState: { lifecycleState: "active" },
    });
    expect(database.prepare("SELECT lifecycle FROM agents WHERE run_id='run_01' AND agent_id='chair_01'").get())
      .toEqual({ lifecycle: "ready" });
    expect(database.prepare("SELECT task_id, state FROM operator_control_fences").all())
      .toEqual([{ task_id: "task_01", state: "released" }]);
  });

  it("atomically journals local pause, resume, and cancel before a post-mutation crash", async () => {
    const { database } = fixture();
    const crashing = createProductionOperatorActionPorts({
      database,
      clock: () => now,
      providerActionAdmission: new ProviderActionAdmissionCoordinator({ database, clock: () => now }),
      adapter: {
        capabilities: async () => { throw new Error("local control must not inspect an adapter"); },
        dispatch: async () => { throw new Error("local control must not dispatch an adapter"); },
        lookup: async () => { throw new Error("local control must not look up an adapter"); },
      },
      fault: (label) => {
        if (label === "operator-effect:after-owned-dispatch") throw new Error("crash after local mutation");
      },
    });
    const recovered = createProductionOperatorActionPorts({
      database,
      clock: () => now,
      providerActionAdmission: new ProviderActionAdmissionCoordinator({ database, clock: () => now }),
      adapter: {
        capabilities: async () => { throw new Error("recovery must not inspect an adapter"); },
        dispatch: async () => { throw new Error("recovery must not dispatch an adapter"); },
        lookup: async () => { throw new Error("recovery must not look up an adapter"); },
      },
    });
    const target = {
      kind: "task" as const,
      projectSessionId: "session_01" as never,
      coordinationRunId: "run_01" as never,
      taskId: "task_01" as never,
      expectedRevision: 3,
    };

    const pause = { kind: "control" as const, action: "pause" as const, target };
    const pauseRequest = scopedEffectRequest("pause_local_crash_01", pause, stateDigest(await crashing.statePort.read(pause)));
    await expect(crashing.effectPort.dispatch(pauseRequest)).rejects.toThrow("crash after local mutation");
    expect(database.prepare("SELECT state FROM operator_effect_custody WHERE command_id='pause_local_crash_01'").get())
      .toEqual({ state: "terminal" });
    await expect(recovered.effectPort.observe({ ...pauseRequest, attemptGeneration: 2, effectRef: null }))
      .resolves.toMatchObject({ status: "committed", afterState: { lifecycleState: "paused" } });

    const resume = { kind: "control" as const, action: "resume" as const, target };
    const resumeRequest = scopedEffectRequest("resume_local_crash_01", resume, stateDigest(await crashing.statePort.read(resume)));
    await expect(crashing.effectPort.dispatch(resumeRequest)).rejects.toThrow("crash after local mutation");
    expect(database.prepare("SELECT state FROM operator_effect_custody WHERE command_id='resume_local_crash_01'").get())
      .toEqual({ state: "terminal" });
    await expect(recovered.effectPort.observe({ ...resumeRequest, attemptGeneration: 2, effectRef: null }))
      .resolves.toMatchObject({ status: "committed", afterState: { lifecycleState: "active" } });

    const cancel = { kind: "control" as const, action: "cancel" as const, target, reason: "superseded" };
    const cancelRequest = scopedEffectRequest("cancel_local_crash_01", cancel, stateDigest(await crashing.statePort.read(cancel)));
    await expect(crashing.effectPort.dispatch(cancelRequest)).rejects.toThrow("crash after local mutation");
    expect(database.prepare("SELECT state FROM operator_effect_custody WHERE command_id='cancel_local_crash_01'").get())
      .toEqual({ state: "terminal" });
    await expect(recovered.effectPort.observe({ ...cancelRequest, attemptGeneration: 2, effectRef: null }))
      .resolves.toMatchObject({ status: "committed", afterState: { lifecycleState: "cancelled" } });
  });

  it("looks up an ambiguous pause action without replaying provider I/O", async () => {
    let dispatches = 0;
    let lookups = 0;
    let providerActionId = "";
    let providerPayload: Record<string, unknown> = {};
    const { database, ports } = fixture({
      capabilities: async () => ({ actionJournal: true, operations: ["interrupt", "lookup_action"] }),
      dispatch: async (_adapterId, input) => {
        dispatches += 1;
        providerActionId = input.actionId;
        providerPayload = input.payload;
        throw new Error("connection lost after dispatch");
      },
      lookup: async (_adapterId, actionId) => {
        lookups += 1;
        expect(actionId).toBe(providerActionId);
        return {
          actionId,
          operation: "interrupt",
          payloadHash: sha256(canonicalJson(providerPayload)),
          status: "terminal",
          history: ["prepared", "dispatched", "accepted", "terminal"],
          executionCount: 1,
          effectCount: 1,
          result: {
            interrupted: true,
            resumeReference: "provider_session_01",
            turnId: "native_turn_01",
          },
        };
      },
    });
    seedProviderAction(database, {
      actionId: "turn_01",
      payloadJson: '{"taskId":"task_01"}',
      status: "dispatched",
      historyJson: '["prepared","dispatched"]',
      resultJson: '{"turnId":"native_turn_01"}',
    });
    database.exec(`
      INSERT INTO provider_session_turn_leases(
        run_id, adapter_id, agent_id, provider_session_generation, turn_lease_generation,
        action_id, status, created_at, updated_at
      ) VALUES ('run_01', 'fake', 'chair_01', 2, 1, 'turn_01', 'active', ${now}, ${now});
    `);
    const pause = {
      kind: "control" as const,
      action: "pause" as const,
      target: {
        kind: "task" as const,
        projectSessionId: "session_01" as never,
        coordinationRunId: "run_01" as never,
        taskId: "task_01" as never,
        expectedRevision: 3,
      },
    };
    const request = {
      commandId: "pause_ambiguous_01",
      intent: pause,
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    };

    const ambiguous = await ports.effectPort.dispatch(request);
    expect(ambiguous).toMatchObject({ status: "ambiguous" });
    if (ambiguous.status !== "ambiguous" || !("effectRef" in ambiguous) || ambiguous.effectRef === undefined) {
      throw new Error("expected an effect reference");
    }
    expect(dispatches).toBe(1);

    await expect(ports.effectPort.dispatch(request)).resolves.toEqual(ambiguous);
    expect(dispatches).toBe(1);

    await expect(ports.effectPort.observe({ ...request, attemptGeneration: 2, effectRef: ambiguous.effectRef }))
      .resolves.toMatchObject({ status: "committed", afterState: { lifecycleState: "paused" } });
    expect(dispatches).toBe(1);
    expect(lookups).toBe(1);
    expect(database.prepare("SELECT task_id, state FROM operator_control_fences").all())
      .toEqual([{ task_id: "task_01", state: "paused" }]);
    expect(database.prepare("SELECT status FROM provider_session_turn_leases WHERE action_id='turn_01'").get())
      .toEqual({ status: "released" });
    expect(database.prepare("SELECT status FROM provider_actions WHERE action_id='turn_01'").get())
      .toEqual({ status: "terminal" });
  });

  it("does not interrupt a shared owner's unrelated active turn", async () => {
    let adapterCalls = 0;
    const { database, ports } = fixture({
      capabilities: async () => { adapterCalls += 1; return {}; },
      dispatch: async () => { adapterCalls += 1; return {}; },
      lookup: async () => { adapterCalls += 1; return {}; },
    });
    seedProviderAction(database, {
      actionId: "turn_sibling",
      payloadJson: '{"taskId":"task_02"}',
      status: "dispatched",
      historyJson: '["prepared","dispatched"]',
      resultJson: '{"turnId":"native_sibling"}',
    });
    database.exec(`
      INSERT INTO tasks(
        run_id, task_id, authority_id, objective, base_revision, state,
        owner_agent_id, revision, owner_lease_generation, created_by
      ) VALUES (
        'run_01', 'task_02', 'authority_01', 'Unrelated live turn', 'base_02', 'active',
        'chair_01', 7, 1, 'chair_01'
      );
      INSERT INTO provider_session_turn_leases(
        run_id, adapter_id, agent_id, provider_session_generation, turn_lease_generation,
        action_id, status, created_at, updated_at
      ) VALUES ('run_01', 'fake', 'chair_01', 2, 1, 'turn_sibling', 'active', ${now}, ${now});
    `);
    const pause = {
      kind: "control" as const,
      action: "pause" as const,
      target: {
        kind: "task" as const,
        projectSessionId: "session_01" as never,
        coordinationRunId: "run_01" as never,
        taskId: "task_01" as never,
        expectedRevision: 3,
      },
    };

    await expect(ports.effectPort.dispatch({
      commandId: "pause_scoped_01",
      intent: pause,
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    })).resolves.toMatchObject({ status: "committed", afterState: { lifecycleState: "paused" } });
    expect(adapterCalls).toBe(0);
    expect(database.prepare("SELECT status FROM provider_session_turn_leases WHERE action_id='turn_sibling'").get())
      .toEqual({ status: "active" });
    expect(database.prepare("SELECT lifecycle FROM agents WHERE run_id='run_01' AND agent_id='chair_01'").get())
      .toEqual({ lifecycle: "ready" });
  });

  it("interrupts an exact task participant turn even when the task owner is idle", async () => {
    const dispatchedAgents: string[] = [];
    const { database, ports } = fixture({
      capabilities: async () => ({ actionJournal: true, operations: ["interrupt", "lookup_action"] }),
      dispatch: async (_adapterId, input) => {
        dispatchedAgents.push(String(input.payload.agentId));
        return {
          actionId: input.actionId,
          operation: "interrupt",
          payloadHash: sha256(canonicalJson(input.payload)),
          status: "terminal",
          history: ["prepared", "dispatched", "accepted", "terminal"],
          executionCount: 1,
          effectCount: 1,
          result: {
            interrupted: true,
            resumeReference: "provider_worker_01",
            turnId: "participant_turn_01",
          },
        };
      },
      lookup: async () => { throw new Error("terminal interrupt must not be looked up"); },
    });
    database.exec(`
      INSERT INTO agents(run_id, agent_id, authority_id, provider_session_ref, lifecycle)
      VALUES ('run_01', 'worker_01', 'authority_01', 'provider_worker_01', 'ready');
      INSERT INTO provider_state(run_id, agent_id, provider_session_generation, context_revision)
      VALUES ('run_01', 'worker_01', 3, 'worker_context_01');
      INSERT INTO agent_adapter_bindings(run_id, agent_id, adapter_id, bound_at)
      VALUES ('run_01', 'worker_01', 'fake', ${now});
      INSERT INTO task_participants(run_id, task_id, agent_id)
      VALUES ('run_01', 'task_01', 'worker_01');
    `);
    seedProviderAction(database, {
      actionId: "participant_turn_source",
      targetAgentId: "worker_01",
      providerSessionGeneration: 3,
      payloadJson: '{"taskId":"task_01"}',
      status: "accepted",
      historyJson: '["prepared","dispatched","accepted"]',
      effectCount: 1,
      resultJson: '{"turnId":"participant_turn_01"}',
    });
    database.exec(`
      INSERT INTO provider_session_turn_leases(
        run_id, adapter_id, agent_id, provider_session_generation, turn_lease_generation,
        action_id, status, created_at, updated_at
      ) VALUES ('run_01', 'fake', 'worker_01', 3, 1, 'participant_turn_source', 'active', ${now}, ${now});
    `);
    const intent = {
      kind: "control" as const,
      action: "pause" as const,
      target: {
        kind: "task" as const,
        projectSessionId: "session_01" as never,
        coordinationRunId: "run_01" as never,
        taskId: "task_01" as never,
        expectedRevision: 3,
      },
    };
    const before = await ports.statePort.read(intent);
    expect(before).toMatchObject({
      binding: { turns: [{ agentId: "worker_01", sourceActionId: "participant_turn_source" }] },
    });

    await expect(ports.effectPort.dispatch(scopedEffectRequest(
      "pause_participant_turn_01",
      intent,
      stateDigest(before),
    ))).resolves.toMatchObject({ status: "committed", afterState: { lifecycleState: "paused" } });
    expect(dispatchedAgents).toEqual(["worker_01"]);
    expect(database.prepare("SELECT status FROM provider_session_turn_leases WHERE action_id='participant_turn_source'").get())
      .toEqual({ status: "released" });
    expect(database.prepare("SELECT state FROM operator_control_fences WHERE task_id='task_01'").get())
      .toEqual({ state: "paused" });
    expect(database.prepare(`
      SELECT binding.run_id,binding.adapter_id,binding.source_action_id,
             binding.source_payload_hash,binding.operation,binding.target_agent_id,
             binding.provider_session_ref,binding.provider_session_generation,
             binding.turn_lease_generation,binding.turn_id
        FROM operator_control_provider_action_bindings binding
    `).get()).toEqual({
      run_id: "run_01",
      adapter_id: "fake",
      source_action_id: "participant_turn_source",
      source_payload_hash: "d".repeat(64),
      operation: "interrupt",
      target_agent_id: "worker_01",
      provider_session_ref: "provider_worker_01",
      provider_session_generation: 3,
      turn_lease_generation: 1,
      turn_id: "participant_turn_01",
    });
    expect(() => database.prepare(`
      UPDATE operator_control_provider_action_bindings SET turn_id='changed'
    `).run()).toThrow("INVARIANT_operator_control_provider_action_binding_immutable");
    expect(() => database.prepare(`
      DELETE FROM operator_control_provider_action_bindings
    `).run()).toThrow("INVARIANT_operator_control_provider_action_binding_immutable");
    expect(() => database.prepare(`
      INSERT INTO operator_control_provider_action_bindings
      SELECT * FROM operator_control_provider_action_bindings
    `).run()).toThrow();

    const validBinding = database.prepare(`
      SELECT * FROM operator_control_provider_action_bindings
    `).get() as Record<string, unknown>;
    const sourceAction = database.prepare(`
      SELECT * FROM provider_actions WHERE adapter_id=? AND action_id=?
    `).get(validBinding.adapter_id, validBinding.action_id) as Record<string, unknown>;
    const crossedActionId = `${String(validBinding.action_id)}:crossed-identity`;
    const actionColumns = Object.keys(sourceAction);
    database.pragma("foreign_keys = OFF");
    database.prepare(`
      INSERT INTO provider_actions(${actionColumns.map((column) => `"${column}"`).join(",")})
      VALUES (${actionColumns.map((column) => `@${column}`).join(",")})
    `).run({ ...sourceAction, action_id: crossedActionId });
    const bindingColumns = Object.keys(validBinding);
    const insertBinding = database.prepare(`
      INSERT INTO operator_control_provider_action_bindings(
        ${bindingColumns.map((column) => `"${column}"`).join(",")}
      ) VALUES (${bindingColumns.map((column) => `@${column}`).join(",")})
    `);
    const exactCandidate = { ...validBinding, action_id: crossedActionId };
    const crossedIdentities = [
      { custody_id: "missing-custody" },
      { run_id: "missing-run" },
      { adapter_id: "missing-adapter" },
      { source_adapter_id: "missing-source-adapter" },
      { source_action_id: "missing-source-action" },
      { source_payload_hash: "e".repeat(64) },
      { operation: "steer" },
      { target_agent_id: "missing-agent" },
      { provider_session_ref: "missing-session" },
      { provider_session_generation: 4 },
      { turn_lease_generation: 2 },
      { turn_id: "missing-turn" },
    ];
    for (const crossedIdentity of crossedIdentities) {
      expect(() => insertBinding.run({ ...exactCandidate, ...crossedIdentity }))
        .toThrow("INVARIANT_operator_control_provider_action_binding_identity");
    }
    expect(database.prepare(`
      SELECT COUNT(*) AS count FROM operator_control_provider_action_bindings
    `).get()).toEqual({ count: 1 });
    database.prepare(`
      DELETE FROM provider_actions WHERE adapter_id=? AND action_id=?
    `).run(validBinding.adapter_id, crossedActionId);
    database.pragma("foreign_keys = ON");
  });

  it("keeps unresolved pairs retryable when a multi-agent capability check throws once", async () => {
    let dispatches = 0;
    let secondAdapterReady = false;
    const releasedDispositions: string[] = [];
    const { database, ports } = fixture({
      capabilities: async (adapterId) => {
        if (adapterId === "fake_bad" && !secondAdapterReady) {
          throw new Error("transient capability discovery failure");
        }
        return { actionJournal: true, operations: ["interrupt", "lookup_action"] };
      },
      dispatch: async (_adapterId, input) => {
        dispatches += 1;
        return {
          actionId: input.actionId,
          operation: input.operation,
          payloadHash: sha256(canonicalJson(input.payload)),
          status: "terminal",
          history: ["prepared", "dispatched", "accepted", "terminal"],
          executionCount: 1,
          effectCount: 1,
          result: {
            interrupted: true,
            resumeReference: input.payload.resumeReference,
            turnId: input.payload.turnId,
          },
        };
      },
      lookup: async () => { throw new Error("rejected preflight must not perform lookup"); },
    }, (coordinator) => {
      const release = coordinator.release.bind(coordinator);
      coordinator.release = ((ticket, failure) => {
        releasedDispositions.push(ticket.disposition);
        release(ticket, failure);
      }) as typeof coordinator.release;
    });
    database.exec(`
      INSERT INTO agents(run_id, agent_id, authority_id, provider_session_ref, lifecycle)
      VALUES ('run_01', 'worker_02', 'authority_01', 'provider_session_02', 'ready');
      INSERT INTO provider_state(run_id, agent_id, provider_session_generation, context_revision)
      VALUES ('run_01', 'worker_02', 3, 'context_02');
      INSERT INTO agent_adapter_bindings(run_id, agent_id, adapter_id, bound_at)
      VALUES ('run_01', 'worker_02', 'fake_bad', ${now});
      INSERT INTO tasks(
        run_id, task_id, authority_id, objective, base_revision, state,
        owner_agent_id, revision, owner_lease_generation, created_by
      ) VALUES (
        'run_01', 'task_02', 'authority_01', 'Second live turn', 'base_02', 'active',
        'worker_02', 7, 1, 'chair_01'
      );
    `);
    seedProviderAction(database, {
      actionId: "turn_chair",
      payloadJson: '{"taskId":"task_01"}',
      status: "accepted",
      historyJson: '["prepared","dispatched","accepted"]',
      resultJson: '{"turnId":"turn_chair_native"}',
    });
    seedProviderAction(database, {
      actionId: "turn_worker",
      adapterId: "fake_bad",
      targetAgentId: "worker_02",
      providerSessionGeneration: 3,
      payloadJson: '{"taskId":"task_02"}',
      status: "accepted",
      historyJson: '["prepared","dispatched","accepted"]',
      resultJson: '{"turnId":"turn_worker_native"}',
    });
    database.exec(`
      INSERT INTO provider_session_turn_leases(
        run_id, adapter_id, agent_id, provider_session_generation, turn_lease_generation,
        action_id, status, created_at, updated_at
      ) VALUES
        ('run_01', 'fake', 'chair_01', 2, 1, 'turn_chair', 'active', ${now}, ${now}),
        ('run_01', 'fake_bad', 'worker_02', 3, 1, 'turn_worker', 'active', ${now}, ${now});
    `);

    await expect(ports.effectPort.dispatch({
      commandId: "pause_run_preflight_01",
      intent: {
        kind: "control",
        action: "pause",
        target: {
          kind: "run",
          projectSessionId: "session_01" as never,
          coordinationRunId: "run_01" as never,
          expectedRevision: 4,
        },
      },
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    })).rejects.toThrow("transient capability discovery failure");
    expect(dispatches).toBe(0);
    expect(database.prepare("SELECT COUNT(*) AS count FROM provider_actions WHERE operation='interrupt'").get())
      .toEqual({ count: 0 });
    expect(database.prepare(`
      SELECT state, COUNT(*) AS count
        FROM provider_action_pair_preflights
       WHERE action_id GLOB 'operator-*'
       GROUP BY state
    `).all()).toEqual([{ state: "resolving", count: 2 }]);
    expect(releasedDispositions).toEqual([]);

    secondAdapterReady = true;
    await expect(ports.effectPort.dispatch({
      commandId: "pause_run_preflight_retry_01",
      intent: {
        kind: "control",
        action: "pause",
        target: {
          kind: "run",
          projectSessionId: "session_01" as never,
          coordinationRunId: "run_01" as never,
          expectedRevision: 4,
        },
      },
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    })).resolves.toMatchObject({ status: "committed" });
    expect(dispatches).toBe(2);
    expect(database.prepare(`
      SELECT state, COUNT(*) AS count
        FROM provider_action_pair_preflights
       WHERE adapter_id IN ('fake','fake_bad')
         AND action_id GLOB 'operator-*'
       GROUP BY state
    `).all()).toEqual([{ state: "admitted", count: 2 }]);
  });

  it("rolls back an operator action and its owner binding atomically before provider effect", async () => {
    let dispatches = 0;
    const { database, ports } = fixture({
      capabilities: async () => ({ actionJournal: true, operations: ["interrupt", "lookup_action"] }),
      dispatch: async () => {
        dispatches += 1;
        throw new Error("provider effect must not run after owner-binding rollback");
      },
      lookup: async () => { throw new Error("owner-binding rollback must not look up"); },
    }, undefined, (label) => {
      if (label === "provider-action-admission:after-dependants") {
        throw new Error("crash after operator owner binding");
      }
    });
    seedProviderAction(database, {
      actionId: "atomic_operator_source",
      payloadJson: '{"taskId":"task_01"}',
      status: "accepted",
      historyJson: '["prepared","dispatched","accepted"]',
      effectCount: 1,
      resultJson: '{"turnId":"atomic_operator_turn"}',
    });
    database.prepare(`
      INSERT INTO provider_session_turn_leases(
        run_id,adapter_id,agent_id,provider_session_generation,turn_lease_generation,
        action_id,status,created_at,updated_at
      ) VALUES ('run_01','fake','chair_01',2,1,'atomic_operator_source','active',?,?)
    `).run(now, now);
    const intent = {
      kind: "control" as const,
      action: "pause" as const,
      target: {
        kind: "task" as const,
        projectSessionId: "session_01" as never,
        coordinationRunId: "run_01" as never,
        taskId: "task_01" as never,
        expectedRevision: 3,
      },
    };
    const before = await ports.statePort.read(intent);
    await expect(ports.effectPort.dispatch(scopedEffectRequest(
      "atomic_operator_binding",
      intent,
      stateDigest(before),
    ))).rejects.toThrow("crash after operator owner binding");
    expect(dispatches).toBe(0);
    expect(database.prepare(`
      SELECT COUNT(*) AS count FROM provider_actions WHERE operation='interrupt'
    `).get()).toEqual({ count: 0 });
    expect(database.prepare(`
      SELECT COUNT(*) AS count FROM operator_control_provider_action_bindings
    `).get()).toEqual({ count: 0 });
    expect(database.prepare(`
      SELECT state FROM provider_action_pair_preflights WHERE action_id GLOB 'operator-*'
    `).get()).toEqual({ state: "resolving" });
  });

  it("does not claim pause from a terminal adapter row with the wrong operation or replay count", async () => {
    let providerActionId = "";
    const { database, ports } = fixture({
      capabilities: async () => ({ actionJournal: true, operations: ["interrupt", "lookup_action"] }),
      dispatch: async (_adapterId, input) => {
        providerActionId = input.actionId;
        throw new Error("ambiguous dispatch");
      },
      lookup: async () => ({
        actionId: providerActionId,
        operation: "steer",
        status: "terminal",
        history: ["prepared", "dispatched", "terminal"],
        executionCount: 2,
        effectCount: 1,
        result: { interrupted: true },
      }),
    });
    seedProviderAction(database, {
      actionId: "turn_unproved",
      payloadJson: '{"taskId":"task_01"}',
      status: "dispatched",
      historyJson: '["prepared","dispatched"]',
      resultJson: '{"turnId":"native_unproved"}',
    });
    database.exec(`
      INSERT INTO provider_session_turn_leases(
        run_id, adapter_id, agent_id, provider_session_generation, turn_lease_generation,
        action_id, status, created_at, updated_at
      ) VALUES ('run_01', 'fake', 'chair_01', 2, 1, 'turn_unproved', 'active', ${now}, ${now});
    `);
    const request = {
      commandId: "pause_unproved_01",
      intent: {
        kind: "control" as const,
        action: "pause" as const,
        target: {
          kind: "task" as const,
          projectSessionId: "session_01" as never,
          coordinationRunId: "run_01" as never,
          taskId: "task_01" as never,
          expectedRevision: 3,
        },
      },
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    };
    const ambiguous = await ports.effectPort.dispatch(request);
    if (ambiguous.status !== "ambiguous" || !("effectRef" in ambiguous) || ambiguous.effectRef === undefined) {
      throw new Error("expected ambiguous control effect");
    }

    await expect(ports.effectPort.observe({ ...request, attemptGeneration: 2, effectRef: ambiguous.effectRef }))
      .resolves.toMatchObject({ status: "ambiguous" });
    expect(database.prepare("SELECT COUNT(*) AS count FROM operator_control_fences").get()).toEqual({ count: 0 });
  });

  it("steers only an exact task-attributed active turn and requires a proved adapter effect", async () => {
    const dispatched: Array<Record<string, unknown>> = [];
    const { database, ports } = fixture({
      capabilities: async () => ({ actionJournal: true, operations: ["steer", "lookup_action"] }),
      dispatch: async (_adapterId, input) => {
        dispatched.push(input.payload);
        return {
          actionId: input.actionId,
          operation: "steer",
          payloadHash: sha256(canonicalJson(input.payload)),
          status: "terminal",
          history: ["prepared", "dispatched", "accepted", "terminal"],
          executionCount: 1,
          effectCount: 1,
          result: {
            steered: true,
            resumeReference: "provider_session_01",
            turnId: "turn_provider_01",
          },
        };
      },
      lookup: async () => { throw new Error("terminal steer must not be looked up"); },
    });
    seedProviderAction(database, {
      actionId: "turn_steer",
      payloadJson: '{"taskId":"task_01"}',
      status: "accepted",
      historyJson: '["prepared","dispatched","accepted"]',
      effectCount: 1,
      resultJson: '{"turnId":"turn_provider_01"}',
    });
    database.exec(`
      INSERT INTO provider_session_turn_leases(
        run_id, adapter_id, agent_id, provider_session_generation, turn_lease_generation,
        action_id, status, created_at, updated_at
      ) VALUES ('run_01', 'fake', 'chair_01', 2, 1, 'turn_steer', 'active', ${now}, ${now});
    `);

    await expect(ports.effectPort.dispatch({
      commandId: "steer_01",
      intent: {
        kind: "control",
        action: "steer",
        target: {
          kind: "task",
          projectSessionId: "session_01" as never,
          coordinationRunId: "run_01" as never,
          taskId: "task_01" as never,
          expectedRevision: 3,
        },
        instruction: "Return only the bounded evidence.",
        evidenceRefs: [],
      },
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    })).resolves.toMatchObject({ status: "committed", afterState: { steered: true } });
    expect(dispatched).toEqual([expect.objectContaining({
      sourceActionId: "turn_steer",
      turnId: "turn_provider_01",
      expectedTurnId: "turn_provider_01",
      instruction: "Return only the bounded evidence.",
      providerSessionGeneration: 2,
      turnLeaseGeneration: 1,
    })]);
  });

  it("joins fresh-command steer replay and skips capabilities after restart", async () => {
    let capabilityCalls = 0;
    let dispatchCalls = 0;
    const adapter = {
      capabilities: async () => {
        capabilityCalls += 1;
        await new Promise((resolve) => setTimeout(resolve, 30));
        return { actionJournal: true, operations: ["steer", "lookup_action"] };
      },
      dispatch: async (_adapterId: string, input: { actionId: string; operation: "interrupt" | "steer"; payload: Record<string, unknown> }) => {
        dispatchCalls += 1;
        return {
          actionId: input.actionId,
          operation: input.operation,
          payloadHash: sha256(canonicalJson(input.payload)),
          status: "terminal",
          history: ["prepared", "dispatched", "accepted", "terminal"],
          executionCount: 1,
          effectCount: 1,
          result: {
            steered: true,
            resumeReference: "provider_session_01",
            turnId: "turn_provider_pair",
          },
        };
      },
      lookup: async () => { throw new Error("terminal steer must not be looked up"); },
    };
    const { database, ports } = fixture(adapter);
    admitProviderActionFixture(database, {
      runId: "run_01",
      actionId: "turn_steer_pair",
      adapterId: "fake",
      operation: "send_turn",
      targetAgentId: "chair_01",
      providerSessionGeneration: 2,
      turnLeaseGeneration: 1,
      identityHash: "c".repeat(64),
      payloadHash: "d".repeat(64),
      payloadJson: '{"taskId":"task_01"}',
      status: "accepted",
      historyJson: '["prepared","dispatched","accepted"]',
      executionCount: 1,
      effectCount: 1,
      resultJson: '{"turnId":"turn_provider_pair"}',
      updatedAt: now,
    });
    database.prepare(`
      INSERT INTO provider_session_turn_leases(
        run_id, adapter_id, agent_id, provider_session_generation, turn_lease_generation,
        action_id, status, created_at, updated_at
      ) VALUES ('run_01', 'fake', 'chair_01', 2, 1, 'turn_steer_pair', 'active', ?, ?)
    `).run(now, now);
    const intent = {
      kind: "control" as const,
      action: "steer" as const,
      target: {
        kind: "task" as const,
        projectSessionId: "session_01" as never,
        coordinationRunId: "run_01" as never,
        taskId: "task_01" as never,
        expectedRevision: 3,
      },
      instruction: "Keep this semantic steer single-flight.",
      evidenceRefs: [],
    };
    const request = (commandId: string) => ({
      commandId,
      intent,
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    });

    const [first, second] = await Promise.all([
      ports.effectPort.dispatch(request("steer_pair_first")),
      ports.effectPort.dispatch(request("steer_pair_second")),
    ]);
    expect(first).toEqual(second);
    expect(first).toMatchObject({ status: "committed", afterState: { steered: true } });
    expect(capabilityCalls).toBe(1);
    expect(dispatchCalls).toBe(1);

    const reopened = createProductionOperatorActionPorts({
      database,
      clock: () => now,
      providerActionAdmission: new ProviderActionAdmissionCoordinator({ database, clock: () => now }),
      adapter: {
        capabilities: async () => { throw new Error("admitted replay must not inspect capabilities"); },
        dispatch: async () => { throw new Error("admitted replay must not redispatch"); },
        lookup: async () => { throw new Error("terminal replay must not look up"); },
      },
    });
    await expect(reopened.effectPort.dispatch(request("steer_pair_after_restart"))).resolves.toEqual(first);
  });

  it("cancels only the exact task scope and advances its revision", async () => {
    const { database, ports } = fixture();
    database.exec(`
      INSERT INTO tasks(
        run_id, task_id, authority_id, objective, base_revision, state,
        owner_agent_id, revision, owner_lease_generation, created_by
      ) VALUES (
        'run_01', 'task_02', 'authority_01', 'Unaffected sibling', 'base_02', 'active',
        'chair_01', 7, 1, 'chair_01'
      );
      INSERT INTO task_owner_leases(
        project_session_id, run_id, task_id, lease_id, holder_agent_id,
        generation, status, updated_at
      ) VALUES (
        'session_01', 'run_01', 'task_01', 'task-owner:run_01:task_01:1',
        'chair_01', 1, 'active', ${now}
      ), (
        'session_01', 'run_01', 'task_02', 'task-owner:run_01:task_02:1',
        'chair_01', 1, 'active', ${now}
      );
      INSERT INTO project_session_memberships(
        project_session_id, coordination_run_id, member_kind, member_id,
        required, state, revision, created_at, updated_at
      ) VALUES (
        'session_01', 'run_01', 'task', 'task_01',
        1, 'active', 1, ${now}, ${now}
      ), (
        'session_01', 'run_01', 'task', 'task_02',
        1, 'active', 1, ${now}, ${now}
      ), (
        'session_01', 'run_01', 'lease', 'lease_task_01',
        1, 'active', 1, ${now}, ${now}
      ), (
        'session_01', 'run_01', 'required-message', 'message_task_01',
        1, 'active', 1, ${now}, ${now}
      ), (
        'session_01', 'run_01', 'scoped-barrier', 'barrier_task_01',
        1, 'active', 1, ${now}, ${now}
      ), (
        'session_01', 'run_01', 'required-message', 'reply_task_01',
        1, 'active', 1, ${now}, ${now}
      );
      INSERT INTO leases(lease_id, run_id, kind, holder_agent_id, generation, status, expires_at, updated_at)
      VALUES ('lease_task_01', 'run_01', 'write', 'chair_01', 1, 'active', ${now + 60_000}, ${now});
      INSERT INTO write_scope_entries(lease_id, canonical_path)
      VALUES ('lease_task_01', '/project/one/src/task_01');
      INSERT INTO task_obligation_bindings(
        coordination_run_id, task_id, obligation_kind, obligation_id, state, created_at, updated_at
      ) VALUES
        ('run_01', 'task_01', 'write-lease', 'lease_task_01', 'active', ${now}, ${now}),
        ('run_01', 'task_01', 'resource-reservation', 'reservation_task_01', 'active', ${now}, ${now});
      INSERT INTO resource_scopes(
        scope_id, project_id, project_session_id, coordination_run_id, parent_scope_id,
        scope_kind, owner_ref, state, revision
      ) VALUES ('scope_project_01', 'project_01', NULL, NULL, NULL, 'project', 'project_01', 'active', 1);
      INSERT INTO resource_dimensions(scope_id, unit_key, limit_value, used, reserved, usage_unknown)
      VALUES ('scope_project_01', 'tokens', 10, 0, 3, 0);
      INSERT INTO resource_reservations(
        reservation_id, project_session_id, coordination_run_id, leaf_scope_id,
        operation_id, actor_agent_id, state, revision, generation, identity_hash,
        path_json, amounts_json, created_at, updated_at
      ) VALUES (
        'reservation_task_01', 'session_01', 'run_01', 'scope_project_01',
        'task_01', 'chair_01', 'reserved', 1, 1, '${"8".repeat(64)}',
        '[{"scopeId":"scope_project_01"}]', '{"tokens":3}', ${now}, ${now}
      );
      INSERT INTO resource_reservation_dimensions(
        reservation_id, scope_id, unit_key, amount, consumed, released, usage_unknown
      ) VALUES ('reservation_task_01', 'scope_project_01', 'tokens', 3, 0, 0, 0);
      INSERT INTO writer_admissions(
        writer_admission_id, reservation_id, repository_root, worktree_path, writer_generation, state
      ) VALUES ('writer_task_01', 'reservation_task_01', '/project/one', '/project/one/.worktrees/task_01', 1, 'active');
      INSERT INTO messages(
        message_id, run_id, sender_id, dedupe_key, payload_hash, audience_json,
        kind, body, requires_ack, conversation_id, task_revision, hop_count, created_at
      ) VALUES (
        'message_task_01', 'run_01', 'chair_01', 'request:task_01', '${"9".repeat(64)}',
        '{"kind":"task","taskId":"task_01"}', 'request', 'bounded request', 1,
        'conversation_task_01', 3, 0, ${now}
      ), (
        'reply_task_01', 'run_01', 'chair_01', 'reply:task_01', '${"7".repeat(64)}',
        '{"kind":"agent","agentId":"chair_01"}', 'reply', 'bounded result', 1,
        'conversation_task_01', 4, 0, ${now}
      );
      INSERT INTO deliveries(
        delivery_id, message_id, run_id, recipient_id, mailbox_sequence, state, attempt_count
      ) VALUES
        ('delivery_task_01', 'message_task_01', 'run_01', 'chair_01', 1, 'ready', 0),
        ('delivery_reply_task_01', 'reply_task_01', 'run_01', 'chair_01', 2, 'ready', 0);
      INSERT INTO task_requests(
        request_id, project_session_id, run_id, task_id, requester_agent_id,
        request_revision, conversation_id, request_message_id, target_agent_id,
        target_provider_session, expected_artifacts_json, acknowledgement_required,
        dedupe_key, response_deadline, callback_id, callback_generation,
        dependent_barrier_id, state, payload_digest, created_at, updated_at
      ) VALUES (
        'request_task_01', 'session_01', 'run_01', 'task_01', 'chair_01', 1,
        'conversation_task_01', 'message_task_01', 'chair_01', 'provider_session_01',
        '[]', 1, 'request:task_01', ${now + 60_000}, 'callback_task_01', 1,
        'barrier_task_01', 'pending', '${digest}', ${now}, ${now}
      );
      INSERT INTO task_request_recipients(request_id, delivery_id)
      VALUES ('request_task_01', 'delivery_task_01');
      INSERT INTO task_request_barriers(request_id, barrier_id, state)
      VALUES ('request_task_01', 'barrier_task_01', 'blocked');
      INSERT INTO task_results(
        result_id, request_id, project_session_id, run_id, task_id, task_revision,
        reply_message_id, reply_revision, payload_digest, artifacts_json,
        terminal_state, summary, created_at
      ) VALUES (
        'result_task_01', 'request_task_01', 'session_01', 'run_01', 'task_01', 4,
        'reply_task_01', 1, '${digest}', '[]', 'complete', 'result prepared before crash', ${now}
      );
      INSERT INTO result_deliveries(
        result_delivery_id, callback_id, request_id, result_id, project_session_id,
        run_id, task_id, requester_agent_id, target_provider_session, state, required,
        revision, claim_generation, assignment_generation, response_deadline,
        request_revision, reply_revision, task_revision, payload_digest, updated_at
      ) VALUES (
        'result_delivery_task_01', 'callback_task_01', 'request_task_01', 'result_task_01',
        'session_01', 'run_01', 'task_01', 'chair_01', 'provider_session_01', 'pending', 1,
        1, 0, 1, ${now + 60_000}, 1, 1, 4, '${digest}', ${now}
      );
      INSERT INTO task_expected_artifacts(run_id, task_id, relative_path)
      VALUES ('run_01', 'task_01', 'cancelled-task-missing.txt');
    `);
    const cancel = {
      kind: "control" as const,
      action: "cancel" as const,
      target: {
        kind: "task" as const,
        projectSessionId: "session_01" as never,
        coordinationRunId: "run_01" as never,
        taskId: "task_01" as never,
        expectedRevision: 3,
      },
      reason: "Operator cancelled the bounded task",
    };

    await expect(ports.effectPort.dispatch({
      commandId: "cancel_01",
      intent: cancel,
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    })).resolves.toMatchObject({
      status: "committed",
      afterState: { lifecycleState: "cancelled", cancelledTasks: 1 },
    });
    expect(database.prepare("SELECT state, revision FROM tasks WHERE run_id='run_01' AND task_id='task_01'").get())
      .toEqual({ state: "cancelled", revision: 4 });
    expect(database.prepare("SELECT state, revision FROM tasks WHERE run_id='run_01' AND task_id='task_02'").get())
      .toEqual({ state: "active", revision: 7 });
    expect(database.prepare("SELECT lifecycle FROM agents WHERE run_id='run_01' AND agent_id='chair_01'").get())
      .toEqual({ lifecycle: "ready" });
    expect(database.prepare(`
      SELECT status FROM task_owner_leases WHERE lease_id='task-owner:run_01:task_01:1'
    `).get()).toEqual({ status: "released" });
    expect(database.prepare(`
      SELECT status FROM task_owner_leases WHERE lease_id='task-owner:run_01:task_02:1'
    `).get()).toEqual({ status: "active" });
    expect(database.prepare(`
      SELECT state, revision, abandoned_reason FROM project_session_memberships
       WHERE project_session_id='session_01' AND coordination_run_id='run_01'
         AND member_kind='task' AND member_id='task_01'
    `).get()).toEqual({
      state: "abandoned",
      revision: 2,
      abandoned_reason: "Operator cancelled the bounded task",
    });
    expect(database.prepare(`
      SELECT state, revision, abandoned_reason FROM project_session_memberships
       WHERE project_session_id='session_01' AND coordination_run_id='run_01'
         AND member_kind='task' AND member_id='task_02'
    `).get()).toEqual({ state: "active", revision: 1, abandoned_reason: null });
    expect(database.prepare("SELECT status FROM leases WHERE lease_id='lease_task_01'").get())
      .toEqual({ status: "released" });
    expect(database.prepare("SELECT state FROM task_obligation_bindings WHERE obligation_id='lease_task_01'").get())
      .toEqual({ state: "abandoned" });
    expect(database.prepare("SELECT state FROM task_obligation_bindings WHERE obligation_id='reservation_task_01'").get())
      .toEqual({ state: "abandoned" });
    expect(database.prepare("SELECT state FROM resource_reservations WHERE reservation_id='reservation_task_01'").get())
      .toEqual({ state: "released" });
    expect(database.prepare("SELECT reserved FROM resource_dimensions WHERE scope_id='scope_project_01' AND unit_key='tokens'").get())
      .toEqual({ reserved: 0 });
    expect(database.prepare("SELECT state FROM writer_admissions WHERE writer_admission_id='writer_task_01'").get())
      .toEqual({ state: "revoked" });
    expect(database.prepare("SELECT state FROM task_requests WHERE request_id='request_task_01'").get())
      .toEqual({ state: "abandoned" });
    expect(database.prepare("SELECT state FROM task_request_barriers WHERE request_id='request_task_01'").get())
      .toEqual({ state: "abandoned" });
    expect(database.prepare("SELECT state, resolution_reason FROM deliveries WHERE delivery_id='delivery_task_01'").get())
      .toEqual({ state: "abandoned", resolution_reason: "Operator cancelled the bounded task" });
    expect(database.prepare("SELECT state, required, abandoned_reason FROM result_deliveries WHERE result_delivery_id='result_delivery_task_01'").get())
      .toEqual({ state: "abandoned", required: 0, abandoned_reason: "Operator cancelled the bounded task" });
    expect(database.prepare("SELECT state, resolution_reason FROM deliveries WHERE delivery_id='delivery_reply_task_01'").get())
      .toEqual({ state: "abandoned", resolution_reason: "Operator cancelled the bounded task" });
    expect(database.prepare(`
      SELECT kind, state FROM attention_items WHERE dedupe_key='operator-cancel:run_01:task_01:3'
    `).get()).toEqual({ kind: "operator-task-cancelled", state: "open" });
  });

  it("does not retain missing artifacts from a cancelled task as drain obligations", async () => {
    const { database, ports } = fixture();
    database.prepare(`
      INSERT INTO task_expected_artifacts(run_id, task_id, relative_path)
      VALUES ('run_01', 'task_01', 'cancelled-task-missing.txt')
    `).run();
    await ports.effectPort.dispatch({
      commandId: "cancel_before_drain",
      intent: {
        kind: "control",
        action: "cancel",
        target: {
          kind: "task",
          projectSessionId: "session_01" as never,
          coordinationRunId: "run_01" as never,
          taskId: "task_01" as never,
          expectedRevision: 3,
        },
        reason: "cancel before drain",
      },
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    });
    const session = database.prepare(`
      SELECT revision, generation FROM project_sessions WHERE project_session_id='session_01'
    `).get() as { revision: number; generation: number };
    const global = database.prepare("SELECT revision FROM daemon_global_state WHERE singleton=1")
      .get() as { revision: number };
    const intent = {
      kind: "project-session-drain" as const,
      projectSessionId: "session_01" as never,
      expectedSessionRevision: session.revision,
      expectedSessionGeneration: session.generation,
      expectedGlobalStateRevision: global.revision,
    };
    const before = await ports.statePort.read(intent);
    const request = scopedEffectRequest("drain_after_cancel", intent, stateDigest(before));
    ports.effectPort.prepare?.(request);

    await expect(ports.effectPort.dispatch(request)).resolves.toMatchObject({
      status: "committed",
      afterState: { lifecycleState: "quiescing", obligationsSettled: true },
    });
  });

  it("rejects stale revision, generation, and wrong-session control targets before effects", async () => {
    const { ports } = fixture();
    const base = {
      kind: "control" as const,
      action: "pause" as const,
    };
    await expect(ports.statePort.read({
      ...base,
      target: {
        kind: "task",
        projectSessionId: "session_01" as never,
        coordinationRunId: "run_01" as never,
        taskId: "task_01" as never,
        expectedRevision: 2,
      },
    })).rejects.toMatchObject({ code: "STALE_REVISION" });
    await expect(ports.statePort.read({
      ...base,
      target: {
        kind: "session",
        projectSessionId: "session_01" as never,
        expectedRevision: 2,
        expectedGeneration: 2,
      },
    })).rejects.toMatchObject({ code: "STALE_GENERATION" });
    await expect(ports.statePort.read({
      ...base,
      target: {
        kind: "task",
        projectSessionId: "session_other" as never,
        coordinationRunId: "run_01" as never,
        taskId: "task_01" as never,
        expectedRevision: 3,
      },
    })).rejects.toMatchObject({ code: "NOT_FOUND" });
  });

  it("binds operator effects to the live principal generation", async () => {
    const { database, ports } = fixture();
    database.prepare(`
      INSERT INTO operator_principals(
        operator_id, project_id, project_session_id, authenticated_subject_hash,
        project_authority_generation, principal_generation, state, created_at, updated_at
      ) VALUES ('operator_01','project_01','session_01','operator-subject',1,3,'active',?,?)
    `).run(now, now);
    const intent = {
      kind: "control" as const,
      action: "pause" as const,
      target: {
        kind: "task" as const,
        projectSessionId: "session_01" as never,
        coordinationRunId: "run_01" as never,
        taskId: "task_01" as never,
        expectedRevision: 3,
      },
    };
    const before = await ports.statePort.read(intent);
    await expect(ports.effectPort.dispatch(scopedEffectRequest(
      "pause_stale_principal_01",
      intent,
      stateDigest(before),
    ))).rejects.toMatchObject({ code: "STALE_PRINCIPAL_GENERATION" });

    const { principalGeneration: _omitted, ...liveRequest } = scopedEffectRequest(
      "pause_live_principal_01",
      intent,
      stateDigest(before),
    );
    await expect(ports.effectPort.dispatch(liveRequest)).resolves.toMatchObject({ status: "committed" });
    expect(database.prepare(`
      SELECT principal_generation FROM operator_effect_custody
       WHERE command_id='pause_live_principal_01'
    `).get()).toEqual({ principal_generation: 3 });
  });

  it("binds a prepared subtree control to its exact descendants and dependency/membership revisions", async () => {
    const { database, ports } = fixture();
    const intent = {
      kind: "control" as const,
      action: "pause" as const,
      target: {
        kind: "subtree" as const,
        projectSessionId: "session_01" as never,
        coordinationRunId: "run_01" as never,
        rootTaskId: "task_01" as never,
        expectedRevision: 3,
      },
    };
    const before = await ports.statePort.read(intent);
    expect(before).toMatchObject({
      kind: "control",
      binding: {
        projectSessionId: "session_01",
        membershipRevision: 1,
        runs: [{ runId: "run_01", dependencyRevision: 1 }],
        tasks: [{ taskId: "task_01", revision: 3 }],
      },
    });
    const request = scopedEffectRequest("pause_subtree_race_01", intent, stateDigest(before));
    ports.effectPort.prepare?.(request);
    database.exec(`
      INSERT INTO tasks(
        run_id, task_id, authority_id, objective, base_revision, state,
        owner_agent_id, revision, owner_lease_generation, created_by
      ) VALUES (
        'run_01', 'task_02', 'authority_01', 'Late descendant', 'base_02', 'active',
        'chair_01', 7, 1, 'chair_01'
      );
      INSERT INTO dependency_mutation_guards(
        run_id, project_session_id, target_revision, expected_edge_count, expected_binding_count
      ) VALUES ('run_01', 'session_01', 2, 1, 0);
      UPDATE runs SET dependency_revision=2 WHERE run_id='run_01';
      INSERT INTO task_dependencies(
        run_id, task_id, dependency_task_id, project_session_id, dependency_revision
      ) VALUES ('run_01', 'task_02', 'task_01', 'session_01', 2);
      DELETE FROM dependency_mutation_guards WHERE run_id='run_01';
      UPDATE project_sessions SET membership_revision=2, revision=revision+1
       WHERE project_session_id='session_01';
    `);

    await expect(ports.effectPort.dispatch(request)).resolves.toMatchObject({ status: "rejected", code: "state-changed" });
    expect(database.prepare("SELECT COUNT(*) AS count FROM operator_control_fences").get()).toEqual({ count: 0 });
  });

  it("rejects a prepared task control when its exact provider turn is replaced", async () => {
    let adapterCalls = 0;
    const { database, ports } = fixture({
      capabilities: async () => { adapterCalls += 1; return { actionJournal: true, operations: ["interrupt", "lookup_action"] }; },
      dispatch: async () => { adapterCalls += 1; return {}; },
      lookup: async () => { adapterCalls += 1; return {}; },
    });
    seedProviderAction(database, {
      actionId: "turn_before",
      payloadJson: '{"taskId":"task_01"}',
      status: "accepted",
      historyJson: '["prepared","dispatched","accepted"]',
      effectCount: 1,
      resultJson: '{"turnId":"native_before"}',
    });
    database.exec(`
      INSERT INTO provider_session_turn_leases(
        run_id, adapter_id, agent_id, provider_session_generation, turn_lease_generation,
        action_id, status, created_at, updated_at
      ) VALUES ('run_01', 'fake', 'chair_01', 2, 1, 'turn_before', 'active', ${now}, ${now});
    `);
    const intent = {
      kind: "control" as const,
      action: "pause" as const,
      target: {
        kind: "task" as const,
        projectSessionId: "session_01" as never,
        coordinationRunId: "run_01" as never,
        taskId: "task_01" as never,
        expectedRevision: 3,
      },
    };
    const before = await ports.statePort.read(intent);
    expect(before).toMatchObject({ binding: { turns: [{ sourceActionId: "turn_before", turnId: "native_before" }] } });
    const request = scopedEffectRequest("pause_turn_race_01", intent, stateDigest(before));
    ports.effectPort.prepare?.(request);
    database.prepare("UPDATE provider_session_turn_leases SET status='released' WHERE action_id='turn_before'").run();
    seedProviderAction(database, {
      actionId: "turn_after",
      turnLeaseGeneration: 2,
      payloadJson: '{"taskId":"task_01"}',
      status: "accepted",
      historyJson: '["prepared","dispatched","accepted"]',
      effectCount: 1,
      resultJson: '{"turnId":"native_after"}',
    });
    database.exec(`
      INSERT INTO provider_session_turn_leases(
        run_id, adapter_id, agent_id, provider_session_generation, turn_lease_generation,
        action_id, status, created_at, updated_at
      ) VALUES ('run_01', 'fake', 'chair_01', 2, 2, 'turn_after', 'active', ${now}, ${now});
    `);

    await expect(ports.effectPort.dispatch(request)).resolves.toMatchObject({ status: "rejected", code: "state-changed" });
    expect(adapterCalls).toBe(0);
  });

  it("proves a prepared control had no external effect instead of replaying it after restart", async () => {
    let adapterCalls = 0;
    const { database, ports } = fixture({
      capabilities: async () => { adapterCalls += 1; return {}; },
      dispatch: async () => { adapterCalls += 1; return {}; },
      lookup: async () => { adapterCalls += 1; return {}; },
    });
    const intent = {
      kind: "control" as const,
      action: "pause" as const,
      target: {
        kind: "task" as const,
        projectSessionId: "session_01" as never,
        coordinationRunId: "run_01" as never,
        taskId: "task_01" as never,
        expectedRevision: 3,
      },
    };
    const before = await ports.statePort.read(intent);
    const request = scopedEffectRequest("pause_prepared_crash_01", intent, stateDigest(before));
    ports.effectPort.prepare?.(request);

    await expect(ports.effectPort.dispatch({ ...request, principalGeneration: 2 }))
      .rejects.toMatchObject({ code: "DEDUPE_CONFLICT" });

    await expect(ports.effectPort.observe({ ...request, attemptGeneration: 2, effectRef: null }))
      .resolves.toMatchObject({ status: "rejected", code: "state-changed" });
    expect(adapterCalls).toBe(0);
    expect(database.prepare(`
      SELECT state FROM operator_effect_custody WHERE command_id='pause_prepared_crash_01'
    `).get()).toEqual({ state: "no-effect" });
  });

  it("admits work only for explicit active visibility states", () => {
    for (const state of [
      "draft", "awaiting_launch", "launching", "launch_failed", "launch_ambiguous",
      "reconciling", "recovery_required", "quarantined",
    ]) {
      const { database } = fixture();
      database.prepare("UPDATE project_sessions SET state=?, revision=revision+1 WHERE project_session_id='session_01'")
        .run(state);
      database.prepare("UPDATE runs SET lifecycle_state=?, revision=revision+1 WHERE run_id='run_01'").run(state);
      expect(() => assertRunAcceptingWork(database, "run_01"), state).toThrow(/not accepting/u);
    }
    for (const state of ["active", "visibility_degraded"]) {
      const { database } = fixture();
      database.prepare("UPDATE project_sessions SET state=?, revision=revision+1 WHERE project_session_id='session_01'")
        .run(state);
      database.prepare("UPDATE runs SET lifecycle_state=?, revision=revision+1 WHERE run_id='run_01'").run(state);
      expect(() => assertRunAcceptingWork(database, "run_01"), state).not.toThrow();
    }
  });

  it("binds subtree, run, and session targets to their authoritative revision families", async () => {
    const { database, ports } = fixture();
    database.exec(`
      INSERT INTO tasks(
        run_id, task_id, authority_id, objective, base_revision, state,
        owner_agent_id, revision, owner_lease_generation, created_by
      ) VALUES (
        'run_01', 'task_02', 'authority_01', 'Dependent child', 'base_02', 'active',
        'chair_01', 7, 1, 'chair_01'
      );
      INSERT INTO dependency_mutation_guards(
        run_id, project_session_id, target_revision, expected_edge_count, expected_binding_count
      ) VALUES ('run_01', 'session_01', 2, 1, 0);
      UPDATE runs SET dependency_revision=2 WHERE run_id='run_01';
      INSERT INTO task_dependencies(
        run_id, task_id, dependency_task_id, project_session_id, dependency_revision
      ) VALUES ('run_01', 'task_02', 'task_01', 'session_01', 2);
      DELETE FROM dependency_mutation_guards WHERE run_id='run_01';
    `);
    const subtree = {
      kind: "control" as const,
      action: "pause" as const,
      target: {
        kind: "subtree" as const,
        projectSessionId: "session_01" as never,
        coordinationRunId: "run_01" as never,
        rootTaskId: "task_01" as never,
        expectedRevision: 3,
      },
    };
    await expect(ports.effectPort.dispatch({
      commandId: "pause_subtree_01",
      intent: subtree,
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    })).resolves.toMatchObject({ status: "committed" });
    expect(database.prepare(`
      SELECT task_id, scope_kind FROM operator_control_fences WHERE state='paused' ORDER BY task_id
    `).all()).toEqual([
      { task_id: "task_01", scope_kind: "subtree" },
      { task_id: "task_02", scope_kind: "subtree" },
    ]);
    await expect(ports.statePort.read({
      kind: "control",
      action: "resume",
      target: {
        kind: "run",
        projectSessionId: "session_01" as never,
        coordinationRunId: "run_01" as never,
        expectedRevision: 4,
      },
    })).resolves.toMatchObject({ kind: "control", revision: 4, lifecycleState: "paused" });
    await expect(ports.statePort.read({
      kind: "control",
      action: "resume",
      target: {
        kind: "session",
        projectSessionId: "session_01" as never,
        expectedRevision: 2,
        expectedGeneration: 1,
      },
    })).resolves.toMatchObject({
      kind: "control",
      revision: 2,
      lifecycleState: "paused",
    });

    await expect(ports.effectPort.dispatch({
      commandId: "resume_subtree_01",
      intent: { ...subtree, action: "resume" },
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    })).resolves.toMatchObject({ status: "committed" });
    const runPause = {
      kind: "control" as const,
      action: "pause" as const,
      target: {
        kind: "run" as const,
        projectSessionId: "session_01" as never,
        coordinationRunId: "run_01" as never,
        expectedRevision: 4,
      },
    };
    await expect(ports.effectPort.dispatch({
      commandId: "pause_run_01",
      intent: runPause,
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    })).resolves.toMatchObject({ status: "committed" });
    expect(() => assertRunAcceptingWork(database, "run_01")).toThrow(/paused/u);
    await expect(ports.effectPort.dispatch({
      commandId: "resume_run_01",
      intent: { ...runPause, action: "resume" },
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    })).resolves.toMatchObject({ status: "committed" });
    expect(() => assertRunAcceptingWork(database, "run_01")).not.toThrow();
  });

  it("does not overwrite a delivery freeze owned by another lifecycle", async () => {
    const { database, ports } = fixture();
    database.exec(`
      INSERT INTO delivery_freezes(run_id, agent_id, reason, created_at)
      VALUES ('run_01', 'chair_01', 'context-reconciliation', ${now});
      UPDATE agents SET lifecycle='suspended' WHERE run_id='run_01' AND agent_id='chair_01';
    `);
    await expect(ports.effectPort.dispatch({
      commandId: "pause_run_foreign_freeze_01",
      intent: {
        kind: "control",
        action: "pause",
        target: {
          kind: "run",
          projectSessionId: "session_01" as never,
          coordinationRunId: "run_01" as never,
          expectedRevision: 4,
        },
      },
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    })).rejects.toMatchObject({ code: "CONFLICT" });
    expect(database.prepare("SELECT reason FROM delivery_freezes WHERE run_id='run_01' AND agent_id='chair_01'").get())
      .toEqual({ reason: "context-reconciliation" });
    expect(database.prepare("SELECT COUNT(*) AS count FROM operator_control_fences").get()).toEqual({ count: 0 });
  });

  it("persists a project drain receipt only after obligations settle and requires it for stop", async () => {
    const { database, ports } = fixture();
    database.prepare("UPDATE tasks SET state='complete', revision=revision+1 WHERE run_id='run_01' AND task_id='task_01'").run();
    database.exec(`
      INSERT INTO run_chair_leases(
        project_session_id, run_id, lease_id, holder_agent_id, generation, status, updated_at
      ) VALUES ('session_01', 'run_01', 'chair:run_01:1', 'chair_01', 1, 'active', ${now});
      INSERT INTO project_session_memberships(
        project_session_id, coordination_run_id, member_kind, member_id,
        required, state, revision, created_at, updated_at
      ) VALUES
        ('session_01', 'run_01', 'coordination-run', 'run_01', 1, 'active', 1, ${now}, ${now}),
        ('session_01', 'run_01', 'lease', 'chair:run_01:1', 1, 'active', 1, ${now}, ${now}),
        ('session_01', 'run_01', 'lease', 'lease_work_01', 1, 'active', 1, ${now}, ${now});
      INSERT INTO leases(lease_id, run_id, kind, holder_agent_id, generation, status, expires_at, updated_at)
      VALUES ('lease_work_01', 'run_01', 'write', 'chair_01', 1, 'active', ${now + 60_000}, ${now});
    `);
    const beforeDrain = database.prepare("SELECT revision FROM daemon_global_state WHERE singleton=1").get() as { revision: number };
    const drain = {
      kind: "project-session-drain" as const,
      projectSessionId: "session_01" as never,
      expectedSessionRevision: 2,
      expectedSessionGeneration: 1,
      expectedGlobalStateRevision: beforeDrain.revision,
    };

    await expect(ports.statePort.read(drain)).resolves.toMatchObject({
      kind: "project-session-lifecycle",
      revision: 2,
      sessionGeneration: 1,
      globalStateRevision: beforeDrain.revision,
      lifecycleState: "active",
      drainReceiptRef: null,
    });
    const request = {
      commandId: "project_drain_01",
      intent: drain,
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    };
    await expect(ports.effectPort.dispatch(request)).resolves.toEqual({ status: "pending", phase: "accepted" });
    expect(database.prepare("SELECT COUNT(*) AS count FROM operator_lifecycle_receipts").get()).toEqual({ count: 0 });
    database.exec(`
      UPDATE leases SET status='released', updated_at=${now + 1} WHERE lease_id='lease_work_01';
      UPDATE project_session_memberships
         SET state='reconciled', revision=revision+1, updated_at=${now + 1}
       WHERE project_session_id='session_01' AND coordination_run_id='run_01'
         AND member_kind='lease' AND member_id='lease_work_01';
    `);
    const drained = await ports.effectPort.observe({ ...request, attemptGeneration: 2, effectRef: null });
    expect(drained).toMatchObject({
      status: "committed",
      afterState: { lifecycleState: "quiescing", obligationsSettled: true },
    });
    if (drained.status !== "committed" || drained.effectRef === undefined) throw new Error("expected drain receipt");
    expect(database.prepare("SELECT state, revision FROM project_sessions WHERE project_session_id='session_01'").get())
      .toEqual({ state: "quiescing", revision: 3 });
    expect(database.prepare("SELECT lifecycle_state, revision FROM runs WHERE run_id='run_01'").get())
      .toEqual({ lifecycle_state: "quiescing", revision: 5 });
    expect(database.prepare("SELECT kind, relative_path, sha256 FROM operator_lifecycle_receipts").get())
      .toEqual({
        kind: "project-session-drain",
        relative_path: drained.effectRef.path,
        sha256: drained.effectRef.digest,
      });

    const afterDrain = database.prepare("SELECT revision FROM daemon_global_state WHERE singleton=1").get() as { revision: number };
    const stop = {
      kind: "project-session-stop" as const,
      projectSessionId: "session_01" as never,
      expectedSessionRevision: 3,
      expectedSessionGeneration: 1,
      expectedGlobalStateRevision: afterDrain.revision,
      drainReceiptRef: drained.effectRef,
    };
    await expect(ports.effectPort.dispatch({
      commandId: "project_stop_bad_receipt",
      intent: {
        ...stop,
        drainReceiptRef: { path: drained.effectRef.path, digest: `sha256:${"f".repeat(64)}` as never },
      },
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    })).rejects.toMatchObject({ code: "NOT_FOUND" });
    expect(database.prepare("SELECT state FROM project_sessions WHERE project_session_id='session_01'").get())
      .toEqual({ state: "quiescing" });
    const stopRequest = {
      commandId: "project_stop_01",
      intent: stop,
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    };
    const crashingStop = createProductionOperatorActionPorts({
      database,
      clock: () => now,
      providerActionAdmission: new ProviderActionAdmissionCoordinator({ database, clock: () => now }),
      adapter: {
        capabilities: async () => { throw new Error("project stop must not inspect an adapter"); },
        dispatch: async () => { throw new Error("project stop must not dispatch an adapter"); },
        lookup: async () => { throw new Error("project stop must not look up an adapter"); },
      },
      fault: (label) => {
        if (label === "operator-effect:after-owned-dispatch") throw new Error("crash after project stop");
      },
    });
    await expect(crashingStop.effectPort.dispatch(stopRequest)).rejects.toThrow("crash after project stop");
    expect(database.prepare("SELECT state FROM operator_effect_custody WHERE command_id='project_stop_01'").get())
      .toEqual({ state: "terminal" });
    await expect(ports.effectPort.observe({ ...stopRequest, attemptGeneration: 2, effectRef: null }))
      .resolves.toMatchObject({ status: "committed", afterState: { lifecycleState: "cancelled" } });
    expect(database.prepare("SELECT state, revision FROM project_sessions WHERE project_session_id='session_01'").get())
      .toEqual({ state: "cancelled", revision: 4 });
    expect(database.prepare("SELECT status FROM run_chair_leases WHERE lease_id='chair:run_01:1'").get())
      .toEqual({ status: "revoked" });
    expect(database.prepare(`
      SELECT member_kind, member_id, state, abandoned_reason
        FROM project_session_memberships ORDER BY member_kind, member_id
    `).all()).toEqual([
      {
        member_kind: "coordination-run",
        member_id: "run_01",
        state: "abandoned",
        abandoned_reason: "operator stop project_stop_01",
      },
      {
        member_kind: "lease",
        member_id: "chair:run_01:1",
        state: "abandoned",
        abandoned_reason: "operator stop project_stop_01",
      },
      { member_kind: "lease", member_id: "lease_work_01", state: "reconciled", abandoned_reason: null },
    ]);
  });

  it("resumes a pending project drain after restart without fabricating a receipt while busy", async () => {
    const { database, ports } = fixture();
    const beforeDrain = database.prepare("SELECT revision FROM daemon_global_state WHERE singleton=1").get() as { revision: number };
    const drain = {
      kind: "project-session-drain" as const,
      projectSessionId: "session_01" as never,
      expectedSessionRevision: 2,
      expectedSessionGeneration: 1,
      expectedGlobalStateRevision: beforeDrain.revision,
    };
    const request = {
      commandId: "project_drain_restart_01",
      intent: drain,
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    };

    await expect(ports.effectPort.dispatch(request)).resolves.toEqual({ status: "pending", phase: "accepted" });
    expect(database.prepare("SELECT COUNT(*) AS count FROM operator_lifecycle_receipts").get()).toEqual({ count: 0 });
    database.prepare("UPDATE tasks SET state='complete', revision=revision+1 WHERE run_id='run_01' AND task_id='task_01'").run();
    const restarted = createProductionOperatorActionPorts({
      database,
      clock: () => now + 1,
      providerActionAdmission: new ProviderActionAdmissionCoordinator({ database, clock: () => now + 1 }),
      adapter: {
        capabilities: async () => { throw new Error("project drain has no adapter effect"); },
        dispatch: async () => { throw new Error("project drain has no adapter effect"); },
        lookup: async () => { throw new Error("project drain has no adapter effect"); },
      },
    });
    await expect(restarted.effectPort.observe({ ...request, attemptGeneration: 2, effectRef: null }))
      .resolves.toMatchObject({
        status: "committed",
        afterState: { lifecycleState: "quiescing", obligationsSettled: true },
        effectRef: { path: expect.stringContaining("project-session-drain") },
      });
  });

  it("scopes same-ID lifecycle receipts by operator, project, and authority session", async () => {
    const { database, ports } = fixture();
    database.prepare("UPDATE tasks SET state='complete', revision=revision+1 WHERE run_id='run_01' AND task_id='task_01'").run();
    database.exec(`
      INSERT INTO projects(project_id, canonical_root, trust_record_digest, revision, authority_generation, created_at, updated_at)
      VALUES ('project_02', '/project/two', '${digest}', 1, 1, ${now}, ${now});
      INSERT INTO project_sessions(
        project_session_id, project_id, mode, state, revision, generation, authority_ref,
        budget_ref, launch_packet_path, launch_packet_digest, membership_revision,
        origin_kind, origin_operator_id, created_at, updated_at
      ) VALUES (
        'session_02', 'project_02', 'coordinated', 'active', 2, 1, '${digest}',
        'budget_02', 'launch-02.json', '${digest}', 1, 'operator-launch', 'operator_02', ${now}, ${now}
      );
      INSERT INTO runs(
        run_id, chair_agent_id, workspace_root, project_run_directory, created_at,
        project_session_id, lifecycle_state, revision, chair_generation, chair_lease_id,
        authority_ref, budget_ref, dependency_revision, topology_slot
      ) VALUES (
        'run_02', 'chair_02', '/project/two', '.agent-run/AFAB-002', ${now},
        'session_02', 'active', 4, 1, 'chair:run_02:1', '${digest}', 'budget_02', 1, 1
      );
      INSERT INTO authorities(authority_id, run_id, authority_json, authority_hash, created_at)
      VALUES ('authority_02', 'run_02', '{}', '${"8".repeat(64)}', ${now});
      INSERT INTO agents(run_id, agent_id, authority_id, provider_session_ref, lifecycle)
      VALUES ('run_02', 'chair_02', 'authority_02', 'provider_session_02', 'ready');
      INSERT INTO tasks(
        run_id, task_id, authority_id, objective, base_revision, state,
        owner_agent_id, revision, owner_lease_generation, created_by
      ) VALUES (
        'run_02', 'task_02', 'authority_02', 'Already settled', 'base_02', 'complete',
        'chair_02', 4, 1, 'chair_02'
      );
    `);

    const drain = async (scope: { operatorId: string; projectId: string; projectSessionId: string }) => {
      const global = database.prepare("SELECT revision FROM daemon_global_state WHERE singleton=1").get() as { revision: number };
      const intent = {
        kind: "project-session-drain" as const,
        projectSessionId: scope.projectSessionId as never,
        expectedSessionRevision: 2,
        expectedSessionGeneration: 1,
        expectedGlobalStateRevision: global.revision,
      };
      const before = await ports.statePort.read(intent);
      const request = {
        ...scopedEffectRequest("same_lifecycle_command", intent, stateDigest(before)),
        ...scope,
      };
      ports.effectPort.prepare?.(request);
      const outcome = await ports.effectPort.dispatch(request);
      if (outcome.status !== "committed" || outcome.effectRef === undefined) throw new Error("expected scoped drain receipt");
      return outcome.effectRef;
    };
    const first = await drain({ operatorId: "operator_01", projectId: "project_01", projectSessionId: "session_01" });
    const second = await drain({ operatorId: "operator_02", projectId: "project_02", projectSessionId: "session_02" });

    expect(first).not.toEqual(second);
    expect(database.prepare(`
      SELECT operator_id, project_id, authority_session_id, command_id
        FROM operator_lifecycle_receipts ORDER BY operator_id
    `).all()).toEqual([
      {
        operator_id: "operator_01",
        project_id: "project_01",
        authority_session_id: "session_01",
        command_id: "same_lifecycle_command",
      },
      {
        operator_id: "operator_02",
        project_id: "project_02",
        authority_session_id: "session_02",
        command_id: "same_lifecycle_command",
      },
    ]);
  });

  it("fences daemon drain and stop by the exact epoch, global revision, and receipt", async () => {
    const { database } = fixture();
    let stopRequests = 0;
    database.exec(`
      UPDATE tasks SET state='complete', revision=revision+1 WHERE run_id='run_01' AND task_id='task_01';
      UPDATE runs SET lifecycle_state='cancelled', revision=revision+1 WHERE run_id='run_01';
      UPDATE project_sessions
         SET state='cancelled', terminal_path_json='{"kind":"cancelled","reason":"fixture settled"}',
             revision=revision+1
       WHERE project_session_id='session_01';
      INSERT INTO daemon_runtime_epochs(
        instance_generation, instance_id, state, observed_global_revision,
        started_at, heartbeat_at, stopped_at
      ) VALUES (7, 'daemon_07', 'running', NULL, ${now}, ${now}, NULL);
    `);
    const ports = createProductionOperatorActionPorts({
      database,
      clock: () => now,
      providerActionAdmission: new ProviderActionAdmissionCoordinator({ database, clock: () => now }),
      adapter: {
        capabilities: async () => { throw new Error("daemon lifecycle has no adapter effect"); },
        dispatch: async () => { throw new Error("daemon lifecycle has no adapter effect"); },
        lookup: async () => { throw new Error("daemon lifecycle has no adapter effect"); },
      },
      daemonStop: {
        request: async ({ token }) => {
          stopRequests += 1;
          const updated = database.prepare(`
            UPDATE daemon_runtime_epochs SET state='stopped', stopped_at=?, heartbeat_at=?
             WHERE instance_generation=? AND state='quiescing' AND observed_global_revision=?
          `).run(now, now, token.daemonInstanceGeneration, token.observedGlobalStateRevision);
          return updated.changes === 1 ? "stopped" : "busy";
        },
      },
    });
    const before = database.prepare("SELECT revision FROM daemon_global_state WHERE singleton=1").get() as { revision: number };
    const drain = {
      kind: "daemon-drain" as const,
      expectedDaemonGeneration: 7,
      expectedGlobalStateRevision: before.revision,
    };
    const drainBefore = await ports.statePort.read(drain);
    const drainRequest = scopedEffectRequest("daemon_drain_01", drain, stateDigest(drainBefore));
    ports.effectPort.prepare?.(drainRequest);
    const drained = await ports.effectPort.dispatch(drainRequest);
    expect(drained).toMatchObject({ status: "committed", afterState: { lifecycleState: "quiescing" } });
    if (drained.status !== "committed" || drained.effectRef === undefined) throw new Error("expected daemon drain receipt");
    const afterDrain = database.prepare("SELECT revision FROM daemon_global_state WHERE singleton=1").get() as { revision: number };
    const stopIntent = {
      kind: "daemon-stop" as const,
      expectedDaemonGeneration: 7,
      expectedGlobalStateRevision: afterDrain.revision,
      drainReceiptRef: drained.effectRef,
    };
    const stopBefore = await ports.statePort.read(stopIntent);
    const stopRequest = scopedEffectRequest("daemon_stop_01", stopIntent, stateDigest(stopBefore));
    ports.effectPort.prepare?.(stopRequest);
    const competing = scopedEffectRequest("daemon_stop_competing", stopIntent, stateDigest(stopBefore));
    expect(() => ports.effectPort.prepare?.(competing)).toThrow(/another operator command owns/u);
    await expect(ports.effectPort.observe({ ...stopRequest, attemptGeneration: 2, effectRef: null }))
      .resolves.toMatchObject({ status: "rejected", code: "state-changed" });
    ports.effectPort.prepare?.(competing);
    await expect(ports.effectPort.dispatch(competing)).resolves.toMatchObject({
      status: "committed",
      afterState: { lifecycleState: "stopped" },
    });
    await expect(ports.effectPort.dispatch(competing)).resolves.toMatchObject({
      status: "committed",
      afterState: { lifecycleState: "stopped" },
    });
    expect(stopRequests).toBe(1);
    expect(database.prepare(`
      SELECT operator_id, project_id, project_session_id, principal_generation,
             command_id, operation, state,
             result_correlation_digest
        FROM operator_daemon_stop_custody WHERE command_id='daemon_stop_competing'
    `).get()).toMatchObject({
      operator_id: "operator_01",
      project_id: "project_01",
      project_session_id: "session_01",
      principal_generation: 1,
      command_id: "daemon_stop_competing",
      operation: "daemon-stop",
      state: "stopped",
      result_correlation_digest: expect.stringMatching(/^sha256:/u),
    });
    expect(database.prepare(`
      SELECT state FROM operator_daemon_stop_custody WHERE command_id='daemon_stop_01'
    `).get()).toEqual({ state: "no-effect" });
    expect(database.prepare("SELECT state, stopped_at FROM daemon_runtime_epochs WHERE instance_generation=7").get())
      .toEqual({ state: "stopped", stopped_at: now });
  });

  it("keeps a busy daemon quiescing without a stop receipt and rejects new work", async () => {
    const { database, ports } = fixture();
    database.exec(`
      INSERT INTO daemon_runtime_epochs(
        instance_generation, instance_id, state, observed_global_revision,
        started_at, heartbeat_at, stopped_at
      ) VALUES (7, 'daemon_busy_07', 'running', NULL, ${now}, ${now}, NULL);
    `);
    const before = database.prepare("SELECT revision FROM daemon_global_state WHERE singleton=1").get() as { revision: number };
    await expect(ports.effectPort.dispatch({
      commandId: "daemon_drain_busy_01",
      intent: {
        kind: "daemon-drain",
        expectedDaemonGeneration: 7,
        expectedGlobalStateRevision: before.revision,
      },
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    })).resolves.toEqual({ status: "pending", phase: "observing" });
    expect(database.prepare("SELECT COUNT(*) AS count FROM operator_lifecycle_receipts").get()).toEqual({ count: 0 });
    expect(() => assertRunAcceptingWork(database, "run_01")).toThrow(/daemon is not accepting/u);
    await expect(ports.effectPort.dispatch({
      commandId: "daemon_stop_without_receipt",
      intent: {
        kind: "daemon-stop",
        expectedDaemonGeneration: 7,
        expectedGlobalStateRevision: before.revision,
        drainReceiptRef: {
          path: ".agent-fabric/lifecycle-receipts/missing.json" as never,
          digest: digest as never,
        },
      },
      intentDigest: digest as never,
      beforeStateDigest: digest as never,
      attemptGeneration: 1,
    })).rejects.toMatchObject({ code: "NOT_FOUND" });
  });
});
