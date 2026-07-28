import { createHash } from "node:crypto";
import type { ChairBridgeRecoveryIntent, Sha256Digest } from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import { ProjectFabricCoreError, type AuthenticatedOperatorContext } from "./contracts.js";
import { DIGEST, jsonEvidenceDigest } from "./provider-agent-custody.js";
import { retireProjectSessionBridges } from "./bridge-retirement.js";
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
import {
  ChairRecoverySettlement,
  type ChairRecoverySettlementAdapterEffectsPort,
} from "./chair-recovery-settlement.js";
import { ChairRecoveryRetainedBridgeRepository } from "./chair-recovery-retained-bridge-repository.js";
import type {
  LaunchUsageReconciliationInput,
  LaunchUsageReconciliationResult,
} from "./launch-usage-reconciliation.js";
import type { ChairBridgeLossObservation } from "./launch-custody.js";

type Digest = Sha256Digest;

function stale(message: string): never {
  throw new ProjectFabricCoreError("STALE_REVISION", message);
}

export type ChairRecoveryIntentPath = ChairBridgeRecoveryIntent["path"];

export type ChairRecoveryInspection = Readonly<{
  intent: ChairBridgeRecoveryIntent;
  inspectionDigest: Digest;
}>;

export type ChairRecoveryDispatchHandle = Readonly<{
  schemaVersion: 1;
  recoveryId: string;
  intent: ChairBridgeRecoveryIntent;
  intentDigest: Digest;
  inspectionDigest: Digest;
  operatorId: string;
  operatorCommandId: string;
  capability?: string;
  attestationChallenge?: string;
  socketPath?: string;
}>;

export type ChairRecoveryCommit = Readonly<{
  status: "committed" | "ambiguous" | "pending" | "no-effect";
  recoveryId: string;
  path: ChairRecoveryIntentPath;
  evidenceDigest: Digest;
}>;

export type ChairRecoveryCurrentState = Readonly<{
  revision: number;
  inspectionDigest: Digest;
}>;

export type ChairRecoveryAdapterEffectsPort = ChairRecoverySettlementAdapterEffectsPort & Readonly<{
  recoverChair?(handle: ChairRecoveryDispatchHandle): Promise<unknown>;
  lookupRetainedSuccessorBridge?(input: Readonly<{
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
  }>): Promise<"child" | "chair" | "missing">;
  promoteRetainedSuccessorBridge?(input: Readonly<{
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
  }>): Promise<boolean>;
}>;

export type ChairRecoveryCustodyServiceOptions = Readonly<{
  database: Database.Database;
  providerActionAdmission: ProviderActionAdmissionCoordinator;
  clock: () => number;
  fault: (label: string) => void;
  randomCapability: () => string;
  randomAttestationChallenge: () => string;
  fabricSocketPath: string;
  adapterContracts: { inspect(adapterId: string): Promise<unknown> };
  adapterEffects: ChairRecoveryAdapterEffectsPort;
  daemonInstanceGeneration: () => number;
  retireVolatileProjectSession?: (projectSessionId: string) => void;
  reconcileUnknownLaunchUsage: (input: LaunchUsageReconciliationInput) => LaunchUsageReconciliationResult;
}>;

/**
 * Byte-moved from `LaunchCustodyService`'s chair-recovery family (S4d, plan §2): the public
 * abandon/rebind/takeover workflow — inspect, preflight, prepare-in-transaction, dispatch,
 * status and reconcile. Delegates inspection binding, successor binding, commit and
 * startup-recovery/audit logic to `ChairRecoverySettlement`, and retained-bridge-loss
 * persistence to `ChairRecoveryRetainedBridgeRepository`. The launch-usage reconciliation used by
 * the abandon path is an explicitly injected in-transaction port
 * (`reconcileUnknownLaunchUsage`), never a direct import of launch/session-usage logic, so the
 * "usage settlement before cancellation in the same transaction" invariant holds without an
 * import cycle back into `launch-custody.ts`.
 *
 * Preserves: rebind custody/capability/action preparation inside the operator transaction;
 * usage settlement before cancellation in the same transaction (`dispatchPreparedChairRecovery`'s
 * abandon branch); `dispatched` persisted before adapter I/O for rebind/takeover
 * (`#dispatchChairRebind`/`#dispatchChairTakeover`); authority change in exactly one transaction
 * (`ChairRecoverySettlement#commitActiveChairRecovery`); loss evidence inserted before all fences
 * (`ChairRecoveryRetainedBridgeRepository#persistChairBridgeLoss`); and continue-after-error
 * sibling auditing (`ChairRecoverySettlement#auditRetainedChairBridges`).
 */
export class ChairRecoveryCustodyService {
  readonly #database: Database.Database;
  readonly #providerActionAdmission: ProviderActionAdmissionCoordinator;
  readonly #clock: () => number;
  readonly #fault: (label: string) => void;
  readonly #randomCapability: () => string;
  readonly #randomAttestationChallenge: () => string;
  readonly #fabricSocketPath: string;
  readonly #adapterContracts: ChairRecoveryCustodyServiceOptions["adapterContracts"];
  readonly #adapterEffects: ChairRecoveryAdapterEffectsPort;
  readonly #daemonInstanceGeneration: () => number;
  readonly #retireVolatileProjectSession: ((projectSessionId: string) => void) | undefined;
  readonly #reconcileUnknownLaunchUsage: ChairRecoveryCustodyServiceOptions["reconcileUnknownLaunchUsage"];
  readonly #repository: ChairRecoveryRetainedBridgeRepository;
  readonly #settlement: ChairRecoverySettlement;

  constructor(options: ChairRecoveryCustodyServiceOptions) {
    this.#database = options.database;
    this.#providerActionAdmission = options.providerActionAdmission;
    this.#clock = options.clock;
    this.#fault = options.fault;
    this.#randomCapability = options.randomCapability;
    this.#randomAttestationChallenge = options.randomAttestationChallenge;
    this.#fabricSocketPath = options.fabricSocketPath;
    this.#adapterContracts = options.adapterContracts;
    this.#adapterEffects = options.adapterEffects;
    this.#daemonInstanceGeneration = options.daemonInstanceGeneration;
    this.#retireVolatileProjectSession = options.retireVolatileProjectSession;
    this.#reconcileUnknownLaunchUsage = options.reconcileUnknownLaunchUsage;
    this.#repository = new ChairRecoveryRetainedBridgeRepository({
      database: this.#database,
      clock: this.#clock,
      daemonInstanceGeneration: this.#daemonInstanceGeneration,
    });
    this.#settlement = new ChairRecoverySettlement({
      database: this.#database,
      clock: this.#clock,
      fault: this.#fault,
      adapterEffects: this.#adapterEffects,
      repository: this.#repository,
    });
  }

  observeChairBridgeLoss(input: ChairBridgeLossObservation): boolean {
    return this.#database.transaction(() => this.#repository.persistChairBridgeLoss(input))();
  }

  async inspectChairRecovery(intent: ChairBridgeRecoveryIntent): Promise<ChairRecoveryInspection> {
    const inspectionDigest = this.#settlement.chairRecoveryInspectionDigest(intent);
    if (intent.path === "rebind") {
      const contract = await this.#adapterContracts.inspect(intent.providerAdapterId);
      if (`sha256:${sha256(canonicalJson(contract))}` !== intent.providerContractDigest) {
        throw new ProjectFabricCoreError("STALE_REVISION", "chair recovery provider contract changed");
      }
    }
    return { intent, inspectionDigest };
  }

  preflightChairRecovery(input: Readonly<{
    inspection: ChairRecoveryInspection;
    principal: AuthenticatedOperatorContext;
  }>): ProviderActionTicket | null {
    const { intent } = input.inspection;
    if (intent.path !== "rebind") return null;
    const recoveryAction = row(this.#database.prepare(`
      SELECT coordination_run_id,provider_adapter_id
        FROM chair_bridge_losses WHERE loss_id=?
    `).get(intent.lossId), "chair recovery provider action");
    return this.#providerActionAdmission.preflight({
      actionRef: {
        adapterId: text(recoveryAction, "provider_adapter_id"),
        actionId: intent.providerActionId,
      },
      scope: {
        kind: "run-action",
        runId: text(recoveryAction, "coordination_run_id"),
      },
      principal: input.principal,
      canonicalInput: { schemaVersion: 1, operation: "recover-chair", intent },
    });
  }

  async readChairRecoveryCurrentState(intent: ChairBridgeRecoveryIntent): Promise<ChairRecoveryCurrentState> {
    return {
      revision: intent.expectedBridgeRevision,
      inspectionDigest: this.#settlement.chairRecoveryInspectionDigest(intent),
    };
  }

  prepareChairRecoveryInTransaction(input: Readonly<{
    inspection: ChairRecoveryInspection;
    operatorId: string;
    operatorCommandId: string;
    providerActionTicket: ProviderActionTicket | null;
  }>): ChairRecoveryDispatchHandle {
    if (!this.#database.inTransaction) throw new Error("chair recovery preparation requires a transaction");
    const currentDigest = this.#settlement.chairRecoveryInspectionDigest(input.inspection.intent);
    if (currentDigest !== input.inspection.inspectionDigest) {
      throw new ProjectFabricCoreError("STALE_REVISION", "chair recovery state changed after inspection");
    }
    const intent = input.inspection.intent;
    const providerActionTicket = input.providerActionTicket;
    if (intent.path === "rebind" && providerActionTicket === null) {
      throw new Error("chair recovery provider action ticket is unavailable");
    }
    const intentJson = canonicalJson(intent);
    const intentDigest = jsonEvidenceDigest(intent);
    const recoveryId = `chair-bridge-recovery:${sha256(canonicalJson({
      lossId: intent.lossId,
      operatorId: input.operatorId,
      commandId: input.operatorCommandId,
    }))}`;
    const existing = this.#database.prepare(`
      SELECT intent_digest FROM chair_bridge_recovery_custody
       WHERE operator_id=? AND operator_command_id=?
    `).get(input.operatorId, input.operatorCommandId);
    if (isRow(existing)) {
      if (text(existing, "intent_digest") !== intentDigest) {
        throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "chair recovery command changed");
      }
      throw new ProjectFabricCoreError("CONFLICT", "chair recovery command is already prepared");
    }
    const openRecovery = this.#database.prepare(`
      SELECT recovery_id FROM chair_bridge_recovery_custody
       WHERE loss_id=? AND state NOT IN ('terminal','no-effect')
       LIMIT 1
    `).get(intent.lossId);
    if (isRow(openRecovery)) {
      throw new ProjectFabricCoreError("CONFLICT", "chair loss already has an open recovery custody");
    }
    const now = this.#clock();
    const loss = row(this.#database.prepare(`
      SELECT * FROM chair_bridge_losses WHERE loss_id=?
    `).get(intent.lossId), "chair recovery loss");
    let capability: string | undefined;
    let attestationChallenge: string | undefined;
    let providerActionId: string | null = null;
    let successorAgentId: string | null = null;
    let successorPrincipalGeneration: number | null = null;
    let successorBridgeGeneration: number | null = null;
    let successorRevision: number | null = null;
    let newChairAgentId: string | null = null;
    let newProviderActionId: string | null = null;
    let newProviderSessionRef: string | null = null;
    let newProviderSessionGeneration: number | null = null;
    let newPrincipalGeneration: number | null = null;
    let newBridgeGeneration: number | null = null;
    let newCapabilityHash: string | null = null;
    let newActivationEvidenceDigest: string | null = null;
    let attestationChallengeDigest: string | null = null;
    if (intent.path === "rebind") {
      capability = this.#randomCapability();
      attestationChallenge = this.#randomAttestationChallenge();
      if (capability.length === 0 || !/^[0-9a-f]{64}$/u.test(attestationChallenge)) {
        throw new Error("chair recovery private material is invalid");
      }
      providerActionId = intent.providerActionId;
      newChairAgentId = text(loss, "chair_agent_id");
      newProviderActionId = intent.providerActionId;
      newProviderSessionRef = text(loss, "provider_session_ref");
      newProviderSessionGeneration = intent.expectedProviderSessionGeneration + 1;
      newPrincipalGeneration = intent.expectedPrincipalGeneration + 1;
      newBridgeGeneration = intent.expectedLostBridgeGeneration + 1;
      newCapabilityHash = sha256(capability);
      attestationChallengeDigest = `sha256:${createHash("sha256").update(Buffer.from(attestationChallenge, "hex")).digest("hex")}`;
    } else if (intent.path === "takeover") {
      const successor = this.#settlement.chairRecoverySuccessor(intent);
      successorAgentId = intent.successorAgentId;
      successorPrincipalGeneration = intent.expectedSuccessorPrincipalGeneration;
      successorBridgeGeneration = intent.expectedSuccessorBridgeGeneration;
      successorRevision = intent.expectedSuccessorRevision;
      newChairAgentId = intent.successorAgentId;
      newProviderActionId = text(successor, "action_id");
      newProviderSessionRef = text(successor, "provider_session_ref");
      newProviderSessionGeneration = integer(successor, "provider_session_generation");
      newPrincipalGeneration = intent.expectedSuccessorPrincipalGeneration;
      newBridgeGeneration = intent.expectedLostBridgeGeneration + 1;
      newCapabilityHash = text(successor, "capability_hash");
      newActivationEvidenceDigest = text(successor, "activation_evidence_digest");
    }
    this.#database.prepare(`
      INSERT INTO chair_bridge_recovery_custody(
        recovery_id, loss_id, operator_id, operator_command_id, path,
        intent_digest, intent_json, recovery_manifest_digest,
        expected_session_revision, expected_session_generation, expected_run_revision,
        expected_chair_generation, expected_principal_generation, expected_bridge_revision,
        expected_lost_bridge_generation, expected_provider_session_generation,
        provider_adapter_id, provider_contract_digest, provider_action_id,
        successor_agent_id, expected_successor_principal_generation,
        expected_successor_bridge_generation, expected_successor_revision,
        new_chair_agent_id, new_provider_action_id, new_provider_session_ref,
        new_provider_session_generation, new_principal_generation, new_bridge_generation,
        new_capability_hash, new_activation_evidence_digest, attestation_challenge_digest,
        state, result_json, revision, created_at, updated_at
      ) VALUES (
        @recoveryId, @lossId, @operatorId, @operatorCommandId, @path,
        @intentDigest, @intentJson, @recoveryManifestDigest,
        @expectedSessionRevision, @expectedSessionGeneration, @expectedRunRevision,
        @expectedChairGeneration, @expectedPrincipalGeneration, @expectedBridgeRevision,
        @expectedLostBridgeGeneration, @expectedProviderSessionGeneration,
        @providerAdapterId, @providerContractDigest, @providerActionId,
        @successorAgentId, @successorPrincipalGeneration, @successorBridgeGeneration, @successorRevision,
        @newChairAgentId, @newProviderActionId, @newProviderSessionRef,
        @newProviderSessionGeneration, @newPrincipalGeneration, @newBridgeGeneration,
        @newCapabilityHash, @newActivationEvidenceDigest, @attestationChallengeDigest,
        'prepared', NULL, 1, @now, @now
      )
    `).run({
      recoveryId,
      lossId: intent.lossId,
      operatorId: input.operatorId,
      operatorCommandId: input.operatorCommandId,
      path: intent.path,
      intentDigest,
      intentJson,
      recoveryManifestDigest: intent.recoveryManifestDigest,
      expectedSessionRevision: intent.expectedSessionRevision,
      expectedSessionGeneration: intent.expectedSessionGeneration,
      expectedRunRevision: intent.expectedRunRevision,
      expectedChairGeneration: intent.expectedChairGeneration,
      expectedPrincipalGeneration: intent.expectedPrincipalGeneration,
      expectedBridgeRevision: intent.expectedBridgeRevision,
      expectedLostBridgeGeneration: intent.expectedLostBridgeGeneration,
      expectedProviderSessionGeneration: intent.expectedProviderSessionGeneration,
      providerAdapterId: intent.providerAdapterId,
      providerContractDigest: intent.providerContractDigest,
      providerActionId,
      successorAgentId,
      successorPrincipalGeneration,
      successorBridgeGeneration,
      successorRevision,
      newChairAgentId,
      newProviderActionId,
      newProviderSessionRef,
      newProviderSessionGeneration,
      newPrincipalGeneration,
      newBridgeGeneration,
      newCapabilityHash,
      newActivationEvidenceDigest,
      attestationChallengeDigest,
      now,
    });
    if (intent.path === "rebind") {
      this.#database.prepare(`
        INSERT INTO capabilities(token_hash, run_id, agent_id, principal_generation, expires_at)
        SELECT ?, coordination_run_id, chair_agent_id, ?, ?
          FROM chair_bridge_losses WHERE loss_id=?
      `).run(
        newCapabilityHash,
        newPrincipalGeneration,
        now + 24 * 60 * 60_000,
        intent.lossId,
      );
      const publicPayload = {
        schemaVersion: 1,
        recoveryId,
        lossId: intent.lossId,
        providerSessionRef: newProviderSessionRef,
        expectedProviderSessionGeneration: intent.expectedProviderSessionGeneration,
        nextProviderSessionGeneration: newProviderSessionGeneration,
        bridgeGeneration: newBridgeGeneration,
        providerContractDigest: intent.providerContractDigest,
      };
      const payloadJson = canonicalJson(publicPayload);
      const recoveryAction = row(this.#database.prepare(`
        SELECT coordination_run_id,provider_adapter_id,chair_agent_id,project_session_id
          FROM chair_bridge_losses WHERE loss_id=?
      `).get(intent.lossId), "chair recovery provider action");
      const recoveryRunId = text(recoveryAction, "coordination_run_id");
      const recoveryAdapterId = text(recoveryAction, "provider_adapter_id");
      this.#providerActionAdmission.admitUnroutedInCurrentTransaction(providerActionTicket as ProviderActionTicket, {
        runId: recoveryRunId,
        actionId: intent.providerActionId,
        adapterId: recoveryAdapterId,
        operation: "recover-chair",
        targetAgentId: text(recoveryAction, "chair_agent_id"),
        identityHash: sha256(canonicalJson({ recoveryId, actionId: intent.providerActionId })),
        payloadHash: sha256(payloadJson),
        payloadJson,
        status: "prepared",
        historyJson: '["prepared"]',
        executionCount: 0,
        updatedAt: now,
      }, "chair_recovery");
    }
    this.#fault("chair-recovery:prepare:custody");
    return {
      schemaVersion: 1,
      recoveryId,
      intent,
      intentDigest,
      inspectionDigest: input.inspection.inspectionDigest,
      operatorId: input.operatorId,
      operatorCommandId: input.operatorCommandId,
      ...(capability === undefined ? {} : { capability }),
      ...(attestationChallenge === undefined ? {} : { attestationChallenge }),
      ...(capability === undefined ? {} : { socketPath: this.#fabricSocketPath }),
    };
  }

  async dispatchPreparedChairRecovery(handle: ChairRecoveryDispatchHandle): Promise<ChairRecoveryCommit> {
    const custody = row(this.#database.prepare(`
      SELECT * FROM chair_bridge_recovery_custody WHERE recovery_id=?
    `).get(handle.recoveryId), "chair recovery custody");
    if (
      text(custody, "operator_id") !== handle.operatorId ||
      text(custody, "operator_command_id") !== handle.operatorCommandId ||
      text(custody, "intent_digest") !== handle.intentDigest
    ) throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "chair recovery handle changed");
    if (text(custody, "state") === "terminal") {
      const stored: unknown = JSON.parse(text(custody, "result_json"));
      if (!isRow(stored) || stored.status !== "committed") throw new Error("terminal chair recovery result is invalid");
      if (handle.intent.path === "abandon") {
        try { this.#retireVolatileProjectSession?.(handle.intent.projectSessionId); } catch { /* durable fencing exists */ }
      }
      return stored as ChairRecoveryCommit;
    }
    if (text(custody, "state") !== "prepared") {
      throw new ProjectFabricCoreError("CONFLICT", "chair recovery is not prepared");
    }
    if (handle.intent.path === "rebind") {
      assertProviderActionOwner(this.#database, {
        runId: handle.intent.coordinationRunId,
        adapterId: handle.intent.providerAdapterId,
        actionId: handle.intent.providerActionId,
      }, "chair_recovery");
      return await this.#dispatchChairRebind({ ...handle, intent: handle.intent });
    }
    if (handle.intent.path === "takeover") {
      return await this.#dispatchChairTakeover({ ...handle, intent: handle.intent });
    }
    const abandonIntent = handle.intent;
    const now = this.#clock();
    const result = this.#database.transaction((): ChairRecoveryCommit => {
      this.#assertChairAbandonReady(abandonIntent);
      this.#reconcileKnownLaunchUsageForAbandon(abandonIntent);
      const changed = this.#database.prepare(`
        UPDATE chair_bridge_recovery_custody
           SET state='committing', revision=revision+1, updated_at=?
         WHERE recovery_id=? AND state='prepared' AND revision=1
      `).run(now, handle.recoveryId);
      if (changed.changes !== 1) stale("chair recovery custody changed before commit");
      this.#fault("chair-recovery:abandon:committing");
      const bridge = row(this.#database.prepare(`
        SELECT revision FROM launched_chair_bridge_state
         WHERE project_session_id=? AND coordination_run_id=? AND state='lost'
      `).get(handle.intent.projectSessionId, handle.intent.coordinationRunId), "lost chair bridge");
      if (integer(bridge, "revision") !== handle.intent.expectedBridgeRevision) {
        stale("lost chair bridge revision changed");
      }
      const abandonedBridge = this.#database.prepare(`
        UPDATE launched_chair_bridge_state
           SET state='abandoned', revision=revision+1, updated_at=?
         WHERE project_session_id=? AND coordination_run_id=? AND state='lost' AND revision=?
      `).run(
        now,
        abandonIntent.projectSessionId,
        abandonIntent.coordinationRunId,
        abandonIntent.expectedBridgeRevision,
      );
      if (abandonedBridge.changes !== 1) stale("lost chair bridge changed before abandon");
      const resolution = {
        schemaVersion: 1,
        recoveryId: handle.recoveryId,
        lossId: abandonIntent.lossId,
        path: "abandon" as const,
        reason: abandonIntent.reason,
        previousBridgeGeneration: abandonIntent.expectedLostBridgeGeneration,
      };
      const evidenceDigest = jsonEvidenceDigest(resolution);
      this.#database.prepare(`
        INSERT INTO chair_bridge_loss_resolutions(
          loss_id, recovery_id, path, successor_agent_id,
          new_principal_generation, new_bridge_generation, evidence_digest, created_at
        ) VALUES (?, ?, 'abandon', NULL, NULL, NULL, ?, ?)
      `).run(abandonIntent.lossId, handle.recoveryId, evidenceDigest, now);
      const revokedLease = this.#database.prepare(`
        UPDATE run_chair_leases SET status='revoked', updated_at=?
         WHERE project_session_id=? AND run_id=? AND status='frozen'
      `).run(now, abandonIntent.projectSessionId, abandonIntent.coordinationRunId);
      if (revokedLease.changes !== 1) stale("frozen chair lease changed before abandon");
      const terminalPath = canonicalJson({ kind: "cancelled", reason: abandonIntent.reason });
      const cancelledRun = this.#database.prepare(`
        UPDATE runs SET lifecycle_state='cancelled', revision=revision+1
         WHERE run_id=? AND lifecycle_state='recovery_required' AND revision=?
      `).run(abandonIntent.coordinationRunId, abandonIntent.expectedRunRevision);
      if (cancelledRun.changes !== 1) stale("run recovery revision changed before abandon");
      const abandonmentReason = `chair-recovery-abandon:${handle.recoveryId}`;
      const memberships = this.#database.prepare(`
        UPDATE project_session_memberships
           SET state='abandoned',abandoned_reason=?,revision=revision+1,updated_at=?
         WHERE project_session_id=? AND coordination_run_id=? AND required=1 AND state='active'
           AND (
             (member_kind='coordination-run' AND member_id=coordination_run_id) OR
             (member_kind='lease' AND member_id=(
               SELECT chair_lease_id FROM runs WHERE run_id=coordination_run_id
             ))
           )
      `).run(
        abandonmentReason,
        now,
        abandonIntent.projectSessionId,
        abandonIntent.coordinationRunId,
      );
      if (memberships.changes !== 2) stale("chair abandon membership set changed");
      this.#database.prepare(`
        UPDATE capabilities SET revoked_at=COALESCE(revoked_at,?)
         WHERE run_id=?
      `).run(now, abandonIntent.coordinationRunId);
      this.#database.prepare(`
        UPDATE agents SET lifecycle='archived' WHERE run_id=?
      `).run(abandonIntent.coordinationRunId);
      const cancelledSession = this.#database.prepare(`
        UPDATE project_sessions
           SET state='cancelled',membership_revision=membership_revision+1,
               revision=revision+1,terminal_path_json=?,updated_at=?
         WHERE project_session_id=? AND state='recovery_required' AND revision=? AND generation=?
      `).run(
        terminalPath,
        now,
        abandonIntent.projectSessionId,
        abandonIntent.expectedSessionRevision,
        abandonIntent.expectedSessionGeneration,
      );
      if (cancelledSession.changes !== 1) stale("project session recovery revision changed before abandon");
      retireProjectSessionBridges(this.#database, {
        projectSessionId: abandonIntent.projectSessionId,
        sourceKind: "chair-recovery-abandon",
        terminalKind: "cancelled",
        terminalRef: terminalPath,
        ownerOperatorId: handle.operatorId,
        ownerRef: handle.recoveryId,
        now,
      });
      this.#fault("chair-recovery:abandon:after-bridges");
      const commit: ChairRecoveryCommit = {
        status: "committed",
        recoveryId: handle.recoveryId,
        path: "abandon",
        evidenceDigest,
      };
      const terminal = this.#database.prepare(`
        UPDATE chair_bridge_recovery_custody
           SET state='terminal', result_json=?, revision=revision+1, updated_at=?
         WHERE recovery_id=? AND state='committing'
      `).run(canonicalJson(commit), now, handle.recoveryId);
      if (terminal.changes !== 1) stale("chair abandon custody changed before terminal commit");
      return commit;
    })();
    try { this.#retireVolatileProjectSession?.(abandonIntent.projectSessionId); } catch { /* durable fencing already committed */ }
    return result;
  }

  #reconcileKnownLaunchUsageForAbandon(
    intent: Extract<ChairBridgeRecoveryIntent, { path: "abandon" }>,
  ): void {
    const binding = this.#database.prepare(`
      SELECT session.project_id, custody.provider_adapter_id, custody.provider_action_id,
             custody.reservation_id, reservation.revision
        FROM project_session_launch_custody custody
        JOIN project_sessions session ON session.project_session_id=custody.project_session_id
        JOIN resource_reservations reservation ON reservation.reservation_id=custody.reservation_id
       WHERE custody.project_session_id=? AND custody.coordination_run_id=?
    `).get(intent.projectSessionId, intent.coordinationRunId);
    if (!isRow(binding)) return;
    const unknownUnits = this.#database.prepare(`
      SELECT DISTINCT unit_key FROM resource_reservation_dimensions
       WHERE reservation_id=? AND usage_unknown=1 ORDER BY unit_key
    `).all(text(binding, "reservation_id")).map((value) => text(row(value, "unknown launch unit"), "unit_key"));
    const observedUsage: Record<string, number> = {};
    if (unknownUnits.includes("provider_calls")) observedUsage.provider_calls = 1;
    if (unknownUnits.includes("concurrent_turns")) observedUsage.concurrent_turns = 0;
    if (Object.keys(observedUsage).length === 0) return;
    this.#reconcileUnknownLaunchUsage({
      projectId: text(binding, "project_id"),
      projectSessionId: intent.projectSessionId,
      coordinationRunId: intent.coordinationRunId,
      providerAdapterId: text(binding, "provider_adapter_id"),
      providerActionId: text(binding, "provider_action_id"),
      reservationId: text(binding, "reservation_id"),
      expectedReservationRevision: integer(binding, "revision"),
      observedUsage,
      evidenceDigest: jsonEvidenceDigest({
        kind: "deterministic-launch-provider-call",
        lossId: intent.lossId,
        recoveryManifestDigest: intent.recoveryManifestDigest,
        providerAdapterId: text(binding, "provider_adapter_id"),
        providerActionId: text(binding, "provider_action_id"),
      }),
    });
  }

  #assertChairAbandonReady(intent: Extract<ChairBridgeRecoveryIntent, { path: "abandon" }>): void {
    const current = row(this.#database.prepare(`
      SELECT chair_lease_id FROM runs
       WHERE project_session_id=? AND run_id=? AND lifecycle_state='recovery_required'
    `).get(intent.projectSessionId, intent.coordinationRunId), "chair abandon run");
    const currentLeaseId = text(current, "chair_lease_id");
    const allowedMemberships = this.#database.prepare(`
      SELECT member_kind,member_id FROM project_session_memberships
       WHERE project_session_id=? AND coordination_run_id=? AND required=1 AND state='active'
         AND NOT (
           (member_kind='coordination-run' AND member_id=coordination_run_id) OR
           (member_kind='lease' AND member_id=?)
         )
       LIMIT 1
    `).get(intent.projectSessionId, intent.coordinationRunId, currentLeaseId);
    if (allowedMemberships !== undefined) {
      throw new ProjectFabricCoreError("BARRIER_PRECONDITION_FAILED", "unrelated active membership blocks chair abandon");
    }
    const exactTerminalMemberships = this.#database.prepare(`
      SELECT COUNT(*) AS count FROM project_session_memberships
       WHERE project_session_id=? AND coordination_run_id=? AND required=1 AND state='active'
         AND (
           (member_kind='coordination-run' AND member_id=coordination_run_id) OR
           (member_kind='lease' AND member_id=?)
         )
    `).get(intent.projectSessionId, intent.coordinationRunId, currentLeaseId);
    if (integer(row(exactTerminalMemberships, "chair abandon membership set"), "count") !== 2) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "chair abandon requires exact run and current-chair membership");
    }
    const blockers: ReadonlyArray<readonly [string, string]> = [
      ["task", `SELECT 1 FROM tasks WHERE run_id=? AND state NOT IN ('complete','cancelled','degraded') LIMIT 1`],
      ["workstream", `SELECT 1 FROM workstreams WHERE coordination_run_id=? AND state NOT IN ('complete','cancelled','degraded','abandoned') LIMIT 1`],
      ["write lease", `SELECT 1 FROM leases WHERE run_id=? AND status IN ('active','quarantined') LIMIT 1`],
      ["task-owner lease", `SELECT 1 FROM task_owner_leases WHERE run_id=? AND status IN ('active','frozen') LIMIT 1`],
      ["provider action", `SELECT 1 FROM provider_actions WHERE run_id=? AND status IN ('prepared','dispatched','accepted','ambiguous','quarantined') LIMIT 1`],
      ["required message", `SELECT 1 FROM deliveries delivery
        JOIN messages message ON message.message_id=delivery.message_id AND message.run_id=delivery.run_id
        WHERE delivery.run_id=? AND message.requires_ack=1
          AND delivery.state NOT IN ('acknowledged','abandoned','expired') LIMIT 1`],
      ["required result", `SELECT 1 FROM result_deliveries WHERE run_id=? AND required=1 AND state NOT IN ('consumed','abandoned') LIMIT 1`],
      ["gate", `SELECT 1 FROM scoped_gates WHERE coordination_run_id=? AND status IN ('pending','deferred') LIMIT 1`],
      ["barrier", `SELECT 1 FROM barriers WHERE run_id=? AND state<>'closed' LIMIT 1`],
      ["child bridge", `SELECT 1 FROM agent_bridge_state WHERE run_id=? AND bridge_state IN ('pending','lost') LIMIT 1`],
      ["resource reservation", `SELECT 1 FROM resource_reservations WHERE coordination_run_id=? AND state IN ('reserved','partially-consumed','ambiguous') LIMIT 1`],
    ];
    for (const [label, sql] of blockers) {
      if (this.#database.prepare(sql).get(intent.coordinationRunId) !== undefined) {
        throw new ProjectFabricCoreError("BARRIER_PRECONDITION_FAILED", `${label} remains unresolved before chair abandon`);
      }
    }
    if (this.#database.prepare(`
      SELECT 1 FROM operator_effect_custody
       WHERE project_session_id=? AND state IN ('prepared','dispatching','ambiguous','conflict','quarantined','failed')
       LIMIT 1
    `).get(intent.projectSessionId) !== undefined) {
      throw new ProjectFabricCoreError("BARRIER_PRECONDITION_FAILED", "operator effect remains unresolved before chair abandon");
    }
  }

  chairRecoveryStatus(operatorId: string, operatorCommandId: string): ChairRecoveryCommit {
    const custody = row(this.#database.prepare(`
      SELECT recovery_id, path, state, result_json FROM chair_bridge_recovery_custody
       WHERE operator_id=? AND operator_command_id=?
    `).get(operatorId, operatorCommandId), "chair recovery status");
    const state = text(custody, "state");
    if (state === "terminal") {
      const value: unknown = JSON.parse(text(custody, "result_json"));
      if (!isRow(value) || value.status !== "committed") throw new Error("terminal chair recovery status is invalid");
      return value as ChairRecoveryCommit;
    }
    const path = text(custody, "path") as ChairRecoveryIntentPath;
    const evidenceDigest = jsonEvidenceDigest({
      recoveryId: text(custody, "recovery_id"),
      path,
      state,
      result: custody.result_json,
    });
    if (state === "no-effect") {
      return { status: "no-effect", recoveryId: text(custody, "recovery_id"), path, evidenceDigest };
    }
    if (state === "ambiguous") {
      return { status: "ambiguous", recoveryId: text(custody, "recovery_id"), path, evidenceDigest };
    }
    return { status: "pending", recoveryId: text(custody, "recovery_id"), path, evidenceDigest };
  }

  async reconcileChairRecovery(operatorId: string, operatorCommandId: string): Promise<ChairRecoveryCommit> {
    const custody = row(this.#database.prepare(`
      SELECT custody.*,loss.coordination_run_id
        FROM chair_bridge_recovery_custody custody
        JOIN chair_bridge_losses loss ON loss.loss_id=custody.loss_id
       WHERE custody.operator_id=? AND custody.operator_command_id=?
    `).get(operatorId, operatorCommandId), "chair recovery reconciliation");
    if (text(custody, "path") === "rebind" && typeof custody.provider_action_id === "string") {
      assertProviderActionOwner(this.#database, {
        runId: text(custody, "coordination_run_id"),
        adapterId: text(custody, "provider_adapter_id"),
        actionId: custody.provider_action_id,
      }, "chair_recovery");
    }
    const status = this.chairRecoveryStatus(operatorId, operatorCommandId);
    if (status.status === "committed" || status.status === "no-effect") return status;
    const path = text(custody, "path");
    const custodyState = text(custody, "state");
    if (path === "takeover" && ["dispatched", "accepted", "ambiguous"].includes(custodyState)) {
      return await this.#reconcileChairTakeover(custody, operatorId, operatorCommandId, status);
    }
    if (
      path !== "rebind" || !["dispatched", "accepted", "ambiguous"].includes(custodyState) ||
      typeof custody.provider_action_id !== "string" || this.#adapterEffects.lookupChairRecovery === undefined
    ) return status;
    let record: unknown;
    try {
      record = await this.#adapterEffects.lookupChairRecovery({
        adapterId: text(custody, "provider_adapter_id"),
        actionId: custody.provider_action_id,
      });
    } catch {
      return status;
    }
    const provider = this.#settlement.chairRecoveryLookupResult(custody, record);
    if (provider === undefined) return status;
    const intentValue: unknown = JSON.parse(text(custody, "intent_json"));
    if (!isRow(intentValue) || intentValue.kind !== "chair-bridge-recovery" || intentValue.path !== "rebind") {
      throw new Error("stored chair recovery intent is invalid");
    }
    const intent = intentValue as ChairBridgeRecoveryIntent & { path: "rebind" };
    return this.#settlement.commitActiveChairRecovery({
      schemaVersion: 1,
      recoveryId: text(custody, "recovery_id"),
      intent,
      intentDigest: text(custody, "intent_digest") as Digest,
      inspectionDigest: jsonEvidenceDigest({ recovery: text(custody, "recovery_id") }),
      operatorId,
      operatorCommandId,
    }, jsonEvidenceDigest(provider.fabricContinuity));
  }

  /** Startup-recovery entry point; forwards to `ChairRecoverySettlement` with a bound takeover port. */
  async recoverChairRecoveryCustody(result: {
    lookedUp: number;
    activated: number;
    failed: number;
    ambiguous: number;
    recoveryRequired: number;
  }): Promise<void> {
    await this.#settlement.recoverChairRecoveryCustody(
      result,
      (operatorId, operatorCommandId) => this.reconcileChairRecovery(operatorId, operatorCommandId),
    );
  }

  /** Startup-recovery audit entry point; forwards to `ChairRecoverySettlement`. */
  auditRetainedChairBridges(result: { recoveryRequired: number; ambiguous: number }, errors: unknown[]): void {
    this.#settlement.auditRetainedChairBridges(result, errors);
  }

  async #dispatchChairRebind(
    handle: ChairRecoveryDispatchHandle & Readonly<{ intent: Extract<ChairBridgeRecoveryIntent, { path: "rebind" }> }>,
  ): Promise<ChairRecoveryCommit> {
    if (
      this.#adapterEffects.recoverChair === undefined ||
      handle.capability === undefined ||
      handle.attestationChallenge === undefined ||
      handle.socketPath === undefined
    ) throw new ProjectFabricCoreError("CAPABILITY_UNAVAILABLE", "chair rebind adapter custody is unavailable");
    const now = this.#clock();
    this.#database.transaction(() => {
      const recovery = this.#database.prepare(`
        UPDATE chair_bridge_recovery_custody
           SET state='dispatched', revision=revision+1, updated_at=?
         WHERE recovery_id=? AND state='prepared'
      `).run(now, handle.recoveryId);
      const action = this.#database.prepare(`
        UPDATE provider_actions
           SET status='dispatched', history_json='["prepared","dispatched"]',
               execution_count=1, journal_revision=journal_revision+1, updated_at=?
         WHERE adapter_id=? AND action_id=? AND status='prepared'
      `).run(now, handle.intent.providerAdapterId, handle.intent.providerActionId);
      if (recovery.changes !== 1 || action.changes !== 1) stale("chair rebind changed before dispatch");
      this.#fault("chair-recovery:rebind:dispatched");
    })();
    let raw: unknown;
    try {
      raw = await this.#adapterEffects.recoverChair(handle);
      this.#fault("chair-recovery:rebind:after-adapter");
    } catch (error: unknown) {
      return this.#markChairRecoveryAmbiguous(handle, error);
    }
    if (!isRow(raw)) return this.#markChairRecoveryAmbiguous(handle, "malformed rebind result");
    const expectedSessionRef = text(row(this.#database.prepare(`
      SELECT * FROM chair_bridge_losses WHERE loss_id=?
    `).get(handle.intent.lossId), "chair rebind loss"), "provider_session_ref");
    const expectedGeneration = handle.intent.expectedProviderSessionGeneration + 1;
    if (
      raw.schemaVersion !== 1 || raw.recoveryId !== handle.recoveryId ||
      raw.providerAdapterId !== handle.intent.providerAdapterId ||
      raw.providerActionId !== handle.intent.providerActionId ||
      raw.providerContractDigest !== handle.intent.providerContractDigest ||
      raw.providerSessionRef !== expectedSessionRef ||
      raw.providerSessionGeneration !== expectedGeneration ||
      typeof raw.activationEvidenceDigest !== "string" || !DIGEST.test(raw.activationEvidenceDigest)
    ) return this.#markChairRecoveryAmbiguous(handle, "rebind result binding changed");
    return this.#settlement.commitActiveChairRecovery(handle, raw.activationEvidenceDigest as Digest);
  }

  async #dispatchChairTakeover(
    handle: ChairRecoveryDispatchHandle & Readonly<{ intent: Extract<ChairBridgeRecoveryIntent, { path: "takeover" }> }>,
  ): Promise<ChairRecoveryCommit> {
    const promote = this.#adapterEffects.promoteRetainedSuccessorBridge;
    if (promote === undefined) {
      throw new ProjectFabricCoreError("CAPABILITY_UNAVAILABLE", "chair takeover bridge promotion is unavailable");
    }
    const successor = this.#settlement.chairRecoverySuccessor(handle.intent);
    const now = this.#clock();
    const dispatched = this.#database.prepare(`
      UPDATE chair_bridge_recovery_custody
         SET state='dispatched', revision=revision+1, updated_at=?
       WHERE recovery_id=? AND state='prepared'
    `).run(now, handle.recoveryId);
    if (dispatched.changes !== 1) stale("chair takeover changed before dispatch");
    this.#fault("chair-recovery:takeover:dispatched");
    const promotionInput = {
      projectSessionId: handle.intent.projectSessionId,
      runId: handle.intent.coordinationRunId,
      agentId: handle.intent.successorAgentId,
      principalGeneration: handle.intent.expectedSuccessorPrincipalGeneration,
      adapterId: text(successor, "adapter_id"),
      actionId: text(successor, "action_id"),
      providerSessionRef: text(successor, "provider_session_ref"),
      providerSessionGeneration: integer(successor, "provider_session_generation"),
      sourceBridgeGeneration: handle.intent.expectedSuccessorBridgeGeneration,
      chairBridgeGeneration: handle.intent.expectedLostBridgeGeneration + 1,
    } as const;
    let promoted = false;
    try {
      promoted = await promote(promotionInput);
      this.#fault("chair-recovery:takeover:after-adapter");
    } catch (error: unknown) {
      return this.#markChairRecoveryAmbiguous(handle, error);
    }
    if (!promoted) return await this.#settleUnobservedChairTakeover(handle, promotionInput);
    return this.#settlement.commitActiveChairRecovery(
      handle,
      text(successor, "activation_evidence_digest") as Digest,
    );
  }

  async #settleUnobservedChairTakeover(
    handle: ChairRecoveryDispatchHandle & Readonly<{ intent: Extract<ChairBridgeRecoveryIntent, { path: "takeover" }> }>,
    input: Parameters<NonNullable<ChairRecoveryAdapterEffectsPort["promoteRetainedSuccessorBridge"]>>[0],
  ): Promise<ChairRecoveryCommit> {
    const lookup = this.#adapterEffects.lookupRetainedSuccessorBridge;
    if (lookup === undefined) return this.#markChairRecoveryAmbiguous(handle, "successor bridge lookup unavailable");
    let observed: "child" | "chair" | "missing";
    try {
      observed = await lookup(input);
    } catch (error: unknown) {
      return this.#markChairRecoveryAmbiguous(handle, error);
    }
    if (observed === "chair") {
      const successor = this.#settlement.chairRecoverySuccessor(handle.intent);
      return this.#settlement.commitActiveChairRecovery(handle, text(successor, "activation_evidence_digest") as Digest);
    }
    if (observed === "child") return this.#markChairRecoveryNoEffect(handle, "successor remained a child");
    return this.#markChairRecoveryAmbiguous(handle, "successor bridge state is unobservable");
  }

  async #reconcileChairTakeover(
    custody: Row,
    operatorId: string,
    operatorCommandId: string,
    current: ChairRecoveryCommit,
  ): Promise<ChairRecoveryCommit> {
    const lookup = this.#adapterEffects.lookupRetainedSuccessorBridge;
    if (lookup === undefined) return current;
    const intentValue: unknown = JSON.parse(text(custody, "intent_json"));
    if (!isRow(intentValue) || intentValue.kind !== "chair-bridge-recovery" || intentValue.path !== "takeover") {
      throw new Error("stored chair takeover intent is invalid");
    }
    const intent = intentValue as ChairBridgeRecoveryIntent & { path: "takeover" };
    const successor = this.#settlement.chairRecoverySuccessor(intent);
    const handle: ChairRecoveryDispatchHandle & Readonly<{ intent: typeof intent }> = {
      schemaVersion: 1,
      recoveryId: text(custody, "recovery_id"),
      intent,
      intentDigest: text(custody, "intent_digest") as Digest,
      inspectionDigest: jsonEvidenceDigest({ recovery: text(custody, "recovery_id") }),
      operatorId,
      operatorCommandId,
    };
    return await this.#settleUnobservedChairTakeover(handle, {
      projectSessionId: intent.projectSessionId,
      runId: intent.coordinationRunId,
      agentId: intent.successorAgentId,
      principalGeneration: intent.expectedSuccessorPrincipalGeneration,
      adapterId: text(successor, "adapter_id"),
      actionId: text(successor, "action_id"),
      providerSessionRef: text(successor, "provider_session_ref"),
      providerSessionGeneration: integer(successor, "provider_session_generation"),
      sourceBridgeGeneration: intent.expectedSuccessorBridgeGeneration,
      chairBridgeGeneration: intent.expectedLostBridgeGeneration + 1,
    });
  }

  #markChairRecoveryNoEffect(handle: ChairRecoveryDispatchHandle, reason: string): ChairRecoveryCommit {
    const now = this.#clock();
    const evidenceDigest = jsonEvidenceDigest({
      recoveryId: handle.recoveryId,
      kind: "chair-recovery-proved-no-effect",
      reason,
    });
    const changed = this.#database.prepare(`
      UPDATE chair_bridge_recovery_custody
         SET state='no-effect', result_json=?, revision=revision+1, updated_at=?
       WHERE recovery_id=? AND state IN ('dispatched','accepted','ambiguous')
    `).run(canonicalJson({ status: "no-effect", evidenceDigest, reason }), now, handle.recoveryId);
    if (changed.changes !== 1) stale("chair recovery no-effect state changed");
    return { status: "no-effect", recoveryId: handle.recoveryId, path: handle.intent.path, evidenceDigest };
  }

  #markChairRecoveryAmbiguous(handle: ChairRecoveryDispatchHandle, evidence: unknown): ChairRecoveryCommit {
    if (handle.intent.path === "rebind") {
      assertProviderActionOwner(this.#database, {
        runId: handle.intent.coordinationRunId,
        adapterId: handle.intent.providerAdapterId,
        actionId: handle.intent.providerActionId,
      }, "chair_recovery");
    }
    const now = this.#clock();
    const evidenceDigest = jsonEvidenceDigest({
      recoveryId: handle.recoveryId,
      kind: "chair-recovery-ambiguous",
      evidence: evidence instanceof Error ? { name: evidence.name, message: evidence.message } : evidence,
    });
    this.#database.transaction(() => {
      this.#database.prepare(`
        UPDATE chair_bridge_recovery_custody
           SET state='ambiguous', result_json=?, revision=revision+1, updated_at=?
         WHERE recovery_id=? AND state IN ('dispatched','accepted','ambiguous')
      `).run(canonicalJson({ status: "ambiguous", evidenceDigest }), now, handle.recoveryId);
      if (handle.intent.path === "rebind") {
        this.#database.prepare(`
          UPDATE provider_actions
             SET status='ambiguous', history_json='["prepared","dispatched","ambiguous"]',
                 result_json=?, journal_revision=journal_revision+1, updated_at=?
           WHERE adapter_id=? AND action_id=? AND status IN ('dispatched','accepted','ambiguous')
        `).run(
          canonicalJson({ kind: "chair-recovery-ambiguous", evidenceDigest }),
          now,
          handle.intent.providerAdapterId,
          handle.intent.providerActionId,
        );
      }
    })();
    return { status: "ambiguous", recoveryId: handle.recoveryId, path: handle.intent.path, evidenceDigest };
  }
}
