import type { ChairBridgeRecoveryIntent, Sha256Digest } from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import {
  parseChairLaunchProviderResult,
  type ChairLaunchProviderResult,
} from "../adapters/providers/types.js";
import { ProjectFabricCoreError } from "./contracts.js";
import { jsonEvidenceDigest } from "./provider-agent-custody.js";
import {
  canonicalJson,
  integer,
  isRow,
  row,
  text,
  type Row,
} from "../persistence/row-codec.js";
import { assertProviderActionOwner } from "../application/provider-action-owner.js";
import type { ChairRecoveryRetainedBridgeRepository } from "./chair-recovery-retained-bridge-repository.js";
import type {
  ChairRecoveryCommit,
  ChairRecoveryDispatchHandle,
} from "./chair-recovery-custody.js";
import type { RetainedChairBridge } from "./launch-custody.js";

type Digest = Sha256Digest;

function stale(message: string): never {
  throw new ProjectFabricCoreError("STALE_REVISION", message);
}

/**
 * Narrow bound-closure port back into `ChairRecoveryCustodyService` so this settlement module
 * never imports it as a value (which would create an import cycle). Only the startup-recovery
 * takeover path needs to re-enter the service's public reconciliation entry point.
 */
export type ChairRecoveryTakeoverReconcilePort = (
  operatorId: string,
  operatorCommandId: string,
) => Promise<ChairRecoveryCommit>;

export type ChairRecoverySettlementAdapterEffectsPort = Readonly<{
  lookupChairRecovery?(input: Readonly<{ adapterId: string; actionId: string }>): Promise<unknown>;
  hasRetainedChairBridge?(entry: RetainedChairBridge): boolean;
}>;

export type ChairRecoverySettlementOptions = Readonly<{
  database: Database.Database;
  clock: () => number;
  fault: (label: string) => void;
  adapterEffects: ChairRecoverySettlementAdapterEffectsPort;
  repository: ChairRecoveryRetainedBridgeRepository;
}>;

/**
 * Byte-moved from `LaunchCustodyService`'s chair-recovery family (S4d, plan §2): inspection
 * binding, successor binding, active-recovery commit, retained-bridge audit and the
 * startup-recovery reconciliation pass. Depends only on
 * `ChairRecoveryRetainedBridgeRepository` (one-directional) plus a narrow bound closure back
 * into the service for the takeover reconciliation path — never the service class itself, to
 * avoid an import cycle. Preserves: single-transaction authority change on commit, and
 * continue-after-error sibling auditing in `auditRetainedChairBridges`.
 */
export class ChairRecoverySettlement {
  readonly #database: Database.Database;
  readonly #clock: () => number;
  readonly #fault: (label: string) => void;
  readonly #adapterEffects: ChairRecoverySettlementAdapterEffectsPort;
  readonly #repository: ChairRecoveryRetainedBridgeRepository;

  constructor(options: ChairRecoverySettlementOptions) {
    this.#database = options.database;
    this.#clock = options.clock;
    this.#fault = options.fault;
    this.#adapterEffects = options.adapterEffects;
    this.#repository = options.repository;
  }

  chairRecoveryInspectionDigest(intent: ChairBridgeRecoveryIntent): Digest {
    if (isRow(this.#database.prepare(`
      SELECT 1 FROM chair_live_handoff_custody
       WHERE coordination_run_id=? AND state NOT IN ('terminal','no-effect') LIMIT 1
    `).get(intent.coordinationRunId))) {
      throw new ProjectFabricCoreError("CONFLICT", "chair live handoff custody owns this run");
    }
    const loss = row(this.#database.prepare(`
      SELECT * FROM chair_bridge_losses WHERE loss_id=?
    `).get(intent.lossId), "chair bridge loss");
    const bridge = row(this.#database.prepare(`
      SELECT * FROM launched_chair_bridge_state
       WHERE project_session_id=? AND coordination_run_id=?
    `).get(intent.projectSessionId, intent.coordinationRunId), "launched chair bridge");
    const session = row(this.#database.prepare(`
      SELECT project_id, state, revision, generation FROM project_sessions WHERE project_session_id=?
    `).get(intent.projectSessionId), "chair recovery project session");
    const run = row(this.#database.prepare(`
      SELECT lifecycle_state, revision, chair_agent_id, chair_generation FROM runs
       WHERE project_session_id=? AND run_id=?
    `).get(intent.projectSessionId, intent.coordinationRunId), "chair recovery run");
    if (
      text(loss, "project_session_id") !== intent.projectSessionId ||
      text(loss, "coordination_run_id") !== intent.coordinationRunId ||
      text(loss, "recovery_manifest_digest") !== intent.recoveryManifestDigest ||
      integer(loss, "principal_generation") !== intent.expectedPrincipalGeneration ||
      integer(loss, "lost_bridge_generation") !== intent.expectedLostBridgeGeneration ||
      integer(loss, "provider_session_generation") !== intent.expectedProviderSessionGeneration ||
      text(loss, "provider_adapter_id") !== intent.providerAdapterId ||
      text(loss, "provider_contract_digest") !== intent.providerContractDigest ||
      text(bridge, "state") !== "lost" ||
      integer(bridge, "revision") !== intent.expectedBridgeRevision ||
      text(session, "state") !== "recovery_required" ||
      integer(session, "revision") !== intent.expectedSessionRevision ||
      integer(session, "generation") !== intent.expectedSessionGeneration ||
      text(run, "lifecycle_state") !== "recovery_required" ||
      integer(run, "revision") !== intent.expectedRunRevision ||
      integer(run, "chair_generation") !== intent.expectedChairGeneration
    ) throw new ProjectFabricCoreError("STALE_REVISION", "chair recovery binding is stale");
    if (this.#repository.hasValidBridgeRetirement(intent.projectSessionId, intent.coordinationRunId)) {
      throw new ProjectFabricCoreError("CONFLICT", "retired chair bridge cannot enter recovery");
    }
    if (this.#database.prepare(`
      SELECT 1 FROM chair_bridge_loss_resolutions WHERE loss_id=?
    `).get(intent.lossId) !== undefined) {
      throw new ProjectFabricCoreError("CONFLICT", "chair bridge loss is already resolved");
    }
    if (intent.path === "rebind" && this.#database.prepare(`
      SELECT 1 FROM provider_actions WHERE adapter_id=? AND action_id=?
    `).get(intent.providerAdapterId, intent.providerActionId) !== undefined) {
      throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "chair recovery provider action is already used");
    }
    const successor = intent.path === "takeover" ? this.chairRecoverySuccessor(intent) : null;
    return jsonEvidenceDigest({ intent, loss, bridge, session, run, successor });
  }

  chairRecoverySuccessor(
    intent: Extract<ChairBridgeRecoveryIntent, { path: "takeover" }>,
  ): Row {
    const successor = row(this.#database.prepare(`
      SELECT bridge.*, custody.principal_generation
        FROM agent_bridge_state bridge
        JOIN provider_agent_custody custody
          ON custody.run_id=bridge.run_id AND custody.action_id=bridge.action_id
         AND custody.adapter_id=bridge.adapter_id AND custody.target_agent_id=bridge.agent_id
         AND custody.bridge_capable=1
        JOIN provider_actions action
          ON action.adapter_id=bridge.adapter_id AND action.action_id=bridge.action_id
         AND action.run_id=bridge.run_id AND action.target_agent_id=bridge.agent_id
         AND action.status='terminal' AND action.execution_count=1 AND action.effect_count=1
        JOIN agents agent
          ON agent.run_id=bridge.run_id AND agent.agent_id=bridge.agent_id
         AND agent.authority_id=custody.authority_id AND agent.lifecycle='ready'
         AND agent.provider_session_ref=bridge.provider_session_ref
        JOIN authorities authority
          ON authority.authority_id=custody.authority_id AND authority.run_id=bridge.run_id
        JOIN capabilities capability
          ON capability.token_hash=bridge.capability_hash
         AND capability.run_id=bridge.run_id AND capability.agent_id=bridge.agent_id
         AND capability.principal_generation=custody.principal_generation
       WHERE bridge.run_id=? AND bridge.agent_id=? AND bridge.bridge_state='active'
         AND capability.revoked_at IS NULL AND capability.expires_at>?
         AND agent.parent_agent_id=(
           SELECT chair_agent_id FROM chair_bridge_losses WHERE loss_id=?
         )
    `).get(
      intent.coordinationRunId,
      intent.successorAgentId,
      this.#clock(),
      intent.lossId,
    ), "chair recovery successor bridge");
    if (
      text(successor, "adapter_id") !== intent.providerAdapterId ||
      integer(successor, "principal_generation") !== intent.expectedSuccessorPrincipalGeneration ||
      integer(successor, "bridge_generation") !== intent.expectedSuccessorBridgeGeneration ||
      integer(successor, "revision") !== intent.expectedSuccessorRevision
    ) throw new ProjectFabricCoreError("STALE_REVISION", "chair recovery successor bridge changed");
    return successor;
  }

  chairRecoveryLookupResult(custody: Row, record: unknown): ChairLaunchProviderResult | undefined {
    if (
      !isRow(record) || record.actionId !== custody.provider_action_id ||
      record.status !== "terminal" || record.operation !== "recover_chair" ||
      record.executionCount !== 1 || record.effectCount !== 1 || !isRow(record.result)
    ) return undefined;
    try {
      const provider = parseChairLaunchProviderResult(record.result, {
        providerAdapterId: text(custody, "provider_adapter_id"),
        providerActionId: text(custody, "provider_action_id"),
        providerContractDigest: text(custody, "provider_contract_digest"),
        challengeDigest: text(custody, "attestation_challenge_digest"),
      });
      return provider.resumeReference === custody.new_provider_session_ref &&
        provider.providerSessionGeneration === custody.new_provider_session_generation
        ? provider
        : undefined;
    } catch {
      return undefined;
    }
  }

  commitActiveChairRecovery(
    handle: ChairRecoveryDispatchHandle & Readonly<{
      intent: Extract<ChairBridgeRecoveryIntent, { path: "rebind" | "takeover" }>;
    }>,
    activationEvidenceDigest: Digest,
  ): ChairRecoveryCommit {
    if (handle.intent.path === "rebind") {
      assertProviderActionOwner(this.#database, {
        runId: handle.intent.coordinationRunId,
        adapterId: handle.intent.providerAdapterId,
        actionId: handle.intent.providerActionId,
      }, "chair_recovery");
    }
    const now = this.#clock();
    return this.#database.transaction((): ChairRecoveryCommit => {
      const custody = row(this.#database.prepare(`
        SELECT * FROM chair_bridge_recovery_custody WHERE recovery_id=?
      `).get(handle.recoveryId), "active chair recovery custody");
      const allowedState = text(custody, "state");
      if (!["dispatched", "accepted", "ambiguous"].includes(allowedState)) {
        stale("active chair recovery custody changed");
      }
      const committing = this.#database.prepare(`
        UPDATE chair_bridge_recovery_custody
           SET state='committing', new_activation_evidence_digest=?, revision=revision+1, updated_at=?
         WHERE recovery_id=? AND state=?
      `).run(activationEvidenceDigest, now, handle.recoveryId, allowedState);
      if (committing.changes !== 1) stale("active chair recovery custody changed before commit");
      this.#fault(`chair-recovery:${handle.intent.path}:committing`);
      const updatedBridge = this.#database.prepare(`
        UPDATE launched_chair_bridge_state
           SET chair_agent_id=(SELECT new_chair_agent_id FROM chair_bridge_recovery_custody WHERE recovery_id=?),
               provider_adapter_id=(SELECT provider_adapter_id FROM chair_bridge_recovery_custody WHERE recovery_id=?),
               provider_action_id=(SELECT new_provider_action_id FROM chair_bridge_recovery_custody WHERE recovery_id=?),
               provider_contract_digest=(SELECT provider_contract_digest FROM chair_bridge_recovery_custody WHERE recovery_id=?),
               provider_session_ref=(SELECT new_provider_session_ref FROM chair_bridge_recovery_custody WHERE recovery_id=?),
               provider_session_generation=(SELECT new_provider_session_generation FROM chair_bridge_recovery_custody WHERE recovery_id=?),
               principal_generation=(SELECT new_principal_generation FROM chair_bridge_recovery_custody WHERE recovery_id=?),
               bridge_generation=(SELECT new_bridge_generation FROM chair_bridge_recovery_custody WHERE recovery_id=?),
               capability_hash=(SELECT new_capability_hash FROM chair_bridge_recovery_custody WHERE recovery_id=?),
               activation_evidence_digest=?, state='active', revision=revision+1, updated_at=?
         WHERE project_session_id=? AND coordination_run_id=? AND state='lost' AND revision=?
      `).run(
        handle.recoveryId, handle.recoveryId, handle.recoveryId, handle.recoveryId,
        handle.recoveryId, handle.recoveryId, handle.recoveryId, handle.recoveryId,
        handle.recoveryId, activationEvidenceDigest, now,
        handle.intent.projectSessionId, handle.intent.coordinationRunId,
        handle.intent.expectedBridgeRevision,
      );
      if (updatedBridge.changes !== 1) stale("lost chair bridge changed during recovery");
      const target = row(this.#database.prepare(`
        SELECT new_chair_agent_id, new_provider_session_ref, new_provider_session_generation,
               new_principal_generation, new_bridge_generation
          FROM chair_bridge_recovery_custody WHERE recovery_id=?
      `).get(handle.recoveryId), "chair recovery target");
      const targetAgentId = text(target, "new_chair_agent_id");
      const newChairGeneration = handle.intent.expectedChairGeneration + 1;
      const leaseId = `chair:${handle.intent.coordinationRunId}:${String(newChairGeneration)}:recovery`;
      const predecessor = row(this.#database.prepare(`
        SELECT chair_lease_id FROM runs
         WHERE project_session_id=? AND run_id=? AND chair_generation=?
      `).get(
        handle.intent.projectSessionId,
        handle.intent.coordinationRunId,
        handle.intent.expectedChairGeneration,
      ), "recovered predecessor chair lease");
      const predecessorLeaseId = text(predecessor, "chair_lease_id");
      const revokedLease = this.#database.prepare(`
        UPDATE run_chair_leases SET status='revoked', updated_at=?
         WHERE project_session_id=? AND run_id=? AND lease_id=? AND status='frozen'
      `).run(now, handle.intent.projectSessionId, handle.intent.coordinationRunId, predecessorLeaseId);
      if (revokedLease.changes !== 1) stale("frozen chair lease changed during recovery");
      this.#database.prepare(`
        INSERT INTO run_chair_leases(
          project_session_id, run_id, lease_id, holder_agent_id, generation, status, updated_at
        ) VALUES (?, ?, ?, ?, ?, 'active', ?)
      `).run(
        handle.intent.projectSessionId,
        handle.intent.coordinationRunId,
        leaseId,
        targetAgentId,
        newChairGeneration,
        now,
      );
      const retiredMembership = this.#database.prepare(`
        UPDATE project_session_memberships
           SET state='abandoned', abandoned_reason='chair-bridge-recovery',
               revision=revision+1, updated_at=?
         WHERE project_session_id=? AND coordination_run_id=?
           AND member_kind='lease' AND member_id=? AND required=1 AND state='active'
      `).run(
        now,
        handle.intent.projectSessionId,
        handle.intent.coordinationRunId,
        predecessorLeaseId,
      );
      if (retiredMembership.changes !== 1) stale("predecessor chair membership changed during recovery");
      this.#database.prepare(`
        INSERT INTO project_session_memberships(
          project_session_id, coordination_run_id, member_kind, member_id,
          required, state, revision, abandoned_reason, created_at, updated_at
        ) VALUES (?, ?, 'lease', ?, 1, 'active', 1, NULL, ?, ?)
      `).run(
        handle.intent.projectSessionId,
        handle.intent.coordinationRunId,
        leaseId,
        now,
        now,
      );
      const updatedRun = this.#database.prepare(`
        UPDATE runs
           SET chair_agent_id=?, chair_generation=?, chair_lease_id=?,
               lifecycle_state='active', revision=revision+1
         WHERE run_id=? AND lifecycle_state='recovery_required'
           AND revision=? AND chair_generation=?
      `).run(
        targetAgentId,
        newChairGeneration,
        leaseId,
        handle.intent.coordinationRunId,
        handle.intent.expectedRunRevision,
        handle.intent.expectedChairGeneration,
      );
      if (updatedRun.changes !== 1) stale("run recovery revision changed");
      const updatedSession = this.#database.prepare(`
        UPDATE project_sessions
           SET state='active', generation=generation+1,
               membership_revision=membership_revision+1,
               revision=revision+1, updated_at=?
         WHERE project_session_id=? AND state='recovery_required'
           AND revision=? AND generation=?
      `).run(
        now,
        handle.intent.projectSessionId,
        handle.intent.expectedSessionRevision,
        handle.intent.expectedSessionGeneration,
      );
      if (updatedSession.changes !== 1) stale("project session recovery revision changed");
      const suspendedChair = this.#database.prepare(`
        UPDATE agents SET lifecycle='suspended'
         WHERE run_id=? AND agent_id=(SELECT chair_agent_id FROM chair_bridge_losses WHERE loss_id=?)
      `).run(handle.intent.coordinationRunId, handle.intent.lossId);
      if (suspendedChair.changes !== 1) stale("lost chair identity changed during recovery");
      const activatedChair = this.#database.prepare(`
        UPDATE agents SET lifecycle='ready', provider_session_ref=? WHERE run_id=? AND agent_id=?
      `).run(
        text(target, "new_provider_session_ref"),
        handle.intent.coordinationRunId,
        targetAgentId,
      );
      if (activatedChair.changes !== 1) stale("recovery target identity changed");
      this.#database.prepare(`
        INSERT INTO provider_state(
          run_id, agent_id, provider_session_generation, context_revision, reconciled_checkpoint_sha256
        ) VALUES (?, ?, ?, NULL, NULL)
        ON CONFLICT(run_id, agent_id) DO UPDATE SET
          provider_session_generation=excluded.provider_session_generation,
          context_revision=NULL, reconciled_checkpoint_sha256=NULL
      `).run(
        handle.intent.coordinationRunId,
        targetAgentId,
        integer(target, "new_provider_session_generation"),
      );
      this.#database.prepare(`
        DELETE FROM delivery_freezes
         WHERE run_id=? AND agent_id=(SELECT chair_agent_id FROM chair_bridge_losses WHERE loss_id=?)
      `).run(handle.intent.coordinationRunId, handle.intent.lossId);
      if (handle.intent.path === "takeover") {
        const clearedSuccessor = this.#database.prepare(`
          UPDATE agent_bridge_state
             SET provider_session_ref=NULL, provider_session_generation=NULL,
                 bridge_state='none', capability_hash=NULL, activation_evidence_digest=NULL,
                 revision=revision+1, updated_at=?
           WHERE run_id=? AND agent_id=? AND bridge_state='active' AND revision=?
        `).run(
          now,
          handle.intent.coordinationRunId,
          handle.intent.successorAgentId,
          handle.intent.expectedSuccessorRevision,
        );
        if (clearedSuccessor.changes !== 1) stale("takeover successor bridge changed during recovery");
      }
      const resolution = {
        schemaVersion: 1,
        recoveryId: handle.recoveryId,
        lossId: handle.intent.lossId,
        path: handle.intent.path,
        successorAgentId: targetAgentId,
        newPrincipalGeneration: integer(target, "new_principal_generation"),
        newBridgeGeneration: integer(target, "new_bridge_generation"),
        activationEvidenceDigest,
      };
      const evidenceDigest = jsonEvidenceDigest(resolution);
      this.#database.prepare(`
        INSERT INTO chair_bridge_loss_resolutions(
          loss_id, recovery_id, path, successor_agent_id,
          new_principal_generation, new_bridge_generation, evidence_digest, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
      `).run(
        handle.intent.lossId,
        handle.recoveryId,
        handle.intent.path,
        targetAgentId,
        integer(target, "new_principal_generation"),
        integer(target, "new_bridge_generation"),
        evidenceDigest,
        now,
      );
      if (handle.intent.path === "rebind") {
        const providerAction = this.#database.prepare(`
          UPDATE provider_actions
             SET status='terminal', history_json=CASE status
                   WHEN 'ambiguous' THEN '["prepared","dispatched","ambiguous","accepted","terminal"]'
                   WHEN 'accepted' THEN '["prepared","dispatched","accepted","terminal"]'
                   ELSE '["prepared","dispatched","accepted","terminal"]'
                 END,
                 provider_session_generation=?, effect_count=1, idempotency_proven=1,
                 result_json=?, journal_revision=journal_revision+1, updated_at=?
           WHERE adapter_id=? AND action_id=? AND status IN ('dispatched','accepted','ambiguous')
        `).run(
          integer(target, "new_provider_session_generation"),
          canonicalJson(resolution),
          now,
          handle.intent.providerAdapterId,
          handle.intent.providerActionId,
        );
        if (providerAction.changes !== 1) stale("chair recovery provider action changed during commit");
      }
      const commit: ChairRecoveryCommit = {
        status: "committed",
        recoveryId: handle.recoveryId,
        path: handle.intent.path,
        evidenceDigest,
      };
      const terminal = this.#database.prepare(`
        UPDATE chair_bridge_recovery_custody
           SET state='terminal', result_json=?, revision=revision+1, updated_at=?
         WHERE recovery_id=? AND state='committing'
      `).run(canonicalJson(commit), now, handle.recoveryId);
      if (terminal.changes !== 1) stale("chair recovery custody changed before terminal commit");
      return commit;
    })();
  }

  auditRetainedChairBridges(
    result: { recoveryRequired: number; ambiguous: number },
    errors: unknown[],
  ): void {
    const hasRetainedBridge = this.#adapterEffects.hasRetainedChairBridge;
    if (hasRetainedBridge === undefined) return;
    const active = this.#database.prepare(`
      SELECT * FROM launched_chair_bridge_state bridge WHERE state='active'
       ORDER BY project_session_id, coordination_run_id
    `).all().filter(isRow).filter((state) => !this.#repository.hasValidBridgeRetirement(
      text(state, "project_session_id"),
      text(state, "coordination_run_id"),
    ));
    for (const state of active) {
      const entry = {
        projectSessionId: text(state, "project_session_id"),
        runId: text(state, "coordination_run_id"),
        agentId: text(state, "chair_agent_id"),
        principalGeneration: integer(state, "principal_generation"),
        adapterId: text(state, "provider_adapter_id"),
        actionId: text(state, "provider_action_id"),
        providerSessionRef: text(state, "provider_session_ref"),
        providerSessionGeneration: integer(state, "provider_session_generation"),
        bridgeGeneration: integer(state, "bridge_generation"),
      };
      let retained = false;
      let reason = "daemon startup found no exact retained chair bridge";
      try {
        retained = hasRetainedBridge(entry);
      } catch (error: unknown) {
        reason = `daemon startup chair bridge audit failed: ${error instanceof Error ? error.name : "unknown error"}`;
      }
      if (retained) continue;
      try {
        const persisted = this.#database.transaction(() => this.#repository.persistChairBridgeLoss({
          ...entry,
          reason,
        }))();
        if (persisted) result.recoveryRequired += 1;
      } catch (error: unknown) {
        // Keep auditing sibling sessions. The unchanged row remains visible on the next recovery pass.
        result.ambiguous += 1;
        errors.push(error);
      }
    }
  }

  async recoverChairRecoveryCustody(
    result: {
      lookedUp: number;
      activated: number;
      failed: number;
      ambiguous: number;
      recoveryRequired: number;
    },
    reconcileTakeover: ChairRecoveryTakeoverReconcilePort,
  ): Promise<void> {
    const prepared = this.#database.prepare(`
      SELECT custody.*,loss.coordination_run_id
        FROM chair_bridge_recovery_custody custody
        JOIN chair_bridge_losses loss ON loss.loss_id=custody.loss_id
       WHERE custody.state='prepared' ORDER BY custody.created_at, custody.recovery_id
    `).all().filter(isRow);
    for (const custody of prepared) {
      if (text(custody, "path") === "rebind" && typeof custody.provider_action_id === "string") {
        assertProviderActionOwner(this.#database, {
          runId: text(custody, "coordination_run_id"),
          adapterId: text(custody, "provider_adapter_id"),
          actionId: custody.provider_action_id,
        }, "chair_recovery");
      }
      const now = this.#clock();
      this.#database.transaction(() => {
        if (text(custody, "path") === "rebind" && typeof custody.new_capability_hash === "string") {
          this.#database.prepare(`UPDATE capabilities SET revoked_at=? WHERE token_hash=? AND revoked_at IS NULL`)
            .run(now, custody.new_capability_hash);
        }
        if (typeof custody.provider_action_id === "string") {
          const proof = {
            schemaVersion: 1,
            kind: "chair-recovery-pre-dispatch-no-effect",
            recoveryId: text(custody, "recovery_id"),
            executionCount: 0,
          };
          this.#database.prepare(`
            UPDATE provider_actions
               SET status='terminal', history_json='["prepared","terminal"]',
                   execution_count=0, effect_count=0, idempotency_proven=1,
                   result_json=?, journal_revision=journal_revision+1, updated_at=?
             WHERE adapter_id=? AND action_id=? AND status='prepared'
          `).run(
            canonicalJson({ ...proof, evidenceDigest: jsonEvidenceDigest(proof) }),
            now,
            text(custody, "provider_adapter_id"),
            custody.provider_action_id,
          );
        }
        this.#database.prepare(`
          UPDATE chair_bridge_recovery_custody
             SET state='no-effect', result_json=?, revision=revision+1, updated_at=?
           WHERE recovery_id=? AND state='prepared'
        `).run(
          canonicalJson({ status: "no-effect", reason: "prepared-before-restart" }),
          now,
          text(custody, "recovery_id"),
        );
      })();
      result.failed += 1;
      result.recoveryRequired += 1;
    }
    const observable = this.#database.prepare(`
      SELECT custody.*,loss.coordination_run_id
        FROM chair_bridge_recovery_custody custody
        JOIN chair_bridge_losses loss ON loss.loss_id=custody.loss_id
       WHERE custody.path='rebind' AND custody.state IN ('dispatched','accepted','ambiguous')
       ORDER BY custody.created_at, custody.recovery_id
    `).all().filter(isRow);
    for (const custody of observable) {
      if (this.#adapterEffects.lookupChairRecovery === undefined || typeof custody.provider_action_id !== "string") {
        result.ambiguous += 1;
        result.recoveryRequired += 1;
        continue;
      }
      assertProviderActionOwner(this.#database, {
        runId: text(custody, "coordination_run_id"),
        adapterId: text(custody, "provider_adapter_id"),
        actionId: custody.provider_action_id,
      }, "chair_recovery");
      let record: unknown;
      try {
        record = await this.#adapterEffects.lookupChairRecovery({
          adapterId: text(custody, "provider_adapter_id"),
          actionId: custody.provider_action_id,
        });
        result.lookedUp += 1;
      } catch {
        result.ambiguous += 1;
        result.recoveryRequired += 1;
        continue;
      }
      const provider = this.chairRecoveryLookupResult(custody, record);
      if (provider === undefined) {
        result.ambiguous += 1;
        result.recoveryRequired += 1;
        continue;
      }
      const intentValue: unknown = JSON.parse(text(custody, "intent_json"));
      if (!isRow(intentValue) || intentValue.kind !== "chair-bridge-recovery" || intentValue.path !== "rebind") {
        throw new Error("stored chair recovery intent is invalid");
      }
      const intent = intentValue as ChairBridgeRecoveryIntent & { path: "rebind" };
      const handle: ChairRecoveryDispatchHandle & Readonly<{ intent: typeof intent }> = {
        schemaVersion: 1,
        recoveryId: text(custody, "recovery_id"),
        intent,
        intentDigest: text(custody, "intent_digest") as Digest,
        inspectionDigest: jsonEvidenceDigest({ recovery: text(custody, "recovery_id") }),
        operatorId: text(custody, "operator_id"),
        operatorCommandId: text(custody, "operator_command_id"),
      };
      this.commitActiveChairRecovery(
        handle,
        jsonEvidenceDigest(provider.fabricContinuity),
      );
      result.activated += 1;
    }
    const takeoverObservable = this.#database.prepare(`
      SELECT operator_id, operator_command_id
        FROM chair_bridge_recovery_custody
       WHERE path='takeover' AND state IN ('dispatched','accepted','ambiguous')
       ORDER BY created_at, recovery_id
    `).all().filter(isRow);
    for (const custody of takeoverObservable) {
      const reconciled = await reconcileTakeover(
        text(custody, "operator_id"),
        text(custody, "operator_command_id"),
      );
      result.lookedUp += 1;
      if (reconciled.status === "committed") result.activated += 1;
      else if (reconciled.status === "no-effect") {
        result.failed += 1;
        result.recoveryRequired += 1;
      } else {
        result.ambiguous += 1;
        result.recoveryRequired += 1;
      }
    }
  }
}
