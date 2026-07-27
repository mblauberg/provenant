import Database from "better-sqlite3";
import { createHash } from "node:crypto";
import { rm } from "node:fs/promises";
import { describe, expect, it } from "vitest";

import { LifecycleRotationRepository } from "../../../src/lifecycle/rotation-repository.ts";
import { LifecycleReceiptRepository } from "../../../src/lifecycle/receipt-repository.ts";
import { sha256 } from "../../../src/project-session/store-support.ts";
import { admitProviderActionFixture } from "../../support/provider-action-fixture.ts";
import { createLifecycleFixture } from "../../support/lifecycle-testkit.ts";
import { createStage1Fixture } from "../../support/stage1-fixture.ts";

function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map((item) => canonical(item)).join(",")}]`;
  if (value !== null && typeof value === "object") {
    return `{${Object.entries(value as Record<string, unknown>)
      .filter(([, item]) => item !== undefined)
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([key, item]) => `${JSON.stringify(key)}:${canonical(item)}`)
      .join(",")}}`;
  }
  return JSON.stringify(value);
}

function lifecycleDigest(domain: string, value: unknown): string {
  return `sha256:${createHash("sha256")
    .update(`agent-fabric.lifecycle.v1\0${domain}\0`)
    .update(canonical(value))
    .digest("hex")}`;
}

function seedActiveChildBridge(database: Database.Database, runId: string): void {
  const leader = database.prepare(`
    SELECT agent.authority_id,capability.token_hash,capability.expires_at,
           capability.principal_generation
      FROM agents agent
      JOIN capabilities capability
        ON capability.run_id=agent.run_id AND capability.agent_id=agent.agent_id
     WHERE agent.run_id=? AND agent.agent_id='leader' AND capability.revoked_at IS NULL
     ORDER BY capability.principal_generation DESC LIMIT 1
  `).get(runId) as {
    authority_id: string;
    token_hash: string;
    expires_at: number;
    principal_generation: number;
  };
  const sourceActionId = "schema-negative-source";
  const contractDigest = `sha256:${sha256("schema-negative-source-contract")}`;
  admitProviderActionFixture(database, {
    runId,
    adapterId: "fake-lifecycle",
    actionId: sourceActionId,
    operation: "attach",
    targetAgentId: "leader",
    providerSessionGeneration: 1,
    identityHash: sha256("schema-negative-source-identity"),
    payloadHash: sha256("schema-negative-source-payload"),
    payloadJson: "{}",
    status: "terminal",
    historyJson: '["prepared","terminal"]',
    executionCount: 1,
    effectCount: 1,
    idempotencyProven: true,
    resultJson: "{}",
    updatedAt: 1,
  });
  database.transaction(() => {
    database.prepare(`
      INSERT INTO provider_agent_custody(
        run_id,action_id,operation,actor_agent_id,target_agent_id,authority_id,
        adapter_id,bridge_contract_digest,bridge_capable,capability_hash,
        capability_expires_at,principal_generation,requested_provider_session_ref,
        intent_digest,created_at
      ) VALUES (?,?,'attach','chair','leader',?,'fake-lifecycle',?,1,?,?,?,
                'schema-negative-source-session',?,1)
    `).run(
      runId,
      sourceActionId,
      leader.authority_id,
      contractDigest,
      leader.token_hash,
      leader.expires_at,
      leader.principal_generation,
      `sha256:${sha256("schema-negative-source-intent")}`,
    );
    database.prepare(`
      INSERT INTO agent_bridge_state(
        run_id,agent_id,adapter_id,action_id,provider_session_ref,
        provider_session_generation,bridge_state,bridge_generation,capability_hash,
        activation_evidence_digest,revision,created_at,updated_at
      ) VALUES (?,'leader','fake-lifecycle',?,'schema-negative-source-session',
                1,'active',1,?,?,1,1,1)
    `).run(
      runId,
      sourceActionId,
      leader.token_hash,
      `sha256:${sha256("schema-negative-source-activation")}`,
    );
  }).immediate();
}

function seedLifecycleScope(database: Database.Database, input: Readonly<{
  projectId: string;
  projectSessionId: string;
  runId: string;
  authorityId: string;
}>): Readonly<{ checkpointDigest: string }> {
  const scope = { schemaVersion: 1, ...input };
  const admissionDigest = lifecycleDigest("admission", scope);
  const scopeDigest = lifecycleDigest("admitted-scope", scope);
  const requestId = lifecycleDigest("scope-admission-outbox", scope);
  const orderedRecordSetDigest = lifecycleDigest("scope-record-set", []);
  const checkpointBody = {
    schemaVersion: 1,
    authorityId: input.authorityId,
    projectSessionId: input.projectSessionId,
    runId: input.runId,
    receiptCountDec: "0",
    headAuthoritySequenceDec: "0",
    headReceiptDigest: null,
    orderedRecordSetDigest,
  };
  const checkpointDigest = lifecycleDigest("scope-checkpoint", checkpointBody);
  const namespaceMember = {
    projectSessionId: input.projectSessionId,
    runId: input.runId,
    authorityId: input.authorityId,
    scopeCheckpointDigest: checkpointDigest,
    receiptCountDec: "0",
    headReceiptDigest: null,
  };
  const orderedScopeHeadSetDigest = lifecycleDigest("namespace-scope-head-set", [namespaceMember]);
  const namespaceBody = {
    schemaVersion: 1,
    authorityId: input.authorityId,
    projectId: input.projectId,
    scopeCountDec: "1",
    orderedScopeHeadSetDigest,
  };
  const namespaceDigest = lifecycleDigest("namespace-checkpoint", namespaceBody);
  const resolutionBody = {
    schemaVersion: 1,
    admissionRequestId: requestId,
    scopeDigest,
    initialScopeCheckpointDigest: checkpointDigest,
    namespaceCheckpointDigest: namespaceDigest,
  };
  const resolutionDigest = lifecycleDigest("scope-admission-resolution", resolutionBody);
  database.transaction(() => {
    database.prepare(`INSERT INTO lifecycle_receipt_projects VALUES (?,?,?)`).run(
      input.projectId, input.authorityId, 1,
    );
    database.prepare(`INSERT INTO lifecycle_scope_admission_outbox VALUES (?,?,?,?,?,?,?,?,?,?)`).run(
      requestId, input.projectId, input.projectSessionId, input.runId, input.authorityId,
      admissionDigest, 1, canonical(scope), scopeDigest, 1,
    );
    database.prepare(`INSERT INTO lifecycle_admitted_run_scopes VALUES (?,?,?,?,?,?,?,?,?,?)`).run(
      input.projectId, input.projectSessionId, input.runId, input.authorityId,
      admissionDigest, 1, requestId, scopeDigest, checkpointDigest, resolutionDigest,
    );
    database.prepare(`INSERT INTO lifecycle_receipt_scope_checkpoints VALUES (?,?,?,?,?,?,?,?,?,?,?)`).run(
      input.projectSessionId, input.runId, input.authorityId, 0, 0, null,
      orderedRecordSetDigest, canonical(checkpointBody), checkpointDigest, "scope-attestation-0", 1,
    );
    database.prepare(`INSERT INTO lifecycle_receipt_scope_heads VALUES (?,?,?,1)`).run(
      input.projectSessionId, input.runId, checkpointDigest,
    );
    database.prepare(`INSERT INTO lifecycle_receipt_namespace_checkpoints VALUES (?,?,?,?,?,?,?,?)`).run(
      input.projectId, input.authorityId, 1, orderedScopeHeadSetDigest,
      canonical(namespaceBody), namespaceDigest, "namespace-attestation-0", 1,
    );
    database.prepare(`INSERT INTO lifecycle_receipt_namespace_members VALUES (?,?,?,?,?,?,?,?,?)`).run(
      input.projectId, namespaceDigest, 1, input.projectSessionId, input.runId,
      input.authorityId, checkpointDigest, 0, null,
    );
    database.prepare(`INSERT INTO lifecycle_receipt_namespace_heads VALUES (?,?,?,?,?,1)`).run(
      input.projectId, input.authorityId, 1, orderedScopeHeadSetDigest, namespaceDigest,
    );
    database.prepare(`INSERT INTO lifecycle_scope_admission_resolutions VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)`).run(
      requestId, input.projectId, input.projectSessionId, input.runId, input.authorityId,
      admissionDigest, 1, scopeDigest, 0, 0, orderedRecordSetDigest,
      canonical(checkpointBody), checkpointDigest, 1, namespaceDigest,
      canonical(namespaceMember), 1, canonical(resolutionBody), resolutionDigest,
    );
  }).immediate();
  return { checkpointDigest };
}

describe("lifecycle rotation repository", () => {
  it("reserves generations from durable high-water and never reuses a spent target", async () => {
    const fixture = await createStage1Fixture();
    await fixture.fabric.close();
    const database = new Database(fixture.databasePath);
    database.pragma("foreign_keys = ON");
    try {
      const repository = new LifecycleRotationRepository(database);
      const first = database.transaction(() => repository.reserveNextGenerationsInCurrentTransaction({
        runId: "run-stage1",
        agentId: "chair",
        bridgeOwnerKind: "chair",
        sourceProviderGeneration: 1,
        sourcePrincipalGeneration: 1,
        sourceBridgeGeneration: 1,
      }))();
      expect(first).toEqual({
        providerGeneration: 2,
        principalGeneration: 2,
        bridgeGeneration: 2,
      });

      const second = database.transaction(() => repository.reserveNextGenerationsInCurrentTransaction({
        runId: "run-stage1",
        agentId: "chair",
        bridgeOwnerKind: "chair",
        sourceProviderGeneration: 1,
        sourcePrincipalGeneration: 1,
        sourceBridgeGeneration: 1,
      }))();
      expect(second).toEqual({
        providerGeneration: 3,
        principalGeneration: 3,
        bridgeGeneration: 3,
      });
      expect(database.prepare(`
        SELECT provider_generation,principal_generation,revision
          FROM agent_lifecycle_identity_high_water
         WHERE run_id='run-stage1' AND agent_id='chair'
      `).get()).toEqual({ provider_generation: 3, principal_generation: 3, revision: 2 });
      expect(database.prepare(`
        SELECT bridge_generation,revision
          FROM agent_lifecycle_bridge_high_water
         WHERE run_id='run-stage1' AND agent_id='chair' AND bridge_owner_kind='chair'
      `).get()).toEqual({ bridge_generation: 3, revision: 2 });
      expect(database.prepare(`
        SELECT provider_generation,context_revision,revision
          FROM agent_lifecycle_context_high_water
         WHERE run_id='run-stage1' AND agent_id='chair'
         ORDER BY provider_generation
      `).all()).toEqual([
        { provider_generation: 2, context_revision: 0, revision: 1 },
        { provider_generation: 3, context_revision: 0, revision: 1 },
      ]);
    } finally {
      database.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("rejects exhausted generation high-water before writing any reservation", async () => {
    const fixture = await createStage1Fixture();
    await fixture.fabric.close();
    const database = new Database(fixture.databasePath);
    database.pragma("foreign_keys = ON");
    try {
      database.prepare(`
        INSERT INTO agent_lifecycle_identity_high_water VALUES ('run-stage1','chair',?,1,1)
      `).run(Number.MAX_SAFE_INTEGER);
      database.prepare(`
        INSERT INTO agent_lifecycle_bridge_high_water VALUES ('run-stage1','chair','chair',1,1)
      `).run();
      const repository = new LifecycleRotationRepository(database);
      expect(() => database.transaction(() => repository.reserveNextGenerationsInCurrentTransaction({
        runId: "run-stage1",
        agentId: "chair",
        bridgeOwnerKind: "chair",
        sourceProviderGeneration: 1,
        sourcePrincipalGeneration: 1,
        sourceBridgeGeneration: 1,
      }))()).toThrow("provider generation is exhausted");
      expect(database.prepare(`
        SELECT provider_generation,principal_generation,revision
          FROM agent_lifecycle_identity_high_water
         WHERE run_id='run-stage1' AND agent_id='chair'
      `).get()).toEqual({
        provider_generation: Number.MAX_SAFE_INTEGER,
        principal_generation: 1,
        revision: 1,
      });
    } finally {
      database.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("rejects lifecycle high-water values outside their safe integer domains", async () => {
    const fixture = await createStage1Fixture();
    await fixture.fabric.close();
    const database = new Database(fixture.databasePath);
    database.pragma("foreign_keys = ON");
    try {
      const invalidInserts = [
        `INSERT INTO agent_lifecycle_identity_high_water
           VALUES ('run-stage1','unsafe-identity-provider',0,1,1)`,
        `INSERT INTO agent_lifecycle_identity_high_water
           VALUES ('run-stage1','unsafe-identity-principal',1,9007199254740992,1)`,
        `INSERT INTO agent_lifecycle_bridge_high_water
           VALUES ('run-stage1','unsafe-bridge','child',0,1)`,
        `INSERT INTO agent_lifecycle_context_high_water
           VALUES ('run-stage1','unsafe-context-provider',0,0,1)`,
        `INSERT INTO agent_lifecycle_context_high_water
           VALUES ('run-stage1','unsafe-context-revision',1,-1,1)`,
      ];
      for (const sql of invalidInserts) {
        expect(() => database.exec(sql)).toThrow("INVARIANT_agent_lifecycle");
      }

      database.exec(`
        INSERT INTO agent_lifecycle_identity_high_water
          VALUES ('run-stage1','bounded-identity',1,1,1);
        INSERT INTO agent_lifecycle_bridge_high_water
          VALUES ('run-stage1','bounded-bridge','child',1,1);
        INSERT INTO agent_lifecycle_context_high_water
          VALUES ('run-stage1','bounded-context',1,0,1);
      `);
      const invalidUpdates = [
        `UPDATE agent_lifecycle_identity_high_water
            SET provider_generation=9007199254740992,revision=2
          WHERE run_id='run-stage1' AND agent_id='bounded-identity'`,
        `UPDATE agent_lifecycle_identity_high_water
            SET principal_generation=0,provider_generation=2,revision=2
          WHERE run_id='run-stage1' AND agent_id='bounded-identity'`,
        `UPDATE agent_lifecycle_bridge_high_water
            SET bridge_generation=9007199254740992,revision=2
          WHERE run_id='run-stage1' AND agent_id='bounded-bridge'`,
        `UPDATE agent_lifecycle_context_high_water
            SET context_revision=9007199254740992,revision=2
          WHERE run_id='run-stage1' AND agent_id='bounded-context'`,
      ];
      for (const sql of invalidUpdates) {
        expect(() => database.exec(sql)).toThrow("INVARIANT_agent_lifecycle");
      }
    } finally {
      database.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("rejects a child bridge apply whose provider generation differs from the reserved target", async () => {
    const fixture = await createLifecycleFixture();
    await fixture.fabric.close();
    const database = new Database(fixture.databasePath);
    database.pragma("foreign_keys = ON");
    try {
      seedActiveChildBridge(database, fixture.runId);
      const source = database.prepare(`
        SELECT run.project_session_id,run.revision AS run_revision,run.chair_generation,
               session.generation AS session_generation,bridge.adapter_id,bridge.action_id,
               bridge.provider_session_ref,bridge.provider_session_generation,
               bridge.bridge_generation,bridge.capability_hash,bridge.revision AS bridge_revision,
               capability.principal_generation,custody.bridge_contract_digest
          FROM runs run
          JOIN project_sessions session ON session.project_session_id=run.project_session_id
          JOIN agent_bridge_state bridge ON bridge.run_id=run.run_id AND bridge.agent_id='leader'
          JOIN provider_agent_custody custody
            ON custody.run_id=bridge.run_id AND custody.adapter_id=bridge.adapter_id
           AND custody.action_id=bridge.action_id
          JOIN capabilities capability ON capability.token_hash=bridge.capability_hash
         WHERE run.run_id=? AND bridge.bridge_state='active'
      `).get(fixture.runId) as Record<string, string | number>;
      admitProviderActionFixture(database, {
        runId: fixture.runId,
        adapterId: String(source.adapter_id),
        actionId: "schema-negative-replacement",
        operation: "spawn",
        targetAgentId: "leader",
        providerSessionGeneration: Number(source.provider_session_generation) + 1,
        identityHash: sha256("schema-negative-identity"),
        payloadHash: sha256("schema-negative-payload"),
        payloadJson: "{}",
        status: "terminal",
        historyJson: '["prepared","terminal"]',
        executionCount: 1,
        effectCount: 1,
        idempotencyProven: true,
        resultJson: "{}",
        updatedAt: 10,
      });
      const repository = new LifecycleRotationRepository(database);
      database.transaction(() => {
        repository.createInCurrentTransaction({
          projectSessionId: String(source.project_session_id),
          runId: fixture.runId,
          agentId: "leader",
          custodyId: "schema-negative-custody",
          commandId: "schema-negative-command",
          admissionDigest: sha256("schema-negative-admission"),
          actionRef: { adapterId: String(source.adapter_id), actionId: "schema-negative-replacement" },
          bridgeOwnerKind: "child",
          callerTurnLeaseId: "schema-negative-turn",
          callerTurnGeneration: 1,
          predecessorTurnSetDigest: sha256("schema-negative-turns"),
          quarantinedWriteSetDigest: sha256("schema-negative-writes"),
          deliveryCutWatermark: 0,
          adoptionDeliverySetDigest: sha256("schema-negative-deliveries"),
          checkpointRef: "schema-negative-checkpoint",
          checkpointDigest: sha256("schema-negative-checkpoint"),
          taskRevision: 1,
          mailboxRevision: 0,
          childSetDigest: sha256("schema-negative-children"),
          openWorkSetDigest: sha256("schema-negative-open-work"),
          sourceProviderSessionRef: String(source.provider_session_ref),
          sourceCapabilityHash: String(source.capability_hash),
          sourceCustodyActionId: String(source.action_id),
          sourceAdapterId: String(source.adapter_id),
          sourceAdapterContractDigest: String(source.bridge_contract_digest),
          sourceBridgeRowId: `${fixture.runId}:leader`,
          sourceBridgeRevision: Number(source.bridge_revision),
          sourceProviderGeneration: Number(source.provider_session_generation),
          sourcePrincipalGeneration: Number(source.principal_generation),
          sourceBridgeGeneration: Number(source.bridge_generation),
          sourceProjectSessionGeneration: Number(source.session_generation),
          sourceRunGeneration: Number(source.run_revision),
          sourceChairLeaseGeneration: Number(source.chair_generation),
          targetProviderGeneration: Number(source.provider_session_generation) + 1,
          targetPrincipalGeneration: Number(source.principal_generation) + 1,
          targetBridgeGeneration: Number(source.bridge_generation) + 1,
          replacementAdapterId: String(source.adapter_id),
          replacementContractDigest: String(source.bridge_contract_digest),
          stagedCapabilityHash: String(source.capability_hash),
          launchAttestChallengeDigest: sha256("schema-negative-challenge"),
          preconditionDigest: sha256("schema-negative-precondition"),
          createdAt: 11,
        });
        let head = repository.appendInCurrentTransaction({
          runId: fixture.runId, agentId: "leader", custodyId: "schema-negative-custody",
          expectedRevision: 1, state: "prepared", recordedAt: 12,
        });
        head = repository.appendInCurrentTransaction({
          runId: fixture.runId, agentId: "leader", custodyId: "schema-negative-custody",
          expectedRevision: head.revision, state: "dispatched", recordedAt: 13,
        });
        head = repository.appendInCurrentTransaction({
          runId: fixture.runId, agentId: "leader", custodyId: "schema-negative-custody",
          expectedRevision: head.revision, state: "provider-terminal",
          terminalEvidenceDigest: sha256("schema-negative-terminal"), recordedAt: 14,
        });
        repository.appendInCurrentTransaction({
          runId: fixture.runId, agentId: "leader", custodyId: "schema-negative-custody",
          expectedRevision: head.revision, state: "committing",
          terminalEvidenceDigest: sha256("schema-negative-terminal"), recordedAt: 15,
        });
      }).immediate();
      expect(() => database.prepare(`
        UPDATE agent_bridge_state
           SET provider_session_generation=provider_session_generation+2,revision=revision+1
         WHERE run_id=? AND agent_id='leader'
      `).run(fixture.runId)).toThrow("INVARIANT_agent_bridge_lifecycle_rotation_target");
      const targetProviderGeneration = Number(source.provider_session_generation) + 1;
      const targetBridgeGeneration = Number(source.bridge_generation) + 1;
      const targetProviderSessionRef = "schema-negative-target-session";
      const targetActivationDigest = `sha256:${sha256("schema-negative-target-activation")}`;
      const applyTargetBridge = () => database.prepare(`
        UPDATE agent_bridge_state
           SET action_id='schema-negative-replacement',provider_session_ref=?,
               provider_session_generation=?,bridge_generation=?,
               activation_evidence_digest=?,revision=revision+1
         WHERE run_id=? AND agent_id='leader'
      `).run(
        targetProviderSessionRef,
        targetProviderGeneration,
        targetBridgeGeneration,
        targetActivationDigest,
        fixture.runId,
      );
      database.prepare(`
        UPDATE provider_actions SET result_json=?
         WHERE run_id=? AND action_id='schema-negative-replacement'
      `).run(JSON.stringify({
        providerSessionRef: targetProviderSessionRef,
        providerSessionGeneration: targetProviderGeneration + 1,
        bridgeGeneration: targetBridgeGeneration,
        activationEvidenceDigest: targetActivationDigest,
      }), fixture.runId);
      expect(applyTargetBridge).toThrow("INVARIANT_agent_bridge_lifecycle_rotation_target");
      database.prepare(`
        UPDATE provider_actions SET result_json=?
         WHERE run_id=? AND action_id='schema-negative-replacement'
      `).run(JSON.stringify({
        providerSessionRef: targetProviderSessionRef,
        providerSessionGeneration: targetProviderGeneration,
        bridgeGeneration: targetBridgeGeneration + 1,
        activationEvidenceDigest: targetActivationDigest,
      }), fixture.runId);
      expect(applyTargetBridge).toThrow("INVARIANT_agent_bridge_lifecycle_rotation_target");
      database.prepare(`
        UPDATE provider_actions
           SET provider_session_generation=?,result_json=?
         WHERE run_id=? AND action_id='schema-negative-replacement'
      `).run(targetProviderGeneration + 1, JSON.stringify({
        providerSessionRef: targetProviderSessionRef,
        providerSessionGeneration: targetProviderGeneration,
        bridgeGeneration: targetBridgeGeneration,
        activationEvidenceDigest: targetActivationDigest,
      }), fixture.runId);
      expect(applyTargetBridge).toThrow("INVARIANT_agent_bridge_lifecycle_rotation_target");
    } finally {
      database.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });

  it("creates one immutable custody and appends only through the exact head", async () => {
    const fixture = await createStage1Fixture();
    await fixture.fabric.close();
    const database = new Database(fixture.databasePath);
    database.pragma("foreign_keys = ON");
    try {
      const identity = database.prepare(`
        SELECT run.project_session_id, capability.token_hash,
               capability.principal_generation
          FROM runs run
          JOIN capabilities capability
            ON capability.run_id=run.run_id AND capability.agent_id=run.chair_agent_id
         WHERE run.run_id='run-stage1' AND capability.revoked_at IS NULL
         ORDER BY capability.principal_generation DESC LIMIT 1
      `).get() as {
        project_session_id: string;
        token_hash: string;
        principal_generation: number;
      };
      admitProviderActionFixture(database, {
        runId: "run-stage1",
        adapterId: "fake-primary",
        actionId: "rotation-action-01",
        operation: "spawn",
        targetAgentId: "chair",
        identityHash: sha256("rotation-identity"),
        payloadHash: sha256("rotation-payload"),
        payloadJson: "{}",
        status: "prepared",
        historyJson: '["prepared"]',
        executionCount: 0,
        updatedAt: 10,
      });
      const repository = new LifecycleRotationRepository(database);
      const receiptRepository = new LifecycleReceiptRepository(database, repository);
      const createInput = {
        projectSessionId: identity.project_session_id,
        runId: "run-stage1",
        agentId: "chair",
        custodyId: "rotation-custody-01",
        commandId: "rotation-command-01",
        admissionDigest: sha256("admission"),
        actionRef: { adapterId: "fake-primary", actionId: "rotation-action-01" },
        bridgeOwnerKind: "child",
        callerTurnLeaseId: "rotation-turn-01",
        callerTurnGeneration: 1,
        predecessorTurnSetDigest: sha256("predecessor-turns"),
        quarantinedWriteSetDigest: sha256("writes"),
        deliveryCutWatermark: 0,
        adoptionDeliverySetDigest: sha256("deliveries"),
        checkpointRef: "checkpoint-01",
        checkpointDigest: sha256("checkpoint"),
        taskRevision: 1,
        mailboxRevision: 0,
        childSetDigest: sha256("children"),
        openWorkSetDigest: sha256("open-work"),
        sourceProviderSessionRef: "source-session",
        sourceCapabilityHash: identity.token_hash,
        sourceCustodyActionId: "rotation-action-01",
        sourceAdapterId: "fake-primary",
        sourceAdapterContractDigest: sha256("source-contract"),
        sourceBridgeRowId: "source-bridge",
        sourceBridgeRevision: 1,
        sourceProviderGeneration: 1,
        sourcePrincipalGeneration: identity.principal_generation,
        sourceBridgeGeneration: 1,
        sourceProjectSessionGeneration: 1,
        sourceRunGeneration: 1,
        sourceChairLeaseGeneration: 1,
        targetProviderGeneration: 2,
        targetPrincipalGeneration: identity.principal_generation + 1,
        targetBridgeGeneration: 2,
        replacementAdapterId: "fake-primary",
        replacementContractDigest: sha256("replacement-contract"),
        stagedCapabilityHash: sha256("staged-capability"),
        launchAttestChallengeDigest: sha256("challenge"),
        preconditionDigest: sha256("precondition"),
        createdAt: 11,
      } as const;
      const created = database.transaction(() => repository.createInCurrentTransaction(createInput))();
      expect(created).toMatchObject({ revision: 1, state: "awaiting-boundary", terminal: false });
      const createdRow = database.prepare(`
        SELECT semantic_json,semantic_digest,source_ref_digest,journal_json,journal_digest
          FROM lifecycle_rotation_custody_revisions
         WHERE run_id='run-stage1' AND agent_id='chair' AND custody_id='rotation-custody-01'
           AND revision=1
      `).get() as {
        semantic_json: string;
        semantic_digest: string;
        source_ref_digest: string;
        journal_json: string;
        journal_digest: string;
      };
      expect(createdRow.semantic_digest).toBe(
        lifecycleDigest("custody-semantic", JSON.parse(createdRow.semantic_json)),
      );
      expect(createdRow.source_ref_digest).toBe(createdRow.semantic_digest);
      expect(createdRow.journal_digest).toBe(
        lifecycleDigest("custody-journal", JSON.parse(createdRow.journal_json)),
      );

      const prepared = database.transaction(() => repository.appendInCurrentTransaction({
        runId: "run-stage1",
        agentId: "chair",
        custodyId: "rotation-custody-01",
        expectedRevision: 1,
        state: "prepared",
        recordedAt: 12,
      }))();
      expect(prepared).toMatchObject({ revision: 2, state: "prepared", terminal: false });
      const preparedRow = database.prepare(`
        SELECT semantic_json,semantic_digest,source_ref_digest,journal_json,journal_digest
          FROM lifecycle_rotation_custody_revisions
         WHERE run_id='run-stage1' AND agent_id='chair' AND custody_id='rotation-custody-01'
           AND revision=2
      `).get() as typeof createdRow;
      expect(preparedRow.semantic_digest).toBe(
        lifecycleDigest("custody-semantic", JSON.parse(preparedRow.semantic_json)),
      );
      expect(preparedRow.source_ref_digest).toBe(preparedRow.semantic_digest);
      expect(preparedRow.journal_digest).toBe(
        lifecycleDigest("custody-journal", JSON.parse(preparedRow.journal_json)),
      );
      expect(() => database.transaction(() => repository.appendInCurrentTransaction({
        runId: "run-stage1",
        agentId: "chair",
        custodyId: "rotation-custody-01",
        expectedRevision: 1,
        state: "dispatched",
        recordedAt: 13,
      }))()).toThrow("lifecycle custody head changed");
      expect(database.prepare(`
        SELECT revision FROM lifecycle_rotation_custody_revisions
         WHERE run_id='run-stage1' AND agent_id='chair' AND custody_id='rotation-custody-01'
         ORDER BY revision
      `).all()).toEqual([{ revision: 1 }, { revision: 2 }]);

      let head = database.transaction(() => repository.appendInCurrentTransaction({
        runId: "run-stage1", agentId: "chair", custodyId: "rotation-custody-01",
        expectedRevision: 2, state: "dispatched", recordedAt: 14,
      }))();
      head = database.transaction(() => repository.appendInCurrentTransaction({
        runId: "run-stage1", agentId: "chair", custodyId: "rotation-custody-01",
        expectedRevision: head.revision, state: "provider-terminal",
        terminalEvidenceDigest: lifecycleDigest("transition-proof", { terminal: true }), recordedAt: 15,
      }))();
      head = database.transaction(() => repository.appendInCurrentTransaction({
        runId: "run-stage1", agentId: "chair", custodyId: "rotation-custody-01",
        expectedRevision: head.revision, state: "committing",
        terminalEvidenceDigest: lifecycleDigest("transition-proof", { terminal: true }), recordedAt: 16,
      }))();
      const mutationPlan = {
        schemaVersion: 1,
        writes: [],
        writeSetDigest: lifecycleDigest("mutation-plan", { schemaVersion: 1, writes: [] }),
      };
      const preparedTerminal = database.transaction(() => receiptRepository.prepareChildCustodyTerminalInCurrentTransaction({
        runId: "run-stage1",
        agentId: "chair",
        custodyId: "rotation-custody-01",
        expectedRevision: head.revision,
        applyId: "rotation-apply-01",
        transitionProof: { schemaVersion: 1, kind: "provider-terminal", evidence: "verified" },
        mutationPlan,
        recordedAt: 17,
      }))();
      expect(preparedTerminal.subjectDigest).toBe(
        lifecycleDigest("receipt-subject", preparedTerminal.subject),
      );
      expect(preparedTerminal.subjectJson).toBe(canonical(preparedTerminal.subject));
      expect(preparedTerminal.intentDigest).toBe(
        lifecycleDigest("receipt-intent", preparedTerminal.intent),
      );
      expect(preparedTerminal.intentJson).toBe(canonical(preparedTerminal.intent));
      expect(database.prepare("SELECT count(*) AS count FROM lifecycle_receipt_batches").get())
        .toEqual({ count: 1 });
      expect(database.prepare("SELECT count(*) AS count FROM lifecycle_transition_applies").get())
        .toEqual({ count: 0 });

      expect(() => database.transaction(() => receiptRepository.applyAuthorizedChildCustodyTerminalInCurrentTransaction({
        prepared: preparedTerminal,
        expectedRevision: head.revision,
        expectedScopeHead: { checkpointDigest: "missing", revision: 1 },
        receipt: {
          authorityId: fixture.authorities.chair,
          authoritySequence: 1,
          previousReceiptDigest: null,
          receiptDigest: lifecycleDigest("authenticated-receipt", { wrong: true }),
          attestation: "external-attestation",
          verifiedAt: 18,
        },
        scopeCheckpoint: {
          receiptCount: 1,
          headAuthoritySequence: 1,
          headReceiptDigest: lifecycleDigest("authenticated-receipt", { wrong: true }),
          orderedRecordSetDigest: lifecycleDigest("scope-record-set", []),
          checkpointDigest: lifecycleDigest("scope-checkpoint", { wrong: true }),
          attestation: "scope-attestation-1",
          verifiedAt: 18,
        },
        authorizedAt: 18,
        appliedAt: 19,
        localWrites: [],
        revalidateAdoptionWrites: () => undefined,
        performAdoptionWrites: () => undefined,
      }))()).toThrow("externally verified lifecycle authorization is invalid");
      expect(repository.readHead("run-stage1", "chair", "rotation-custody-01"))
        .toMatchObject({ revision: head.revision, state: "committing", terminal: false });

      const project = database.prepare(`
        SELECT session.project_id FROM runs run
        JOIN project_sessions session ON session.project_session_id=run.project_session_id
        WHERE run.run_id='run-stage1'
      `).get() as { project_id: string };
      const initialScope = seedLifecycleScope(database, {
        projectId: project.project_id,
        projectSessionId: identity.project_session_id,
        runId: "run-stage1",
        authorityId: fixture.authorities.chair,
      });
      const receiptBody = {
        schemaVersion: 1,
        kind: "custody-terminal",
        authorityId: fixture.authorities.chair,
        authoritySequence: 1,
        previousReceiptDigest: null,
        intentDigest: preparedTerminal.intentDigest,
        subjectDigest: preparedTerminal.subjectDigest,
      };
      const receiptDigest = lifecycleDigest("authenticated-receipt", receiptBody);
      const laterReceiptDigest = lifecycleDigest("authenticated-receipt", { sequence: 2 });
      const orderedRecordSetDigest = lifecycleDigest("scope-record-set", [[
        "1", receiptDigest, preparedTerminal.intentDigest, "custody-terminal", "chair",
        "custody", "rotation-custody-01", String(preparedTerminal.finalRevision),
      ], [
        "2", laterReceiptDigest, lifecycleDigest("receipt-intent", { later: true }),
        "generation-loss-terminal", "other-agent", "generation-loss", "loss-02", "2",
      ]]);
      const scopeCheckpointBody = {
        schemaVersion: 1,
        authorityId: fixture.authorities.chair,
        projectSessionId: identity.project_session_id,
        runId: "run-stage1",
        receiptCountDec: "2",
        headAuthoritySequenceDec: "2",
        headReceiptDigest: laterReceiptDigest,
        orderedRecordSetDigest,
      };
      const scopeCheckpointDigest = lifecycleDigest("scope-checkpoint", scopeCheckpointBody);
      let callbackObservedApply = true;
      let simulateBusinessCasDrift = true;
      const applyTerminal = () => database.transaction(() => receiptRepository.applyAuthorizedChildCustodyTerminalInCurrentTransaction({
        prepared: preparedTerminal,
        expectedRevision: head.revision,
        expectedScopeHead: { checkpointDigest: initialScope.checkpointDigest, revision: 1 },
        receipt: {
          authorityId: fixture.authorities.chair,
          authoritySequence: 1,
          previousReceiptDigest: null,
          receiptDigest,
          attestation: "external-attestation",
          verifiedAt: 18,
        },
        scopeCheckpoint: {
          receiptCount: 2,
          headAuthoritySequence: 2,
          headReceiptDigest: laterReceiptDigest,
          orderedRecordSetDigest,
          checkpointDigest: scopeCheckpointDigest,
          attestation: "scope-attestation-1",
          verifiedAt: 18,
        },
        authorizedAt: 18,
        appliedAt: 19,
        localWrites: [],
        revalidateAdoptionWrites: () => undefined,
        performAdoptionWrites: () => {
          if (simulateBusinessCasDrift) throw new Error("simulated lifecycle delivery CAS drift");
          callbackObservedApply = (database.prepare(
            "SELECT count(*) FROM lifecycle_transition_applies",
          ).pluck().get() as number) > 0;
        },
      }))();
      expect(applyTerminal).toThrow("simulated lifecycle delivery CAS drift");
      expect(repository.readHead("run-stage1", "chair", "rotation-custody-01"))
        .toMatchObject({ state: "committing", terminal: false });
      expect(database.prepare("SELECT count(*) AS count FROM lifecycle_authority_receipts").get())
        .toEqual({ count: 0 });
      expect(database.prepare("SELECT count(*) AS count FROM lifecycle_transition_applies").get())
        .toEqual({ count: 0 });
      simulateBusinessCasDrift = false;
      const finalized = applyTerminal();
      expect(callbackObservedApply).toBe(false);
      expect(finalized).toMatchObject({
        revision: head.revision + 1, state: "finalized", disposition: "adopted", terminal: true,
      });
      expect(database.prepare(`
        SELECT applied_mutation_plan_digest,local_write_set_digest
          FROM lifecycle_transition_applies WHERE apply_id='rotation-apply-01'
      `).get()).toEqual({
        applied_mutation_plan_digest: mutationPlan.writeSetDigest,
        local_write_set_digest: lifecycleDigest("local-write-set", {
          schemaVersion: 1,
          writes: [
            { relation: "lifecycle_authority_receipts", key: `${preparedTerminal.batchId}:1`, operation: "insert" },
            { relation: "lifecycle_receipt_scope_checkpoints", key: `${identity.project_session_id}:run-stage1:${scopeCheckpointDigest}`, operation: "insert" },
            { relation: "lifecycle_receipt_scope_heads", key: `${identity.project_session_id}:run-stage1`, operation: "update" },
            { relation: "lifecycle_receipt_batch_completions", key: preparedTerminal.batchId, operation: "insert" },
            { relation: "lifecycle_receipt_batch_authorizations", key: preparedTerminal.batchId, operation: "insert" },
            { relation: "lifecycle_rotation_custody_revisions", key: `run-stage1:chair:rotation-custody-01:${preparedTerminal.finalRevision}`, operation: "insert" },
            { relation: "lifecycle_rotation_custody_heads", key: "run-stage1:chair:rotation-custody-01", operation: "update" },
            { relation: "lifecycle_transition_applies", key: "rotation-apply-01", operation: "insert" },
          ].sort((left, right) => canonical(left).localeCompare(canonical(right))),
        }),
      });
      expect(database.prepare(`
        SELECT revision,state,disposition_code FROM lifecycle_rotation_custody_revisions
        WHERE run_id='run-stage1' AND agent_id='chair' AND custody_id='rotation-custody-01'
        ORDER BY revision
      `).all()).toEqual([
        { revision: 1, state: "awaiting-boundary", disposition_code: "none" },
        { revision: 2, state: "prepared", disposition_code: "none" },
        { revision: 3, state: "dispatched", disposition_code: "none" },
        { revision: 4, state: "provider-terminal", disposition_code: "none" },
        { revision: 5, state: "committing", disposition_code: "none" },
        { revision: 6, state: "finalized", disposition_code: "adopted" },
      ]);
      admitProviderActionFixture(database, {
        runId: "run-stage1",
        adapterId: "fake-primary",
        actionId: "rotation-chair-action-02",
        operation: "spawn",
        targetAgentId: "chair",
        identityHash: sha256("rotation-chair-identity-02"),
        payloadHash: sha256("rotation-chair-payload-02"),
        payloadJson: "{}",
        status: "terminal",
        historyJson: '["prepared","terminal"]',
        executionCount: 1,
        effectCount: 1,
        idempotencyProven: true,
        resultJson: "{}",
        updatedAt: 20,
      });
      const chairCustodyInput = {
        ...createInput,
        custodyId: "rotation-chair-custody-02",
        commandId: "rotation-chair-command-02",
        actionRef: { adapterId: "fake-primary", actionId: "rotation-chair-action-02" },
        bridgeOwnerKind: "chair" as const,
        sourceCustodyActionId: "rotation-chair-action-02",
        stagedCapabilityHash: sha256("rotation-chair-staged-capability-02"),
        createdAt: 20,
      };
      let chairHead = database.transaction(() =>
        repository.createInCurrentTransaction(chairCustodyInput))();
      for (const [index, state] of (["prepared", "dispatched", "provider-terminal", "committing"] as const).entries()) {
        chairHead = database.transaction(() => repository.appendInCurrentTransaction({
          runId: "run-stage1",
          agentId: "chair",
          custodyId: chairCustodyInput.custodyId,
          expectedRevision: chairHead.revision,
          state,
          ...(state === "provider-terminal" || state === "committing"
            ? { terminalEvidenceDigest: lifecycleDigest("transition-proof", { chair: true }) }
            : {}),
          recordedAt: 21 + index,
        }))();
      }
      expect(() => database.transaction(() =>
        receiptRepository.prepareChildCustodyTerminalInCurrentTransaction({
          runId: "run-stage1",
          agentId: "chair",
          custodyId: chairCustodyInput.custodyId,
          expectedRevision: chairHead.revision,
          applyId: "rotation-chair-apply-02",
          transitionProof: { schemaVersion: 1, kind: "provider-terminal" },
          mutationPlan,
          recordedAt: 25,
        }))()).toThrow("true-chair lifecycle adoption requires ordinal-two review authority");
      expect(database.prepare("SELECT count(*) AS count FROM lifecycle_receipt_batches").get())
        .toEqual({ count: 1 });
    } finally {
      database.close();
      await rm(fixture.directory, { recursive: true, force: true });
    }
  });
});
