import { rm } from "node:fs/promises";

import Database from "better-sqlite3";
import { afterEach, describe, expect, it } from "vitest";

import { openFabric } from "../../../src/index.ts";
import { LifecycleRotationRepository } from "../../../src/lifecycle/rotation-repository.ts";
import { sha256 } from "../../../src/persistence/row-codec.ts";
import { TestLifecycleReceiptAuthority } from "../../support/lifecycle-receipt-authority-fake.ts";
import {
  createLifecycleFixture,
  reopenLifecycleFabric,
  type LifecycleFixture,
} from "../../support/lifecycle-testkit.ts";
import { admitProviderActionFixture } from "../../support/provider-action-fixture.ts";
import { createStage1Fixture } from "../../support/stage1-fixture.ts";

const fixtures: LifecycleFixture[] = [];

afterEach(async () => {
  await Promise.all(fixtures.splice(0).map(async (fixture) => {
    await fixture.fabric.close();
    await rm(fixture.directory, { recursive: true, force: true });
  }));
});

describe("lifecycle restart recovery", () => {
  it("binds an unmanaged retained session to a generation-loss source before exposing it", async () => {
    const fixture = await createLifecycleFixture({ retainedAgents: true });
    fixtures.push(fixture);
    await fixture.fabric.close();
    const restarted = await reopenLifecycleFabric(fixture, { providerStatus: "unmanaged" });
    fixture.fabric = restarted;

    await expect(restarted.recoverStartupState()).resolves.toMatchObject({ sessionsDegraded: 2 });
    const chair = restarted.connect(fixture.capabilities.chair);
    await expect(chair.getAgentLifecycle({ agentId: "leader" })).resolves.toMatchObject({
      lifecycle: "suspended",
      contextState: "context-unreconciled",
      currentSource: {
        sourceKind: "generation-loss",
        state: "open",
        disposition: null,
      },
    });
  });

  it.each([
    ["matching", "no-effect"],
    ["crossed", "superseded"],
  ] as const)("reads the true-chair source on restart and finalizes a %s source as %s", async (sourceMode, disposition) => {
    const fixture = await createStage1Fixture();
    await fixture.fabric.close();
    const database = new Database(fixture.databasePath);
    database.pragma("foreign_keys = ON");
    const authority = new TestLifecycleReceiptAuthority();
    try {
      const identity = database.prepare(`
        SELECT run.project_session_id,run.revision AS run_generation,
               run.chair_generation,session.generation AS session_generation,
               capability.token_hash,capability.principal_generation
          FROM runs run
          JOIN project_sessions session ON session.project_session_id=run.project_session_id
          JOIN capabilities capability
            ON capability.run_id=run.run_id AND capability.agent_id=run.chair_agent_id
         WHERE run.run_id='run-stage1' AND capability.revoked_at IS NULL
         ORDER BY capability.principal_generation DESC LIMIT 1
      `).get() as {
        project_session_id: string;
        run_generation: number;
        chair_generation: number;
        session_generation: number;
        token_hash: string;
        principal_generation: number;
      };
      const source = {
        adapterId: "fake-primary",
        actionId: "restart-source-action",
        contractDigest: `sha256:${sha256("restart-source-contract")}`,
        providerSessionRef: "restart-source-session",
        providerGeneration: 1,
        bridgeGeneration: 1,
        bridgeRevision: 1,
      };
      database.prepare(`
        INSERT INTO provider_state(
          run_id,agent_id,provider_session_generation,context_revision,reconciled_checkpoint_sha256
        ) VALUES ('run-stage1','chair',1,0,NULL)
      `).run();
      database.prepare(`
        UPDATE agents SET provider_session_ref=? WHERE run_id='run-stage1' AND agent_id='chair'
      `).run(source.providerSessionRef);
      admitProviderActionFixture(database, {
        runId: "run-stage1",
        adapterId: source.adapterId,
        actionId: source.actionId,
        operation: "launch_chair",
        targetAgentId: "chair",
        providerSessionGeneration: source.providerGeneration,
        identityHash: sha256("restart-source-identity"),
        payloadHash: sha256("restart-source-payload"),
        payloadJson: "{}",
        status: "terminal",
        historyJson: '["prepared","terminal"]',
        executionCount: 1,
        effectCount: 1,
        idempotencyProven: true,
        resultJson: "{}",
        updatedAt: 1,
      });
      if (sourceMode === "crossed") {
        admitProviderActionFixture(database, {
          runId: "run-stage1",
          adapterId: source.adapterId,
          actionId: `${source.actionId}:crossed`,
          operation: "launch_chair",
          targetAgentId: "chair",
          providerSessionGeneration: source.providerGeneration,
          identityHash: sha256("restart-crossed-source-identity"),
          payloadHash: sha256("restart-crossed-source-payload"),
          payloadJson: "{}",
          status: "terminal",
          historyJson: '["prepared","terminal"]',
          executionCount: 1,
          effectCount: 1,
          idempotencyProven: true,
          resultJson: "{}",
          updatedAt: 1,
        });
      }
      database.prepare(`
        INSERT INTO launched_chair_bridge_state(
          project_session_id,coordination_run_id,chair_agent_id,provider_adapter_id,
          provider_action_id,provider_contract_digest,provider_session_ref,
          provider_session_generation,principal_generation,bridge_generation,
          capability_hash,activation_evidence_digest,state,revision,created_at,updated_at
        ) VALUES (?,'run-stage1','chair',?,?,?,?,?,?,?,?,?,'active',1,1,1)
      `).run(
        identity.project_session_id,
        source.adapterId,
        source.actionId,
        source.contractDigest,
        source.providerSessionRef,
        source.providerGeneration,
        identity.principal_generation,
        source.bridgeGeneration,
        identity.token_hash,
        `sha256:${sha256("restart-source-activation")}`,
      );
      admitProviderActionFixture(database, {
        runId: "run-stage1",
        adapterId: "missing-adapter",
        actionId: "restart-rotation-action",
        operation: "spawn",
        targetAgentId: "chair",
        identityHash: sha256("restart-rotation-identity"),
        payloadHash: sha256("restart-rotation-payload"),
        payloadJson: "{}",
        status: "prepared",
        historyJson: '["prepared"]',
        executionCount: 0,
        updatedAt: 10,
      });
      const repository = new LifecycleRotationRepository(database);
      database.transaction(() => {
        repository.createInCurrentTransaction({
          projectSessionId: identity.project_session_id,
          runId: "run-stage1",
          agentId: "chair",
          custodyId: "restart-rotation-custody",
          commandId: "restart-rotation-command",
          admissionDigest: `sha256:${"a".repeat(64)}`,
          actionRef: { adapterId: "missing-adapter", actionId: "restart-rotation-action" },
          bridgeOwnerKind: "chair",
          callerTurnLeaseId: "restart-rotation-turn",
          callerTurnGeneration: 1,
          predecessorTurnSetDigest: `sha256:${"b".repeat(64)}`,
          quarantinedWriteSetDigest: `sha256:${"c".repeat(64)}`,
          deliveryCutWatermark: 0,
          adoptionDeliverySetDigest: `sha256:${"d".repeat(64)}`,
          checkpointRef: "restart-checkpoint.json",
          checkpointDigest: `sha256:${"e".repeat(64)}`,
          taskRevision: 1,
          mailboxRevision: 0,
          childSetDigest: `sha256:${"f".repeat(64)}`,
          openWorkSetDigest: `sha256:${"1".repeat(64)}`,
          sourceProviderSessionRef: source.providerSessionRef,
          sourceCapabilityHash: identity.token_hash,
          sourceCustodyActionId: sourceMode === "matching" ? source.actionId : `${source.actionId}:crossed`,
          sourceAdapterId: source.adapterId,
          sourceAdapterContractDigest: source.contractDigest,
          sourceBridgeRowId: `${identity.project_session_id}:run-stage1`,
          sourceBridgeRevision: source.bridgeRevision,
          sourceProviderGeneration: source.providerGeneration,
          sourcePrincipalGeneration: identity.principal_generation,
          sourceBridgeGeneration: source.bridgeGeneration,
          sourceProjectSessionGeneration: identity.session_generation,
          sourceRunGeneration: identity.run_generation,
          sourceChairLeaseGeneration: identity.chair_generation,
          targetProviderGeneration: source.providerGeneration + 1,
          targetPrincipalGeneration: identity.principal_generation + 1,
          targetBridgeGeneration: source.bridgeGeneration + 1,
          replacementAdapterId: "missing-adapter",
          replacementContractDigest: `sha256:${"3".repeat(64)}`,
          stagedCapabilityHash: `sha256:${"4".repeat(64)}`,
          launchAttestChallengeDigest: `sha256:${"5".repeat(64)}`,
          preconditionDigest: `sha256:${"6".repeat(64)}`,
          createdAt: 11,
        });
        database.prepare(`
          INSERT INTO leases(lease_id,run_id,kind,holder_agent_id,generation,status,expires_at,updated_at)
          VALUES ('restart-custody-write-lease','run-stage1','write','chair',4,'quarantined',999999,11)
        `).run();
        database.prepare(`
          INSERT INTO lifecycle_custody_write_leases(
            run_id,agent_id,custody_id,ordinal,lease_id,lease_generation,source_status,active_owner
          ) VALUES ('run-stage1','chair','restart-rotation-custody',1,
                    'restart-custody-write-lease',4,'active',1)
        `).run();
        database.prepare("UPDATE agents SET lifecycle='suspended' WHERE run_id='run-stage1' AND agent_id='chair'").run();
        database.prepare(`
          INSERT INTO delivery_freezes(run_id,agent_id,reason,created_at)
          VALUES ('run-stage1','chair',?,11)
        `).run(`lifecycle-rotation:${sha256("restart-rotation-custody").slice(0, 32)}`);
      }).immediate();
      if (sourceMode === "crossed") {
        database.prepare(`
          UPDATE provider_actions
             SET status='terminal',history_json='["prepared","terminal"]',
                 execution_count=1,effect_count=1,idempotency_proven=1,result_json='{}'
           WHERE run_id='run-stage1' AND adapter_id='missing-adapter'
             AND action_id='restart-rotation-action'
        `).run();
      }
    } finally {
      database.close();
    }

    const restarted = await openFabric({
      databasePath: fixture.databasePath,
      workspaceRoots: [fixture.directory],
      lifecycleReceiptAuthority: authority,
    });
    try {
      await expect(restarted.recoverStartupState()).resolves.toMatchObject({
        actionsReconciled: 0,
        actionsQuarantined: 0,
      });
      const inspection = new Database(fixture.databasePath, { readonly: true });
      try {
        expect(inspection.prepare(`
          SELECT state,disposition_code,terminal
            FROM lifecycle_rotation_custody_heads
           WHERE custody_id='restart-rotation-custody'
        `).get()).toEqual({ state: "finalized", disposition_code: disposition, terminal: 1 });
        expect(inspection.prepare(`
          SELECT status,execution_count,effect_count,idempotency_proven
            FROM provider_actions
           WHERE run_id='run-stage1' AND adapter_id='missing-adapter'
             AND action_id='restart-rotation-action'
        `).get()).toEqual({
          status: "terminal",
          execution_count: sourceMode === "crossed" ? 1 : 0,
          effect_count: sourceMode === "crossed" ? 1 : 0,
          idempotency_proven: 1,
        });
        expect(inspection.prepare(`
          SELECT ownership.active_owner
            FROM leases lease JOIN lifecycle_custody_write_leases ownership
              ON ownership.lease_id=lease.lease_id
           WHERE lease.lease_id='restart-custody-write-lease'
        `).get()).toEqual({ active_owner: 0 });
      } finally {
        inspection.close();
      }
    } finally {
      await restarted.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });
});
