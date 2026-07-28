import type { Sha256Digest } from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError } from "./contracts.js";
import { jsonEvidenceDigest } from "./provider-agent-custody.js";
import {
  canonicalJson,
  integer,
  isRow,
  row,
  sha256,
  text,
} from "../persistence/row-codec.js";
import { supersedeFinalAcceptanceGates } from "./acceptance-cycle.js";
import type { ChairBridgeLossObservation, RetainedChairBridge } from "./launch-custody.js";

type Digest = Sha256Digest;

function stale(message: string): never {
  throw new ProjectFabricCoreError("STALE_REVISION", message);
}

export type ChairRecoveryRetainedBridgeRepositoryOptions = Readonly<{
  database: Database.Database;
  clock: () => number;
  daemonInstanceGeneration: () => number;
}>;

/**
 * Byte-moved from `LaunchCustodyService`'s chair-recovery family (S4d, plan §2): durable reads
 * and writes of retained chair-bridge rows — loss persistence, bridge-retirement proof, and the
 * recovery-manifest digest. No dependency on `chair-recovery-custody.ts` or
 * `chair-recovery-settlement.ts`, so it is the leaf of the family's dependency graph. Preserves:
 * loss evidence inserted before all fences, and the exact fencing statement order.
 */
export class ChairRecoveryRetainedBridgeRepository {
  readonly #database: Database.Database;
  readonly #clock: () => number;
  readonly #daemonInstanceGeneration: () => number;

  constructor(options: ChairRecoveryRetainedBridgeRepositoryOptions) {
    this.#database = options.database;
    this.#clock = options.clock;
    this.#daemonInstanceGeneration = options.daemonInstanceGeneration;
  }

  chairRecoveryManifestDigest(input: RetainedChairBridge): Digest {
    const manifest = {
      schemaVersion: 1,
      projectSession: this.#database.prepare(`
        SELECT project_session_id, state, revision, generation, membership_revision
          FROM project_sessions WHERE project_session_id=?
      `).get(input.projectSessionId),
      run: this.#database.prepare(`
        SELECT run_id, lifecycle_state, revision, chair_agent_id, chair_generation,
               chair_lease_id, authority_ref, dependency_revision
          FROM runs WHERE run_id=?
      `).get(input.runId),
      tasks: this.#database.prepare(`
        SELECT task_id, state, owner_agent_id, revision, owner_lease_generation
          FROM tasks WHERE run_id=? ORDER BY task_id
      `).all(input.runId),
      mailbox: this.#database.prepare(`
        SELECT recipient_id, next_sequence, contiguous_watermark
          FROM mailbox_state WHERE run_id=? ORDER BY recipient_id
      `).all(input.runId),
      leases: this.#database.prepare(`
        SELECT lease_id, kind, holder_agent_id, generation, status, expires_at
          FROM leases WHERE run_id=? ORDER BY lease_id
      `).all(input.runId),
      chairLeases: this.#database.prepare(`
        SELECT lease_id, holder_agent_id, generation, status, handoff_digest
          FROM run_chair_leases WHERE run_id=? ORDER BY generation
      `).all(input.runId),
      checkpoints: this.#database.prepare(`
        SELECT checkpoint_id, agent_id, task_id, task_revision, sha256, created_at
          FROM lifecycle_checkpoints WHERE run_id=? ORDER BY checkpoint_id
      `).all(input.runId),
      provider: this.#database.prepare(`
        SELECT provider_session_generation, context_revision, reconciled_checkpoint_sha256
          FROM provider_state WHERE run_id=? AND agent_id=?
      `).get(input.runId, input.agentId),
      providerAction: this.#database.prepare(`
        SELECT status, journal_revision, provider_session_generation, effect_count
          FROM provider_actions WHERE adapter_id=? AND action_id=?
      `).get(input.adapterId, input.actionId),
      memberships: this.#database.prepare(`
        SELECT member_kind, member_id, required, state, revision, abandoned_reason
          FROM project_session_memberships
         WHERE project_session_id=? AND coordination_run_id=?
         ORDER BY member_kind, member_id
      `).all(input.projectSessionId, input.runId),
    };
    return jsonEvidenceDigest(manifest);
  }

  hasValidBridgeRetirement(projectSessionId: string, coordinationRunId: string): boolean {
    return this.#database.prepare(`
      SELECT 1
        FROM launched_chair_bridge_retirements retirement
        JOIN launched_chair_bridge_state bridge
          ON bridge.project_session_id=retirement.project_session_id
         AND bridge.coordination_run_id=retirement.coordination_run_id
        JOIN runs run ON run.project_session_id=bridge.project_session_id
                     AND run.run_id=bridge.coordination_run_id
        JOIN project_sessions session ON session.project_session_id=bridge.project_session_id
        JOIN run_chair_leases lease
          ON lease.project_session_id=run.project_session_id
         AND lease.run_id=run.run_id
         AND lease.lease_id=run.chair_lease_id
         AND lease.generation=run.chair_generation
        JOIN capabilities capability ON capability.token_hash=bridge.capability_hash
        JOIN agents agent ON agent.run_id=bridge.coordination_run_id
                         AND agent.agent_id=bridge.chair_agent_id
       WHERE retirement.project_session_id=? AND retirement.coordination_run_id=?
         AND bridge.state IN ('active','abandoned')
         AND run.lifecycle_state IN ('closed','cancelled','launch_failed')
         AND session.state IN ('closed','cancelled')
         AND session.terminal_path_json=retirement.terminal_ref
         AND json_valid(session.terminal_path_json)=1
         AND json_extract(session.terminal_path_json,'$.kind')=retirement.terminal_kind
         AND run.chair_agent_id=bridge.chair_agent_id
         AND lease.holder_agent_id=bridge.chair_agent_id
         AND lease.status='revoked'
         AND capability.revoked_at IS NOT NULL
         AND agent.lifecycle='archived'
         AND (
           (retirement.source_kind='project-session-close' AND EXISTS (
             SELECT 1 FROM operator_commands command
              WHERE command.project_session_id=retirement.project_session_id
                AND command.command_id=retirement.owner_ref
                AND command.operator_id=retirement.owner_operator_id
                AND command.operation='decide' AND command.status='committed'
                AND json_valid(command.result_json)=1
                AND json_extract(command.result_json,'$.projectSessionId')=retirement.project_session_id
                AND json_extract(command.result_json,'$.terminalPath.kind')=retirement.terminal_kind
           )) OR
           (retirement.source_kind='project-session-stop' AND EXISTS (
             SELECT 1 FROM operator_effect_custody custody
              WHERE custody.project_session_id=retirement.project_session_id
                AND custody.command_id=retirement.owner_ref
                AND custody.operator_id=retirement.owner_operator_id
                AND custody.operation='project-session-stop'
                AND custody.state IN ('dispatching','terminal')
                AND json_valid(custody.intent_json)=1
                AND json_extract(custody.intent_json,'$.kind')='project-session-stop'
                AND json_extract(custody.intent_json,'$.projectSessionId')=retirement.project_session_id
           )) OR
           (retirement.source_kind='chair-recovery-abandon' AND EXISTS (
             SELECT 1 FROM chair_bridge_recovery_custody recovery
              JOIN chair_bridge_losses loss ON loss.loss_id=recovery.loss_id
              WHERE recovery.recovery_id=retirement.owner_ref AND recovery.path='abandon'
                AND recovery.operator_id=retirement.owner_operator_id
                AND recovery.state='terminal'
                AND loss.project_session_id=retirement.project_session_id
                AND loss.coordination_run_id=retirement.coordination_run_id
           ))
         )
       LIMIT 1
    `).get(projectSessionId, coordinationRunId) !== undefined;
  }

  persistChairBridgeLoss(input: ChairBridgeLossObservation): boolean {
    const stateValue = this.#database.prepare(`
      SELECT * FROM launched_chair_bridge_state
       WHERE project_session_id=? AND coordination_run_id=?
    `).get(input.projectSessionId, input.runId);
    if (!isRow(stateValue)) return false;
    if (this.hasValidBridgeRetirement(input.projectSessionId, input.runId)) return false;
    const state = stateValue;
    const exact =
      text(state, "chair_agent_id") === input.agentId &&
      text(state, "provider_adapter_id") === input.adapterId &&
      text(state, "provider_action_id") === input.actionId &&
      text(state, "provider_session_ref") === input.providerSessionRef &&
      integer(state, "provider_session_generation") === input.providerSessionGeneration &&
      integer(state, "principal_generation") === input.principalGeneration &&
      integer(state, "bridge_generation") === input.bridgeGeneration;
    if (!exact) throw new ProjectFabricCoreError("STALE_GENERATION", "chair bridge loss does not match retained custody");
    if (text(state, "state") === "lost") return false;
    if (text(state, "state") !== "active") {
      throw new ProjectFabricCoreError("CONFLICT", "chair bridge is not active");
    }
    const reason = input.reason.slice(0, 160) || "retained chair bridge lost";
    const now = this.#clock();
    const daemonInstanceGeneration = this.#daemonInstanceGeneration();
    const recoveryManifestDigest = this.chairRecoveryManifestDigest(input);
    const lossBinding = {
      projectSessionId: input.projectSessionId,
      runId: input.runId,
      agentId: input.agentId,
      principalGeneration: input.principalGeneration,
      adapterId: input.adapterId,
      actionId: input.actionId,
      providerSessionRef: input.providerSessionRef,
      providerSessionGeneration: input.providerSessionGeneration,
      bridgeGeneration: input.bridgeGeneration,
      daemonInstanceGeneration,
      reason,
      recoveryManifestDigest,
    };
    const lossId = `chair-bridge-loss:${sha256(canonicalJson({
      runId: input.runId,
      bridgeGeneration: input.bridgeGeneration,
      capabilityHash: text(state, "capability_hash"),
    }))}`;
    const sessionBeforeLoss = row(this.#database.prepare(`
      SELECT state FROM project_sessions WHERE project_session_id=?
    `).get(input.projectSessionId), "chair loss project session");
    const priorSessionState = text(sessionBeforeLoss, "state");
    const superseded = priorSessionState === "quiescing" || priorSessionState === "awaiting_acceptance"
      ? supersedeFinalAcceptanceGates({
          database: this.#database,
          projectSessionId: input.projectSessionId,
          cause: { kind: "chair-bridge-loss", ref: lossId },
          reason: "chair bridge loss exited the acceptance cycle",
          now,
        })
      : { gateChanges: 0, membershipChanges: 0 };
    const evidenceDigest = jsonEvidenceDigest(lossBinding);
    this.#database.prepare(`
      INSERT INTO chair_bridge_losses(
        loss_id, project_session_id, coordination_run_id, chair_agent_id,
        provider_adapter_id, provider_action_id, provider_contract_digest, provider_session_ref,
        provider_session_generation, principal_generation, lost_bridge_generation,
        next_bridge_generation, capability_hash, daemon_instance_generation,
        reason, evidence_digest, recovery_manifest_digest, created_at
      ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
    `).run(
      lossId,
      input.projectSessionId,
      input.runId,
      input.agentId,
      input.adapterId,
      input.actionId,
      text(state, "provider_contract_digest"),
      input.providerSessionRef,
      input.providerSessionGeneration,
      input.principalGeneration,
      input.bridgeGeneration,
      input.bridgeGeneration + 1,
      text(state, "capability_hash"),
      daemonInstanceGeneration,
      reason,
      evidenceDigest,
      recoveryManifestDigest,
      now,
    );
    const changed = this.#database.prepare(`
      UPDATE launched_chair_bridge_state
         SET state='lost', revision=revision+1, updated_at=?
       WHERE project_session_id=? AND coordination_run_id=?
         AND state='active' AND revision=?
    `).run(now, input.projectSessionId, input.runId, integer(state, "revision"));
    if (changed.changes !== 1) stale("chair bridge state changed during loss fencing");
    const revokedCapability = this.#database.prepare(
      "UPDATE capabilities SET revoked_at=? WHERE token_hash=? AND revoked_at IS NULL",
    ).run(now, text(state, "capability_hash"));
    if (revokedCapability.changes !== 1) stale("chair capability changed during loss fencing");
    const frozenLease = this.#database.prepare(`
      UPDATE run_chair_leases SET status='frozen', updated_at=?
       WHERE project_session_id=? AND run_id=? AND holder_agent_id=? AND status='active'
    `).run(now, input.projectSessionId, input.runId, input.agentId);
    if (frozenLease.changes !== 1) stale("active chair lease changed during loss fencing");
    this.#database.prepare(`
      INSERT INTO delivery_freezes(run_id, agent_id, reason, created_at)
      VALUES (?, ?, ?, ?)
      ON CONFLICT(run_id, agent_id) DO UPDATE SET
        reason=excluded.reason, created_at=excluded.created_at
    `).run(input.runId, input.agentId, lossId, now);
    const suspended = this.#database.prepare(
      "UPDATE agents SET lifecycle='suspended' WHERE run_id=? AND agent_id=?",
    ).run(input.runId, input.agentId);
    if (suspended.changes !== 1) stale("chair identity changed during loss fencing");
    const fencedRun = this.#database.prepare(`
      UPDATE runs SET lifecycle_state='recovery_required', revision=revision+1
       WHERE run_id=? AND lifecycle_state IN (
         'active','quiescing','awaiting_acceptance','visibility_degraded',
         'reconciling','quarantined'
       )
    `).run(input.runId);
    if (fencedRun.changes !== 1) stale("run state changed during chair loss fencing");
    const fencedSession = this.#database.prepare(`
      UPDATE project_sessions
         SET state='recovery_required', membership_revision=membership_revision+?,
             revision=revision+1, updated_at=?
       WHERE project_session_id=? AND state IN (
         'active','quiescing','awaiting_acceptance','visibility_degraded',
         'reconciling','quarantined'
       )
    `).run(superseded.membershipChanges + superseded.gateChanges > 0 ? 1 : 0, now, input.projectSessionId);
    if (fencedSession.changes !== 1) stale("project session state changed during chair loss fencing");
    return true;
  }
}
