import type { ChairLiveHandoffIntent, Sha256Digest } from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError, type AuthenticatedOperatorContext } from "./contracts.js";
import { jsonEvidenceDigest } from "./provider-agent-custody.js";
import {
  canonicalJson,
  integer,
  isRow,
  row,
  sha256,
  text,
  type Row,
} from "../persistence/row-codec.js";
import {
  ProviderActionAdmissionCoordinator,
  type ProviderActionTicket,
} from "../application/provider-action-admission.js";
import { assertProviderActionOwner } from "../application/provider-action-owner.js";
import type { RetainedChairBridge } from "./launch-custody.js";

type Digest = Sha256Digest;

function stale(message: string): never {
  throw new ProjectFabricCoreError("STALE_REVISION", message);
}

export type ChairLiveHandoffInspection = Readonly<{
  intent: ChairLiveHandoffIntent;
  inspectionDigest: Digest;
}>;

export type ChairLiveHandoffDispatchHandle = Readonly<{
  schemaVersion: 1;
  custodyId: string;
  promotionActionId: string;
  intent: ChairLiveHandoffIntent;
  intentDigest: Digest;
  inspectionDigest: Digest;
  operatorId: string;
  operatorCommandId: string;
}>;

export type ChairLiveHandoffCommit = Readonly<{
  status: "committed" | "ambiguous" | "pending" | "no-effect";
  custodyId: string;
  evidenceDigest: Digest;
}>;

export type ChairLiveHandoffCurrentState = Readonly<{
  revision: number;
  inspectionDigest: Digest;
}>;

export type ChairLiveHandoffCustodyRecoveryPort = Readonly<{
  markNoEffect(handle: ChairLiveHandoffDispatchHandle, reason: string): ChairLiveHandoffCommit;
  reconcile(operatorId: string, operatorCommandId: string): Promise<ChairLiveHandoffCommit>;
}>;

export type RetainedSuccessorBridgeProbe = Readonly<{
  projectSessionId: string;
  runId: string;
  agentId: string;
  principalGeneration: number;
  adapterId: string;
  actionId: string;
  providerSessionRef: string;
  providerSessionGeneration: number;
  sourceBridgeGeneration: number;
  chairBridgeGeneration: number;
  sourceActionId?: string;
  promotionActionId?: string;
}>;

export type ChairLiveHandoffAdapterEffectsPort = Readonly<{
  lookupRetainedSuccessorBridge?(input: RetainedSuccessorBridgeProbe): Promise<"child" | "chair" | "missing">;
  promoteRetainedSuccessorBridge?(input: RetainedSuccessorBridgeProbe): Promise<boolean>;
}>;

export type ChairLiveHandoffCustodyAdapterOptions = Readonly<{
  database: Database.Database;
  providerActionAdmission: ProviderActionAdmissionCoordinator;
  clock: () => number;
  fault: (label: string) => void;
  adapterContracts: { inspect(adapterId: string): Promise<unknown> };
  adapterEffects: ChairLiveHandoffAdapterEffectsPort;
  retireVolatileChairBridge?: (entry: RetainedChairBridge) => void;
}>;

/**
 * Byte-moved from `LaunchCustodyService`'s chair-live-handoff family (S4c, plan §2): admission,
 * retained-successor lookup/promotion, and volatile predecessor retirement. Preserves: custody +
 * provider action + predecessor/run/dual-delivery fences in the caller's transaction, `dispatched`
 * persisted before promotion I/O, both parties fenced on ambiguity, no-effect restored atomically,
 * and the bridge/capability/lease/membership/chair swap committing before volatile predecessor
 * retirement.
 */
export class ChairLiveHandoffCustodyAdapter {
  readonly #database: Database.Database;
  readonly #providerActionAdmission: ProviderActionAdmissionCoordinator;
  readonly #clock: () => number;
  readonly #fault: (label: string) => void;
  readonly #adapterContracts: ChairLiveHandoffCustodyAdapterOptions["adapterContracts"];
  readonly #adapterEffects: ChairLiveHandoffAdapterEffectsPort;
  readonly #retireVolatileChairBridge: ((entry: RetainedChairBridge) => void) | undefined;

  constructor(options: ChairLiveHandoffCustodyAdapterOptions) {
    this.#database = options.database;
    this.#providerActionAdmission = options.providerActionAdmission;
    this.#clock = options.clock;
    this.#fault = options.fault;
    this.#adapterContracts = options.adapterContracts;
    this.#adapterEffects = options.adapterEffects;
    this.#retireVolatileChairBridge = options.retireVolatileChairBridge;
  }

  async readChairLiveHandoffCurrentState(
    intent: ChairLiveHandoffIntent,
  ): Promise<ChairLiveHandoffCurrentState> {
    return {
      revision: intent.expectedBridgeRevision,
      inspectionDigest: this.#chairLiveHandoffInspectionDigest(intent),
    };
  }

  async inspectChairLiveHandoff(intent: ChairLiveHandoffIntent): Promise<ChairLiveHandoffInspection> {
    const inspectionDigest = this.#chairLiveHandoffInspectionDigest(intent);
    const contract = await this.#adapterContracts.inspect(intent.providerAdapterId);
    if (`sha256:${sha256(canonicalJson(contract))}` !== intent.providerContractDigest) {
      throw new ProjectFabricCoreError("STALE_REVISION", "chair live handoff provider contract changed");
    }
    return { intent, inspectionDigest };
  }

  preflightChairLiveHandoff(input: Readonly<{
    inspection: ChairLiveHandoffInspection;
    operatorCommandId: string;
    principal: AuthenticatedOperatorContext;
  }>): ProviderActionTicket {
    const intentDigest = jsonEvidenceDigest(input.inspection.intent);
    const custodyId = `chair-live-handoff:${sha256(canonicalJson({
      intentDigest,
      operatorId: input.principal.operatorId,
      operatorCommandId: input.operatorCommandId,
    }))}`;
    const promotionActionId = `chair-promotion:${sha256(custodyId)}`;
    return this.#providerActionAdmission.preflight({
      actionRef: {
        adapterId: input.inspection.intent.providerAdapterId,
        actionId: promotionActionId,
      },
      scope: {
        kind: "run-action",
        runId: input.inspection.intent.coordinationRunId,
      },
      principal: input.principal,
      canonicalInput: {
        schemaVersion: 1,
        operation: "promote_retained_bridge",
        intent: input.inspection.intent,
      },
    });
  }

  prepareChairLiveHandoffInTransaction(input: Readonly<{
    inspection: ChairLiveHandoffInspection;
    operatorId: string;
    operatorCommandId: string;
    providerActionTicket: ProviderActionTicket;
  }>): ChairLiveHandoffDispatchHandle {
    const { intent } = input.inspection;
    if (this.#chairLiveHandoffInspectionDigest(intent) !== input.inspection.inspectionDigest) {
      stale("chair live handoff changed after inspection");
    }
    const intentDigest = jsonEvidenceDigest(intent);
    const custodyId = `chair-live-handoff:${sha256(canonicalJson({
      intentDigest,
      operatorId: input.operatorId,
      operatorCommandId: input.operatorCommandId,
    }))}`;
    const promotionActionId = `chair-promotion:${sha256(custodyId)}`;
    const existing = this.#database.prepare(`
      SELECT intent_digest, custody_id, promotion_action_id FROM chair_live_handoff_custody
       WHERE operator_id=? AND operator_command_id=?
    `).get(input.operatorId, input.operatorCommandId);
    if (isRow(existing)) {
      assertProviderActionOwner(this.#database, {
        runId: intent.coordinationRunId,
        adapterId: intent.providerAdapterId,
        actionId: text(existing, "promotion_action_id"),
      }, "chair_live_handoff");
      if (text(existing, "intent_digest") !== intentDigest) {
        throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "chair live handoff command was reused with changed intent");
      }
      return {
        schemaVersion: 1,
        custodyId: text(existing, "custody_id"),
        promotionActionId: text(existing, "promotion_action_id"),
        intent,
        intentDigest,
        inspectionDigest: input.inspection.inspectionDigest,
        operatorId: input.operatorId,
        operatorCommandId: input.operatorCommandId,
      };
    }
    const successor = this.#chairLiveHandoffSuccessor(intent);
    const now = this.#clock();
    this.#database.prepare(`
      INSERT INTO chair_live_handoff_custody(
        custody_id, operator_id, operator_command_id, project_session_id, coordination_run_id,
        intent_digest, intent_json, handoff_path, handoff_digest, predecessor_agent_id,
        successor_agent_id, successor_authority_id, successor_authority_digest,
        expected_session_revision, expected_session_generation, expected_membership_revision,
        expected_run_revision, expected_chair_generation, expected_chair_lease_id,
        expected_bridge_revision, expected_chair_bridge_generation,
        expected_predecessor_principal_generation, expected_successor_principal_generation,
        expected_successor_bridge_revision, expected_successor_bridge_generation,
        provider_adapter_id, provider_contract_digest, source_provider_action_id,
        promotion_action_id, provider_session_ref, provider_session_generation,
        new_bridge_generation, state, result_json, revision, created_at, updated_at
      ) VALUES (
        ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
        'prepared', NULL, 1, ?, ?
      )
    `).run(
      custodyId,
      input.operatorId,
      input.operatorCommandId,
      intent.projectSessionId,
      intent.coordinationRunId,
      intentDigest,
      canonicalJson(intent),
      intent.handoffRef.path,
      intent.handoffRef.digest,
      intent.predecessorAgentId,
      intent.successorAgentId,
      intent.successorAuthorityId,
      intent.successorAuthorityDigest,
      intent.expectedSessionRevision,
      intent.expectedSessionGeneration,
      intent.expectedMembershipRevision,
      intent.expectedRunRevision,
      intent.expectedChairGeneration,
      intent.expectedChairLeaseId,
      intent.expectedBridgeRevision,
      intent.expectedChairBridgeGeneration,
      intent.expectedPredecessorPrincipalGeneration,
      intent.expectedSuccessorPrincipalGeneration,
      intent.expectedSuccessorBridgeRevision,
      intent.expectedSuccessorBridgeGeneration,
      intent.providerAdapterId,
      intent.providerContractDigest,
      text(successor, "action_id"),
      promotionActionId,
      text(successor, "provider_session_ref"),
      integer(successor, "provider_session_generation"),
      Math.max(intent.expectedChairBridgeGeneration, intent.expectedSuccessorBridgeGeneration) + 1,
      now,
      now,
    );
    const payload = {
      schemaVersion: 1,
      custodyId,
      handoffRef: intent.handoffRef,
      predecessorAgentId: intent.predecessorAgentId,
      successorAgentId: intent.successorAgentId,
      sourceActionId: text(successor, "action_id"),
      promotionActionId,
    };
    const promotionPayloadJson = canonicalJson(payload);
    this.#providerActionAdmission.admitUnroutedInCurrentTransaction(input.providerActionTicket, {
      runId: intent.coordinationRunId,
      actionId: promotionActionId,
      adapterId: intent.providerAdapterId,
      operation: "promote_retained_bridge",
      targetAgentId: intent.successorAgentId,
      providerSessionGeneration: integer(successor, "provider_session_generation"),
      identityHash: sha256(canonicalJson({ custodyId, intentDigest })),
      payloadHash: sha256(promotionPayloadJson),
      payloadJson: promotionPayloadJson,
      status: "prepared",
      historyJson: '["prepared"]',
      executionCount: 0,
      updatedAt: now,
    }, "chair_live_handoff");
    const frozenLease = this.#database.prepare(`
      UPDATE run_chair_leases SET status='frozen', updated_at=?
       WHERE project_session_id=? AND run_id=? AND lease_id=? AND generation=? AND status='active'
    `).run(
      now,
      intent.projectSessionId,
      intent.coordinationRunId,
      intent.expectedChairLeaseId,
      intent.expectedChairGeneration,
    );
    if (frozenLease.changes !== 1) stale("chair live handoff predecessor lease changed");
    const reconciling = this.#database.prepare(`
      UPDATE runs SET lifecycle_state='reconciling', revision=revision+1
       WHERE project_session_id=? AND run_id=? AND lifecycle_state='active'
         AND revision=? AND chair_generation=? AND chair_agent_id=?
    `).run(
      intent.projectSessionId,
      intent.coordinationRunId,
      intent.expectedRunRevision,
      intent.expectedChairGeneration,
      intent.predecessorAgentId,
    );
    if (reconciling.changes !== 1) stale("chair live handoff run changed before fencing");
    const freeze = this.#database.prepare(`
      INSERT INTO delivery_freezes(run_id, agent_id, reason, created_at)
      VALUES (?, ?, ?, ?)
    `);
    freeze.run(intent.coordinationRunId, intent.predecessorAgentId, `chair-live-handoff:${custodyId}`, now);
    freeze.run(intent.coordinationRunId, intent.successorAgentId, `chair-live-handoff:${custodyId}`, now);
    this.#fault("chair-live-handoff:prepared");
    return {
      schemaVersion: 1,
      custodyId,
      promotionActionId,
      intent,
      intentDigest,
      inspectionDigest: input.inspection.inspectionDigest,
      operatorId: input.operatorId,
      operatorCommandId: input.operatorCommandId,
    };
  }

  async dispatchPreparedChairLiveHandoff(
    handle: ChairLiveHandoffDispatchHandle,
  ): Promise<ChairLiveHandoffCommit> {
    assertProviderActionOwner(this.#database, {
      runId: handle.intent.coordinationRunId,
      adapterId: handle.intent.providerAdapterId,
      actionId: handle.promotionActionId,
    }, "chair_live_handoff");
    const current = this.chairLiveHandoffStatus(handle.operatorId, handle.operatorCommandId);
    if (current.status !== "pending") return current;
    const custody = row(this.#database.prepare(`
      SELECT state FROM chair_live_handoff_custody WHERE custody_id=?
    `).get(handle.custodyId), "chair live handoff custody");
    if (text(custody, "state") !== "prepared") return current;
    const now = this.#clock();
    this.#database.transaction(() => {
      const changed = this.#database.prepare(`
        UPDATE chair_live_handoff_custody
           SET state='dispatched', revision=revision+1, updated_at=?
         WHERE custody_id=? AND state='prepared'
      `).run(now, handle.custodyId);
      const action = this.#database.prepare(`
        UPDATE provider_actions
           SET status='dispatched', history_json='["prepared","dispatched"]',
               execution_count=1, journal_revision=journal_revision+1, updated_at=?
         WHERE adapter_id=? AND action_id=? AND status='prepared'
      `).run(now, handle.intent.providerAdapterId, handle.promotionActionId);
      if (changed.changes !== 1 || action.changes !== 1) stale("chair live handoff changed before dispatch");
    })();
    this.#fault("chair-live-handoff:dispatched");
    const successor = this.#chairLiveHandoffSuccessor(handle.intent, true);
    const promotionInput = {
      projectSessionId: handle.intent.projectSessionId,
      runId: handle.intent.coordinationRunId,
      agentId: handle.intent.successorAgentId,
      principalGeneration: handle.intent.expectedSuccessorPrincipalGeneration,
      adapterId: handle.intent.providerAdapterId,
      actionId: text(successor, "action_id"),
      sourceActionId: text(successor, "action_id"),
      promotionActionId: handle.promotionActionId,
      providerSessionRef: text(successor, "provider_session_ref"),
      providerSessionGeneration: integer(successor, "provider_session_generation"),
      sourceBridgeGeneration: handle.intent.expectedSuccessorBridgeGeneration,
      chairBridgeGeneration: Math.max(
        handle.intent.expectedChairBridgeGeneration,
        handle.intent.expectedSuccessorBridgeGeneration,
      ) + 1,
    } as const;
    const promote = this.#adapterEffects.promoteRetainedSuccessorBridge;
    if (promote === undefined) return this.#markChairLiveHandoffAmbiguous(handle, "promotion capability unavailable");
    let promoted = false;
    try {
      promoted = await promote(promotionInput);
      this.#fault("chair-live-handoff:after-adapter");
    } catch (error: unknown) {
      return this.#markChairLiveHandoffAmbiguous(handle, error);
    }
    if (promoted) return this.#commitChairLiveHandoff(handle);
    return await this.#observeChairLiveHandoff(handle, promotionInput);
  }

  chairLiveHandoffStatus(operatorId: string, operatorCommandId: string): ChairLiveHandoffCommit {
    const custody = this.#database.prepare(`
      SELECT custody_id, state, result_json FROM chair_live_handoff_custody
       WHERE operator_id=? AND operator_command_id=?
    `).get(operatorId, operatorCommandId);
    if (!isRow(custody)) throw new ProjectFabricCoreError("NOT_FOUND", "chair live handoff custody was not found");
    const state = text(custody, "state");
    if (["terminal", "no-effect", "ambiguous"].includes(state) && typeof custody.result_json === "string") {
      const parsed: unknown = JSON.parse(custody.result_json);
      if (isRow(parsed) && typeof parsed.status === "string" && typeof parsed.evidenceDigest === "string") {
        return {
          status: parsed.status as ChairLiveHandoffCommit["status"],
          custodyId: text(custody, "custody_id"),
          evidenceDigest: parsed.evidenceDigest as Digest,
        };
      }
    }
    return {
      status: state === "ambiguous" ? "ambiguous" : "pending",
      custodyId: text(custody, "custody_id"),
      evidenceDigest: jsonEvidenceDigest({ custodyId: text(custody, "custody_id"), state }),
    };
  }

  async reconcileChairLiveHandoff(
    operatorId: string,
    operatorCommandId: string,
  ): Promise<ChairLiveHandoffCommit> {
    const custody = row(this.#database.prepare(`
      SELECT * FROM chair_live_handoff_custody
       WHERE operator_id=? AND operator_command_id=?
    `).get(operatorId, operatorCommandId), "chair live handoff custody");
    const storedIntent: unknown = JSON.parse(text(custody, "intent_json"));
    if (!isRow(storedIntent) || storedIntent.kind !== "chair-live-handoff") {
      throw new Error("stored chair live handoff intent is invalid");
    }
    const intent = storedIntent as ChairLiveHandoffIntent;
    assertProviderActionOwner(this.#database, {
      runId: intent.coordinationRunId,
      adapterId: intent.providerAdapterId,
      actionId: text(custody, "promotion_action_id"),
    }, "chair_live_handoff");
    const current = this.chairLiveHandoffStatus(operatorId, operatorCommandId);
    if (current.status !== "pending" && current.status !== "ambiguous") return current;
    const state = text(custody, "state");
    if (state === "prepared") return current;
    const handle: ChairLiveHandoffDispatchHandle = {
      schemaVersion: 1,
      custodyId: text(custody, "custody_id"),
      promotionActionId: text(custody, "promotion_action_id"),
      intent,
      intentDigest: text(custody, "intent_digest") as Digest,
      inspectionDigest: jsonEvidenceDigest({ custodyId: text(custody, "custody_id") }),
      operatorId,
      operatorCommandId,
    };
    const successor = this.#chairLiveHandoffSuccessor(intent, true);
    return await this.#observeChairLiveHandoff(handle, {
      projectSessionId: intent.projectSessionId,
      runId: intent.coordinationRunId,
      agentId: intent.successorAgentId,
      principalGeneration: intent.expectedSuccessorPrincipalGeneration,
      adapterId: intent.providerAdapterId,
      actionId: text(successor, "action_id"),
      sourceActionId: text(successor, "action_id"),
      promotionActionId: text(custody, "promotion_action_id"),
      providerSessionRef: text(successor, "provider_session_ref"),
      providerSessionGeneration: integer(successor, "provider_session_generation"),
      sourceBridgeGeneration: intent.expectedSuccessorBridgeGeneration,
      chairBridgeGeneration: integer(custody, "new_bridge_generation"),
    });
  }

  /**
   * Narrow bound-closure port for `ChairLiveHandoffCustodyRecoveryAdapter`
   * (`chair-live-handoff-custody-recovery.ts`, split out to keep this module under the
   * repository's line-count ceiling — same pattern as `ProviderAgentCustodyAdapter.recoveryPort()`
   * in `provider-agent-custody.ts`). Keeps the no-effect/reconcile transaction fences
   * byte-identical to before the split.
   */
  recoveryPort(): ChairLiveHandoffCustodyRecoveryPort {
    return {
      markNoEffect: (handle, reason) => this.#markChairLiveHandoffNoEffect(handle, reason),
      reconcile: (operatorId, operatorCommandId) => this.reconcileChairLiveHandoff(operatorId, operatorCommandId),
    };
  }

  #chairLiveHandoffInspectionDigest(intent: ChairLiveHandoffIntent): Digest {
    const session = row(this.#database.prepare(`
      SELECT project_id, mode, state, revision, generation, membership_revision
        FROM project_sessions WHERE project_session_id=?
    `).get(intent.projectSessionId), "chair live handoff session");
    const run = row(this.#database.prepare(`
      SELECT chair_agent_id, chair_generation, chair_lease_id, lifecycle_state, revision
        FROM runs WHERE project_session_id=? AND run_id=?
    `).get(intent.projectSessionId, intent.coordinationRunId), "chair live handoff run");
    const lease = row(this.#database.prepare(`
      SELECT holder_agent_id, generation, status FROM run_chair_leases
       WHERE project_session_id=? AND run_id=? AND lease_id=?
    `).get(intent.projectSessionId, intent.coordinationRunId, intent.expectedChairLeaseId), "chair live handoff lease");
    const bridge = row(this.#database.prepare(`
      SELECT * FROM launched_chair_bridge_state
       WHERE project_session_id=? AND coordination_run_id=?
    `).get(intent.projectSessionId, intent.coordinationRunId), "chair live handoff bridge");
    if (
      text(session, "mode") !== "coordinated" || text(session, "state") !== "active" ||
      integer(session, "revision") !== intent.expectedSessionRevision ||
      integer(session, "generation") !== intent.expectedSessionGeneration ||
      integer(session, "membership_revision") !== intent.expectedMembershipRevision ||
      text(run, "lifecycle_state") !== "active" || integer(run, "revision") !== intent.expectedRunRevision ||
      text(run, "chair_agent_id") !== intent.predecessorAgentId ||
      integer(run, "chair_generation") !== intent.expectedChairGeneration ||
      text(run, "chair_lease_id") !== intent.expectedChairLeaseId ||
      text(lease, "holder_agent_id") !== intent.predecessorAgentId ||
      integer(lease, "generation") !== intent.expectedChairGeneration || text(lease, "status") !== "active"
    ) throw new ProjectFabricCoreError("STALE_REVISION", "chair live handoff lifecycle binding changed");
    if (
      text(bridge, "state") !== "active" || integer(bridge, "revision") !== intent.expectedBridgeRevision ||
      text(bridge, "chair_agent_id") !== intent.predecessorAgentId ||
      integer(bridge, "bridge_generation") !== intent.expectedChairBridgeGeneration ||
      integer(bridge, "principal_generation") !== intent.expectedPredecessorPrincipalGeneration ||
      text(bridge, "provider_adapter_id") !== intent.providerAdapterId ||
      text(bridge, "provider_contract_digest") !== intent.providerContractDigest
    ) throw new ProjectFabricCoreError("STALE_REVISION", "chair live handoff bridge binding changed");
    if (!isRow(this.#database.prepare(`
      SELECT 1 FROM capabilities WHERE token_hash=? AND run_id=? AND agent_id=?
       AND principal_generation=? AND revoked_at IS NULL AND expires_at>?
    `).get(
      text(bridge, "capability_hash"),
      intent.coordinationRunId,
      intent.predecessorAgentId,
      intent.expectedPredecessorPrincipalGeneration,
      this.#clock(),
    ))) throw new ProjectFabricCoreError("STALE_PRINCIPAL_GENERATION", "chair live handoff predecessor capability is not live");
    if (isRow(this.#database.prepare(`
      SELECT 1 FROM chair_live_handoff_custody
       WHERE coordination_run_id=? AND state NOT IN ('terminal','no-effect')
    `).get(intent.coordinationRunId))) {
      throw new ProjectFabricCoreError("CONFLICT", "chair live handoff custody is already open");
    }
    if (isRow(this.#database.prepare(`
      SELECT 1 FROM chair_bridge_losses WHERE coordination_run_id=?
       AND loss_id NOT IN (SELECT loss_id FROM chair_bridge_loss_resolutions)
    `).get(intent.coordinationRunId))) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "lost chair recovery owns this run");
    }
    const projectId = text(session, "project_id");
    if (!isRow(this.#database.prepare(`
      SELECT 1 FROM artifacts WHERE project_id=? AND project_session_id=? AND run_id=?
       AND relative_path=? AND sha256=? AND registry_state='active'
    `).get(
      projectId,
      intent.projectSessionId,
      intent.coordinationRunId,
      intent.handoffRef.path,
      intent.handoffRef.digest,
    ))) throw new ProjectFabricCoreError("ARTIFACT_DIGEST_INVALID", "chair live handoff artifact is not registered to the run");
    const successor = this.#chairLiveHandoffSuccessor(intent);
    if (isRow(this.#database.prepare(`
      SELECT 1 FROM delivery_freezes WHERE run_id=? AND agent_id IN (?,?) LIMIT 1
    `).get(intent.coordinationRunId, intent.predecessorAgentId, intent.successorAgentId))) {
      throw new ProjectFabricCoreError("LIFECYCLE_PRECONDITION_FAILED", "chair live handoff participants are already frozen");
    }
    return jsonEvidenceDigest({ intent, session, run, lease, bridge, successor });
  }

  #chairLiveHandoffSuccessor(intent: ChairLiveHandoffIntent, allowFrozen = false): Row {
    const successor = row(this.#database.prepare(`
      SELECT bridge.*, custody.principal_generation, custody.authority_id,
             authority.authority_hash, authority.parent_authority_id,
             action.status AS action_status, action.execution_count, action.effect_count,
             agent.parent_agent_id, agent.lifecycle
        FROM agent_bridge_state bridge
        JOIN provider_agent_custody custody
          ON custody.run_id=bridge.run_id AND custody.action_id=bridge.action_id
         AND custody.adapter_id=bridge.adapter_id AND custody.target_agent_id=bridge.agent_id
         AND custody.bridge_capable=1
        JOIN provider_actions action
          ON action.run_id=bridge.run_id AND action.adapter_id=bridge.adapter_id
         AND action.action_id=bridge.action_id AND action.target_agent_id=bridge.agent_id
        JOIN agents agent ON agent.run_id=bridge.run_id AND agent.agent_id=bridge.agent_id
        JOIN authorities authority ON authority.run_id=bridge.run_id AND authority.authority_id=custody.authority_id
        JOIN capabilities capability ON capability.token_hash=bridge.capability_hash
         AND capability.run_id=bridge.run_id AND capability.agent_id=bridge.agent_id
         AND capability.principal_generation=custody.principal_generation
       WHERE bridge.run_id=? AND bridge.agent_id=? AND bridge.bridge_state='active'
         AND capability.revoked_at IS NULL AND capability.expires_at>?
    `).get(intent.coordinationRunId, intent.successorAgentId, this.#clock()), "chair live handoff successor");
    const authorityDigest = text(successor, "authority_hash");
    const canonicalAuthorityDigest = authorityDigest.startsWith("sha256:")
      ? authorityDigest
      : `sha256:${authorityDigest}`;
    const predecessorAuthority = row(this.#database.prepare(`
      SELECT authority_id FROM agents WHERE run_id=? AND agent_id=?
    `).get(intent.coordinationRunId, intent.predecessorAgentId), "chair live handoff predecessor authority");
    if (
      text(successor, "adapter_id") !== intent.providerAdapterId ||
      integer(successor, "principal_generation") !== intent.expectedSuccessorPrincipalGeneration ||
      integer(successor, "bridge_generation") !== intent.expectedSuccessorBridgeGeneration ||
      integer(successor, "revision") !== intent.expectedSuccessorBridgeRevision ||
      text(successor, "authority_id") !== intent.successorAuthorityId ||
      canonicalAuthorityDigest !== intent.successorAuthorityDigest ||
      text(successor, "parent_authority_id") !== text(predecessorAuthority, "authority_id") ||
      text(successor, "parent_agent_id") !== intent.predecessorAgentId ||
      text(successor, "action_status") !== "terminal" ||
      integer(successor, "execution_count") !== 1 || integer(successor, "effect_count") !== 1 ||
      (!allowFrozen && text(successor, "lifecycle") !== "ready") ||
      (allowFrozen && !["ready", "suspended"].includes(text(successor, "lifecycle")))
    ) throw new ProjectFabricCoreError("STALE_REVISION", "chair live handoff successor binding changed");
    return successor;
  }

  async #observeChairLiveHandoff(
    handle: ChairLiveHandoffDispatchHandle,
    input: Parameters<NonNullable<ChairLiveHandoffAdapterEffectsPort["lookupRetainedSuccessorBridge"]>>[0],
  ): Promise<ChairLiveHandoffCommit> {
    const lookup = this.#adapterEffects.lookupRetainedSuccessorBridge;
    if (lookup === undefined) return this.#markChairLiveHandoffAmbiguous(handle, "promotion lookup unavailable");
    let observed: "child" | "chair" | "missing";
    try {
      observed = await lookup(input);
    } catch (error: unknown) {
      return this.#markChairLiveHandoffAmbiguous(handle, error);
    }
    if (observed === "chair") return this.#commitChairLiveHandoff(handle);
    if (observed === "child") return this.#markChairLiveHandoffNoEffect(handle, "successor remained a child");
    return this.#markChairLiveHandoffAmbiguous(handle, "promotion state is mixed, missing or unprobeable");
  }

  #markChairLiveHandoffAmbiguous(
    handle: ChairLiveHandoffDispatchHandle,
    evidence: unknown,
  ): ChairLiveHandoffCommit {
    assertProviderActionOwner(this.#database, {
      runId: handle.intent.coordinationRunId,
      adapterId: handle.intent.providerAdapterId,
      actionId: handle.promotionActionId,
    }, "chair_live_handoff");
    const now = this.#clock();
    const evidenceDigest = jsonEvidenceDigest({
      custodyId: handle.custodyId,
      kind: "chair-live-handoff-ambiguous",
      evidence: evidence instanceof Error ? { name: evidence.name, message: evidence.message } : evidence,
    });
    this.#database.transaction(() => {
      this.#database.prepare(`
        UPDATE chair_live_handoff_custody
           SET state='ambiguous', result_json=?, revision=revision+1, updated_at=?
         WHERE custody_id=? AND state IN ('dispatched','ambiguous')
      `).run(canonicalJson({ status: "ambiguous", evidenceDigest }), now, handle.custodyId);
      this.#database.prepare(`
        UPDATE provider_actions SET status='ambiguous',
               history_json='["prepared","dispatched","ambiguous"]', result_json=?,
               journal_revision=journal_revision+1, updated_at=?
         WHERE adapter_id=? AND action_id=? AND status IN ('dispatched','ambiguous')
      `).run(
        canonicalJson({ kind: "chair-live-handoff-ambiguous", evidenceDigest }),
        now,
        handle.intent.providerAdapterId,
        handle.promotionActionId,
      );
    })();
    return { status: "ambiguous", custodyId: handle.custodyId, evidenceDigest };
  }

  #markChairLiveHandoffNoEffect(
    handle: ChairLiveHandoffDispatchHandle,
    reason: string,
  ): ChairLiveHandoffCommit {
    assertProviderActionOwner(this.#database, {
      runId: handle.intent.coordinationRunId,
      adapterId: handle.intent.providerAdapterId,
      actionId: handle.promotionActionId,
    }, "chair_live_handoff");
    const now = this.#clock();
    const evidenceDigest = jsonEvidenceDigest({ custodyId: handle.custodyId, kind: "chair-live-handoff-no-effect", reason });
    return this.#database.transaction((): ChairLiveHandoffCommit => {
      const custody = this.#database.prepare(`
        UPDATE chair_live_handoff_custody
           SET state='no-effect', result_json=?, revision=revision+1, updated_at=?
         WHERE custody_id=? AND state IN ('prepared','dispatched','ambiguous')
      `).run(canonicalJson({ status: "no-effect", evidenceDigest, reason }), now, handle.custodyId);
      const action = this.#database.prepare(`
        UPDATE provider_actions SET status='terminal', effect_count=0, idempotency_proven=1,
               history_json=CASE status WHEN 'prepared' THEN '["prepared","terminal"]'
                 ELSE '["prepared","dispatched","terminal"]' END, result_json=?,
               journal_revision=journal_revision+1, updated_at=?
         WHERE adapter_id=? AND action_id=? AND status IN ('prepared','dispatched','ambiguous')
      `).run(
        canonicalJson({ kind: "terminal-no-effect", evidenceDigest, reason }),
        now,
        handle.intent.providerAdapterId,
        handle.promotionActionId,
      );
      const run = this.#database.prepare(`
        UPDATE runs SET lifecycle_state='active', revision=revision+1
         WHERE run_id=? AND lifecycle_state='reconciling' AND revision=?
           AND chair_agent_id=? AND chair_generation=?
      `).run(
        handle.intent.coordinationRunId,
        handle.intent.expectedRunRevision + 1,
        handle.intent.predecessorAgentId,
        handle.intent.expectedChairGeneration,
      );
      const lease = this.#database.prepare(`
        UPDATE run_chair_leases SET status='active', updated_at=?
         WHERE project_session_id=? AND run_id=? AND lease_id=? AND status='frozen'
      `).run(
        now,
        handle.intent.projectSessionId,
        handle.intent.coordinationRunId,
        handle.intent.expectedChairLeaseId,
      );
      if (custody.changes !== 1 || action.changes !== 1 || run.changes !== 1 || lease.changes !== 1) {
        stale("chair live handoff no-effect restoration changed");
      }
      this.#database.prepare(`
        DELETE FROM delivery_freezes WHERE run_id=? AND agent_id IN (?,?)
          AND reason=?
      `).run(
        handle.intent.coordinationRunId,
        handle.intent.predecessorAgentId,
        handle.intent.successorAgentId,
        `chair-live-handoff:${handle.custodyId}`,
      );
      return { status: "no-effect", custodyId: handle.custodyId, evidenceDigest };
    })();
  }

  #commitChairLiveHandoff(handle: ChairLiveHandoffDispatchHandle): ChairLiveHandoffCommit {
    assertProviderActionOwner(this.#database, {
      runId: handle.intent.coordinationRunId,
      adapterId: handle.intent.providerAdapterId,
      actionId: handle.promotionActionId,
    }, "chair_live_handoff");
    const now = this.#clock();
    const predecessorBridge = row(this.#database.prepare(`
      SELECT * FROM launched_chair_bridge_state
       WHERE project_session_id=? AND coordination_run_id=?
    `).get(handle.intent.projectSessionId, handle.intent.coordinationRunId), "chair live handoff predecessor bridge");
    const successor = this.#chairLiveHandoffSuccessor(handle.intent, true);
    const newBridgeGeneration = Math.max(
      handle.intent.expectedChairBridgeGeneration,
      handle.intent.expectedSuccessorBridgeGeneration,
    ) + 1;
    const evidence = this.#database.transaction((): ChairLiveHandoffCommit => {
      const custody = this.#database.prepare(`
        UPDATE chair_live_handoff_custody
           SET state='committing', revision=revision+1, updated_at=?
         WHERE custody_id=? AND state IN ('dispatched','ambiguous')
      `).run(now, handle.custodyId);
      if (custody.changes !== 1) stale("chair live handoff changed before commit");
      this.#fault("chair-live-handoff:committing");
      const bridge = this.#database.prepare(`
        UPDATE launched_chair_bridge_state
           SET chair_agent_id=?, provider_action_id=?, provider_session_ref=?,
               provider_session_generation=?, principal_generation=?, bridge_generation=?,
               capability_hash=?, activation_evidence_digest=?, revision=revision+1, updated_at=?
         WHERE project_session_id=? AND coordination_run_id=? AND state='active' AND revision=?
           AND chair_agent_id=? AND bridge_generation=?
      `).run(
        handle.intent.successorAgentId,
        handle.promotionActionId,
        text(successor, "provider_session_ref"),
        integer(successor, "provider_session_generation"),
        handle.intent.expectedSuccessorPrincipalGeneration,
        newBridgeGeneration,
        text(successor, "capability_hash"),
        text(successor, "activation_evidence_digest"),
        now,
        handle.intent.projectSessionId,
        handle.intent.coordinationRunId,
        handle.intent.expectedBridgeRevision,
        handle.intent.predecessorAgentId,
        handle.intent.expectedChairBridgeGeneration,
      );
      if (bridge.changes !== 1) stale("chair live handoff bridge changed during commit");
      const predecessorCapability = this.#database.prepare(`
        UPDATE capabilities SET revoked_at=?
         WHERE token_hash=? AND run_id=? AND agent_id=? AND principal_generation=? AND revoked_at IS NULL
      `).run(
        now,
        text(predecessorBridge, "capability_hash"),
        handle.intent.coordinationRunId,
        handle.intent.predecessorAgentId,
        handle.intent.expectedPredecessorPrincipalGeneration,
      );
      const predecessorLease = this.#database.prepare(`
        UPDATE run_chair_leases SET status='revoked', updated_at=?
         WHERE project_session_id=? AND run_id=? AND lease_id=? AND status='frozen'
      `).run(
        now,
        handle.intent.projectSessionId,
        handle.intent.coordinationRunId,
        handle.intent.expectedChairLeaseId,
      );
      if (predecessorCapability.changes !== 1 || predecessorLease.changes !== 1) {
        stale("chair live handoff predecessor fence changed");
      }
      const newChairGeneration = handle.intent.expectedChairGeneration + 1;
      const successorLeaseId = `chair:${handle.intent.coordinationRunId}:${String(newChairGeneration)}:live-handoff`;
      this.#database.prepare(`
        INSERT INTO run_chair_leases(
          project_session_id, run_id, lease_id, holder_agent_id, generation, status, handoff_digest, updated_at
        ) VALUES (?, ?, ?, ?, ?, 'active', ?, ?)
      `).run(
        handle.intent.projectSessionId,
        handle.intent.coordinationRunId,
        successorLeaseId,
        handle.intent.successorAgentId,
        newChairGeneration,
        handle.intent.handoffRef.digest,
        now,
      );
      const retiredMembership = this.#database.prepare(`
        UPDATE project_session_memberships
           SET state='abandoned', abandoned_reason='chair-live-handoff',
               revision=revision+1, updated_at=?
         WHERE project_session_id=? AND coordination_run_id=? AND member_kind='lease'
           AND member_id=? AND required=1 AND state='active'
      `).run(
        now,
        handle.intent.projectSessionId,
        handle.intent.coordinationRunId,
        handle.intent.expectedChairLeaseId,
      );
      if (retiredMembership.changes !== 1) stale("chair live handoff predecessor membership changed");
      this.#database.prepare(`
        INSERT INTO project_session_memberships(
          project_session_id, coordination_run_id, member_kind, member_id,
          required, state, revision, abandoned_reason, created_at, updated_at
        ) VALUES (?, ?, 'lease', ?, 1, 'active', 1, NULL, ?, ?)
      `).run(
        handle.intent.projectSessionId,
        handle.intent.coordinationRunId,
        successorLeaseId,
        now,
        now,
      );
      const run = this.#database.prepare(`
        UPDATE runs SET chair_agent_id=?, chair_generation=?, chair_lease_id=?,
               lifecycle_state='active', revision=revision+1
         WHERE run_id=? AND project_session_id=? AND lifecycle_state='reconciling'
           AND revision=? AND chair_generation=? AND chair_agent_id=?
      `).run(
        handle.intent.successorAgentId,
        newChairGeneration,
        successorLeaseId,
        handle.intent.coordinationRunId,
        handle.intent.projectSessionId,
        handle.intent.expectedRunRevision + 1,
        handle.intent.expectedChairGeneration,
        handle.intent.predecessorAgentId,
      );
      const session = this.#database.prepare(`
        UPDATE project_sessions SET generation=generation+1,
               membership_revision=membership_revision+1, revision=revision+1, updated_at=?
         WHERE project_session_id=? AND state='active' AND revision=? AND generation=?
           AND membership_revision=?
      `).run(
        now,
        handle.intent.projectSessionId,
        handle.intent.expectedSessionRevision,
        handle.intent.expectedSessionGeneration,
        handle.intent.expectedMembershipRevision,
      );
      if (run.changes !== 1 || session.changes !== 1) stale("chair live handoff authority state changed");
      this.#database.prepare(`
        UPDATE agents SET lifecycle='suspended' WHERE run_id=? AND agent_id=?
      `).run(handle.intent.coordinationRunId, handle.intent.predecessorAgentId);
      const activated = this.#database.prepare(`
        UPDATE agents SET lifecycle='ready', authority_id=?, provider_session_ref=?
         WHERE run_id=? AND agent_id=? AND authority_id=?
      `).run(
        handle.intent.successorAuthorityId,
        text(successor, "provider_session_ref"),
        handle.intent.coordinationRunId,
        handle.intent.successorAgentId,
        handle.intent.successorAuthorityId,
      );
      const clearedBridge = this.#database.prepare(`
        UPDATE agent_bridge_state
           SET provider_session_ref=NULL, provider_session_generation=NULL,
               bridge_state='none', capability_hash=NULL, activation_evidence_digest=NULL,
               revision=revision+1, updated_at=?
         WHERE run_id=? AND agent_id=? AND bridge_state='active' AND revision=?
      `).run(
        now,
        handle.intent.coordinationRunId,
        handle.intent.successorAgentId,
        handle.intent.expectedSuccessorBridgeRevision,
      );
      if (activated.changes !== 1 || clearedBridge.changes !== 1) stale("chair live handoff successor changed");
      const resolution = {
        schemaVersion: 1,
        custodyId: handle.custodyId,
        predecessorAgentId: handle.intent.predecessorAgentId,
        successorAgentId: handle.intent.successorAgentId,
        promotionActionId: handle.promotionActionId,
        newChairGeneration,
        newBridgeGeneration,
      };
      const evidenceDigest = jsonEvidenceDigest(resolution);
      this.#database.prepare(`
        INSERT INTO chair_live_handoff_resolutions(
          custody_id, project_session_id, coordination_run_id, predecessor_agent_id,
          successor_agent_id, promotion_action_id, new_chair_generation,
          new_bridge_generation, evidence_digest, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
      `).run(
        handle.custodyId,
        handle.intent.projectSessionId,
        handle.intent.coordinationRunId,
        handle.intent.predecessorAgentId,
        handle.intent.successorAgentId,
        handle.promotionActionId,
        newChairGeneration,
        newBridgeGeneration,
        evidenceDigest,
        now,
      );
      const action = this.#database.prepare(`
        UPDATE provider_actions SET status='terminal', effect_count=1, idempotency_proven=1,
               history_json=CASE status WHEN 'ambiguous'
                 THEN '["prepared","dispatched","ambiguous","terminal"]'
                 ELSE '["prepared","dispatched","terminal"]' END,
               result_json=?, journal_revision=journal_revision+1, updated_at=?
         WHERE adapter_id=? AND action_id=? AND status IN ('dispatched','ambiguous')
      `).run(
        canonicalJson(resolution),
        now,
        handle.intent.providerAdapterId,
        handle.promotionActionId,
      );
      if (action.changes !== 1) stale("chair live handoff promotion action changed during commit");
      const result = { status: "committed" as const, custodyId: handle.custodyId, evidenceDigest };
      const terminal = this.#database.prepare(`
        UPDATE chair_live_handoff_custody
           SET state='terminal', result_json=?, revision=revision+1, updated_at=?
         WHERE custody_id=? AND state='committing'
      `).run(canonicalJson(result), now, handle.custodyId);
      if (terminal.changes !== 1) stale("chair live handoff terminal state changed");
      this.#database.prepare(`
        DELETE FROM delivery_freezes WHERE run_id=? AND agent_id IN (?,?) AND reason=?
      `).run(
        handle.intent.coordinationRunId,
        handle.intent.predecessorAgentId,
        handle.intent.successorAgentId,
        `chair-live-handoff:${handle.custodyId}`,
      );
      this.#fault("chair-live-handoff:terminal");
      return result;
    })();
    try {
      this.#retireVolatileChairBridge?.({
        projectSessionId: handle.intent.projectSessionId,
        runId: handle.intent.coordinationRunId,
        agentId: handle.intent.predecessorAgentId,
        principalGeneration: handle.intent.expectedPredecessorPrincipalGeneration,
        adapterId: text(predecessorBridge, "provider_adapter_id"),
        actionId: text(predecessorBridge, "provider_action_id"),
        providerSessionRef: text(predecessorBridge, "provider_session_ref"),
        providerSessionGeneration: integer(predecessorBridge, "provider_session_generation"),
        bridgeGeneration: handle.intent.expectedChairBridgeGeneration,
      });
    } catch { /* durable successor fencing already committed */ }
    return evidence;
  }
}
