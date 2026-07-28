import type { OperatorActionIntent, Sha256Digest } from "@local/agent-fabric-protocol";
import { parseSha256Digest } from "@local/agent-fabric-protocol";
import type Database from "better-sqlite3";

import type {
  OperatorLifecycleRecoveryCommit,
  OperatorLifecycleRecoveryCurrentState,
  OperatorLifecycleRecoveryCustodyPort,
  OperatorLifecycleRecoveryInspection,
} from "./action-store.js";
import { FabricError } from "../errors.js";
import { parseStoredAttestationDigests } from "../gates/attestation-binding.js";
import { GenerationLossRepository } from "../lifecycle/generation-loss-repository.js";
import { lifecycleDigest } from "../lifecycle/custody-codec.js";
import type { LifecycleIntegrityReceiptAuthorityPort, LifecycleDigest } from "../lifecycle/receipt-authority.js";
import type { LifecycleReceiptRepository } from "../lifecycle/receipt-repository.js";
import { recoverTerminalAuthorityReceipt } from "../lifecycle/terminal-receipt-authority.js";
import { ProjectFabricCoreError } from "../project-session/contracts.js";
import { canonicalJson, integer, isRow, row, sha256, text } from "../persistence/row-codec.js";

type LifecycleRecoveryIntent = Extract<OperatorActionIntent, { kind: "agent-lifecycle-recovery" }>;
type LifecycleAbandonIntent = Extract<LifecycleRecoveryIntent, { path: "abandon" }>;

export type LifecycleRecoveryCustodyServiceOptions = Readonly<{
  database: Database.Database;
  receipts: LifecycleReceiptRepository;
  authority: LifecycleIntegrityReceiptAuthorityPort;
  clock?: () => number;
}>;

const EMPTY_MUTATION_PLAN = Object.freeze({
  schemaVersion: 1 as const,
  writes: [] as const,
  writeSetDigest: lifecycleDigest("mutation-plan", { schemaVersion: 1, writes: [] }),
});

function prefixedDigest(value: string): string {
  return value.startsWith("sha256:") ? value : `sha256:${value}`;
}

/**
 * Thin operator custody adapter that terminalises a direct-open generation
 * loss through the existing lifecycle receipt pipeline. It owns no second
 * receipt pipeline: preparation, authority recovery and atomic apply all
 * delegate to the terminal-owner seam proven by the rotation custody owner.
 */
export class LifecycleRecoveryCustodyService implements OperatorLifecycleRecoveryCustodyPort {
  readonly #database: Database.Database;
  readonly #receipts: LifecycleReceiptRepository;
  readonly #authority: LifecycleIntegrityReceiptAuthorityPort;
  readonly #clock: () => number;

  constructor(options: LifecycleRecoveryCustodyServiceOptions) {
    this.#database = options.database;
    this.#receipts = options.receipts;
    this.#authority = options.authority;
    this.#clock = options.clock ?? Date.now;
  }

  async readLifecycleRecoveryCurrentState(
    intent: LifecycleRecoveryIntent,
  ): Promise<OperatorLifecycleRecoveryCurrentState> {
    return await Promise.resolve(this.#currentState(intent));
  }

  async inspectLifecycleRecovery(
    intent: LifecycleRecoveryIntent,
  ): Promise<OperatorLifecycleRecoveryInspection> {
    return await Promise.resolve({ intent, inspectionDigest: this.#inspectionDigest(intent) });
  }

  prepareLifecycleFreshRotateInTransaction(): OperatorLifecycleRecoveryCommit {
    throw new ProjectFabricCoreError(
      "CAPABILITY_FORBIDDEN",
      "lifecycle fresh-rotate custody is a later deferred slice",
    );
  }

  prepareLifecycleAbandonInTransaction(input: Readonly<{
    inspection: OperatorLifecycleRecoveryInspection;
    operatorId: string;
    operatorCommandId: string;
  }>): OperatorLifecycleRecoveryCommit {
    if (!this.#database.inTransaction) throw new Error("lifecycle abandon preparation requires a transaction");
    const intent = input.inspection.intent;
    if (intent.path !== "abandon") {
      throw new ProjectFabricCoreError("CONFLICT", "lifecycle abandon custody received another recovery path");
    }
    if (this.#inspectionDigest(intent) !== input.inspection.inspectionDigest) {
      throw new ProjectFabricCoreError("STALE_REVISION", "lifecycle recovery state changed after inspection");
    }
    const loss = this.#lossIdentity(intent);
    const intentDigest = lifecycleDigest("agent-lifecycle-recovery-intent", intent);
    const existing = this.#database.prepare(`
      SELECT intent_digest FROM agent_lifecycle_recovery_custody
       WHERE operator_id=? AND operator_command_id=?
    `).get(input.operatorId, input.operatorCommandId);
    if (isRow(existing)) {
      if (text(existing, "intent_digest") !== intentDigest) {
        throw new ProjectFabricCoreError("DEDUPE_CONFLICT", "lifecycle recovery command changed");
      }
      throw new ProjectFabricCoreError("CONFLICT", "lifecycle recovery command is already prepared");
    }
    const open = this.#database.prepare(`
      SELECT recovery_id FROM agent_lifecycle_recovery_custody
       WHERE coordination_run_id=? AND agent_id=? AND generation_loss_id=? AND state<>'terminal'
       LIMIT 1
    `).get(intent.coordinationRunId, intent.agentId, loss.generationLossId);
    if (isRow(open)) {
      throw new ProjectFabricCoreError("CONFLICT", "generation loss already has an open recovery custody");
    }
    const attestation = this.#verifiedAttestation(intent, input.operatorId, loss.lossEvidenceDigest);
    const admitted = row(this.#database.prepare(`
      SELECT admission_digest FROM lifecycle_admitted_run_scopes
       WHERE project_session_id=? AND run_id=?
    `).get(intent.projectSessionId, intent.coordinationRunId), "admitted lifecycle receipt scope");
    const recoveryId = `agent-lifecycle-recovery:${sha256(canonicalJson({
      schemaVersion: 1,
      generationLossId: loss.generationLossId,
      operatorId: input.operatorId,
      operatorCommandId: input.operatorCommandId,
    }))}`;
    const operatorDecisionDigest = lifecycleDigest("generation-loss-operator-decision", {
      schemaVersion: 1,
      operatorId: input.operatorId,
      operatorCommandId: input.operatorCommandId,
      directInputAttestationId: intent.directInputAttestationId,
      destructiveConfirmationDigest: intent.destructiveConfirmationDigest,
      gateId: intent.gateId,
      gateRevision: intent.expectedGateRevision,
      reason: intent.reason,
    });
    const terminalEvidenceDigest = lifecycleDigest("generation-loss-direct-open-evidence", {
      schemaVersion: 1,
      runId: intent.coordinationRunId,
      agentId: intent.agentId,
      generationLossId: loss.generationLossId,
      expectedRevision: intent.expectedSourceRevision,
      lossEvidenceDigest: loss.lossEvidenceDigest,
      operatorDecisionDigest,
      destructiveConfirmationDigest: intent.destructiveConfirmationDigest,
    });
    const now = this.#clock();
    const prepared = this.#receipts.prepareGenerationLossTerminalInCurrentTransaction({
      runId: intent.coordinationRunId,
      agentId: intent.agentId,
      generationLossId: loss.generationLossId,
      expectedRevision: intent.expectedSourceRevision,
      applyId: recoveryId,
      admissionDigest: text(admitted, "admission_digest"),
      operatorDecisionDigest,
      transitionProof: {
        schemaVersion: 1,
        kind: "direct-open-abandon",
        operatorDecisionDigest,
        directInputAttestationId: attestation.attestationId,
        destructiveConfirmationDigest: intent.destructiveConfirmationDigest,
      },
      mutationPlan: EMPTY_MUTATION_PLAN,
      terminalEvidenceDigest,
      recordedAt: now,
    });
    this.#database.prepare(`
      INSERT INTO agent_lifecycle_recovery_custody(
        recovery_id, operator_id, operator_command_id, path, project_session_id,
        coordination_run_id, agent_id, generation_loss_id, apply_id,
        expected_source_revision, gate_id, expected_gate_revision,
        direct_input_attestation_id, destructive_confirmation_digest,
        operator_decision_digest, terminal_evidence_digest, intent_digest,
        intent_json, state, result_json, revision, created_at, updated_at
      ) VALUES (?,?,?,'abandon',?,?,?,?,?,?,?,?,?,?,?,?,?,?,'prepared',NULL,1,?,?)
    `).run(
      recoveryId, input.operatorId, input.operatorCommandId, intent.projectSessionId,
      intent.coordinationRunId, intent.agentId, loss.generationLossId, prepared.applyId,
      intent.expectedSourceRevision, intent.gateId, intent.expectedGateRevision,
      attestation.attestationId, intent.destructiveConfirmationDigest,
      operatorDecisionDigest, terminalEvidenceDigest, intentDigest,
      canonicalJson(intent), now, now,
    );
    return {
      status: "pending",
      recoveryId,
      path: "abandon",
      evidenceDigest: parseSha256Digest(terminalEvidenceDigest, "lifecycleRecoveryCustody.evidenceDigest"),
    };
  }

  lifecycleRecoveryStatus(operatorId: string, operatorCommandId: string): OperatorLifecycleRecoveryCommit {
    const custody = this.#custodyRow(operatorId, operatorCommandId);
    return this.#custodyCommit(custody);
  }

  async reconcileLifecycleRecovery(
    operatorId: string,
    operatorCommandId: string,
  ): Promise<OperatorLifecycleRecoveryCommit> {
    const custody = this.#custodyRow(operatorId, operatorCommandId);
    if (text(custody, "state") === "terminal") return this.#custodyCommit(custody);
    const recoveryId = text(custody, "recovery_id");
    const runId = text(custody, "coordination_run_id");
    const agentId = text(custody, "agent_id");
    const generationLossId = text(custody, "generation_loss_id");
    const applyId = text(custody, "apply_id");
    const prepared = this.#receipts.readPreparedGenerationLossTerminal(
      runId,
      agentId,
      generationLossId,
      applyId,
    );
    if (prepared === null) {
      throw new ProjectFabricCoreError("RECOVERY_REQUIRED", "prepared lifecycle abandon batch is missing");
    }
    if (
      prepared.terminalEvidenceDigest !== text(custody, "terminal_evidence_digest") ||
      prepared.subject.operatorDecisionDigest !== text(custody, "operator_decision_digest")
    ) {
      throw new ProjectFabricCoreError("CONFLICT", "prepared lifecycle abandon crossed its operator custody");
    }
    const commit: OperatorLifecycleRecoveryCommit = {
      status: "committed",
      recoveryId,
      path: "abandon",
      evidenceDigest: parseSha256Digest(
        prepared.terminalEvidenceDigest,
        "lifecycleRecoveryCustody.evidenceDigest",
      ),
    };
    const head = new GenerationLossRepository(this.#database).readHead(runId, agentId, generationLossId);
    if (head.terminal) {
      const applied = this.#database.prepare(`
        SELECT receipt_apply_id FROM lifecycle_generation_loss_revisions
         WHERE run_id=? AND agent_id=? AND generation_loss_id=? AND revision=?
      `).get(runId, agentId, generationLossId, prepared.finalRevision);
      if (
        head.revision !== prepared.finalRevision ||
        head.state !== "abandoned" ||
        head.abandonKind !== "direct-open" ||
        !isRow(applied) ||
        text(applied, "receipt_apply_id") !== applyId
      ) {
        throw new ProjectFabricCoreError("CONFLICT", "generation loss was terminalised by another owner");
      }
      this.#settleTerminal(recoveryId, commit);
      return commit;
    }
    const scopeHead = row(this.#database.prepare(`
      SELECT scope.authority_id,head.checkpoint_digest,head.revision
        FROM lifecycle_admitted_run_scopes scope
        JOIN lifecycle_receipt_scope_heads head
          ON head.project_session_id=scope.project_session_id AND head.run_id=scope.run_id
       WHERE scope.project_session_id=? AND scope.run_id=?
    `).get(prepared.projectSessionId, runId), "admitted lifecycle receipt scope head");
    let recovered: Awaited<ReturnType<typeof recoverTerminalAuthorityReceipt>>;
    try {
      recovered = await recoverTerminalAuthorityReceipt(this.#authority, prepared);
    } catch (error: unknown) {
      if (error instanceof FabricError && error.code === "LIFECYCLE_PRECONDITION_FAILED") throw error;
      this.#markAmbiguous(recoveryId);
      return {
        status: "ambiguous",
        recoveryId,
        path: "abandon",
        evidenceDigest: commit.evidenceDigest,
      };
    }
    this.#database.transaction(() => {
      this.#receipts.applyAuthorizedGenerationLossTerminalInCurrentTransaction({
        prepared,
        expectedRevision: prepared.preRevision,
        expectedScopeHead: {
          checkpointDigest: text(scopeHead, "checkpoint_digest"),
          revision: integer(scopeHead, "revision"),
        },
        receipt: {
          authorityId: recovered.record.receipt.authorityId,
          authoritySequence: recovered.record.receipt.authoritySequence,
          previousReceiptDigest: recovered.record.receipt.previousReceiptDigest,
          receiptDigest: recovered.record.receipt.receiptDigest,
          attestation: recovered.record.receipt.attestation,
          verifiedAt: this.#clock(),
        },
        scopeCheckpoint: {
          receiptCount: recovered.checkpoint.receiptCount,
          headAuthoritySequence: recovered.checkpoint.headAuthoritySequence,
          headReceiptDigest: recovered.checkpoint.headReceiptDigest as LifecycleDigest,
          orderedRecordSetDigest: recovered.checkpoint.orderedRecordSetDigest,
          checkpointDigest: recovered.checkpoint.checkpointDigest,
          attestation: recovered.checkpoint.attestation,
          verifiedAt: this.#clock(),
        },
        authorizedAt: this.#clock(),
        appliedAt: this.#clock(),
        localWrites: [],
        revalidateAdoptionWrites: () => undefined,
        performAdoptionWrites: () => undefined,
      });
      this.#settleTerminal(recoveryId, commit);
    }).immediate();
    return commit;
  }

  #settleTerminal(recoveryId: string, commit: OperatorLifecycleRecoveryCommit): void {
    const changed = this.#database.prepare(`
      UPDATE agent_lifecycle_recovery_custody
         SET state='terminal', result_json=?, revision=revision+1, updated_at=?
       WHERE recovery_id=? AND state<>'terminal'
    `).run(canonicalJson(commit), this.#clock(), recoveryId);
    if (changed.changes !== 1) {
      throw new ProjectFabricCoreError("CONFLICT", "lifecycle recovery custody changed before terminal commit");
    }
  }

  #markAmbiguous(recoveryId: string): void {
    this.#database.prepare(`
      UPDATE agent_lifecycle_recovery_custody
         SET state='ambiguous', revision=revision+1, updated_at=?
       WHERE recovery_id=? AND state<>'terminal'
    `).run(this.#clock(), recoveryId);
  }

  #custodyRow(operatorId: string, operatorCommandId: string): Readonly<Record<string, unknown>> {
    const stored = this.#database.prepare(`
      SELECT * FROM agent_lifecycle_recovery_custody
       WHERE operator_id=? AND operator_command_id=?
    `).get(operatorId, operatorCommandId);
    if (!isRow(stored)) {
      throw new ProjectFabricCoreError("NOT_FOUND", "lifecycle recovery custody is unknown");
    }
    return stored;
  }

  #custodyCommit(custody: Readonly<Record<string, unknown>>): OperatorLifecycleRecoveryCommit {
    const state = text(custody, "state");
    if (state === "terminal") {
      const stored: unknown = JSON.parse(text(custody, "result_json"));
      if (
        !isRow(stored) ||
        stored.status !== "committed" ||
        stored.recoveryId !== text(custody, "recovery_id") ||
        stored.path !== "abandon" ||
        typeof stored.evidenceDigest !== "string"
      ) {
        throw new Error("terminal lifecycle recovery result is invalid");
      }
      return stored as OperatorLifecycleRecoveryCommit;
    }
    return {
      status: state === "ambiguous" ? "ambiguous" : "pending",
      recoveryId: text(custody, "recovery_id"),
      path: "abandon",
      evidenceDigest: parseSha256Digest(
        text(custody, "terminal_evidence_digest"),
        "lifecycleRecoveryCustody.evidenceDigest",
      ),
    };
  }

  #inspectionDigest(intent: LifecycleRecoveryIntent): Sha256Digest {
    return parseSha256Digest(
      lifecycleDigest("agent-lifecycle-recovery-inspection", {
        intent,
        state: this.#currentState(intent),
      }),
      "lifecycleRecoveryCustody.inspectionDigest",
    );
  }

  #lossIdentity(intent: LifecycleRecoveryIntent): Readonly<{
    generationLossId: string;
    lossEvidenceDigest: string;
  }> {
    if (intent.source.kind !== "generation-loss") {
      throw new ProjectFabricCoreError(
        "CAPABILITY_FORBIDDEN",
        "custody-source lifecycle recovery is owned by the rotation custody owner",
      );
    }
    const ref = intent.source.generationLossRef;
    const loss = row(this.#database.prepare(`
      SELECT project_session_id,loss_evidence_digest FROM lifecycle_generation_losses
       WHERE run_id=? AND agent_id=? AND generation_loss_id=?
    `).get(ref.runId, ref.agentId, ref.generationLossId), "lifecycle recovery generation loss");
    if (text(loss, "project_session_id") !== intent.projectSessionId) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "generation loss belongs to another project session");
    }
    return {
      generationLossId: ref.generationLossId,
      lossEvidenceDigest: text(loss, "loss_evidence_digest"),
    };
  }

  #verifiedAttestation(
    intent: LifecycleAbandonIntent,
    operatorId: string,
    lossEvidenceDigest: string,
  ): Readonly<{ attestationId: string }> {
    const stored = this.#database.prepare(`
      SELECT operator_id,project_session_id,coordination_run_id,gate_id,
             expected_gate_revision,exact_utterance,artifact_digests_json
        FROM operator_input_attestations WHERE attestation_id=?
    `).get(intent.directInputAttestationId);
    if (!isRow(stored)) {
      throw new ProjectFabricCoreError("GATE_BLOCKED", "lifecycle abandon input attestation is unknown");
    }
    if (text(stored, "operator_id") !== operatorId) {
      throw new ProjectFabricCoreError(
        "CAPABILITY_FORBIDDEN",
        "lifecycle abandon attestation belongs to another operator",
      );
    }
    if (
      text(stored, "project_session_id") !== intent.projectSessionId ||
      text(stored, "coordination_run_id") !== intent.coordinationRunId ||
      text(stored, "gate_id") !== intent.gateId ||
      integer(stored, "expected_gate_revision") !== intent.expectedGateRevision
    ) {
      throw new ProjectFabricCoreError("GATE_BLOCKED", "lifecycle abandon attestation no longer matches the gate");
    }
    const utterance = text(stored, "exact_utterance");
    if (
      utterance.length === 0 ||
      `sha256:${sha256(utterance)}` !== intent.destructiveConfirmationDigest
    ) {
      throw new ProjectFabricCoreError(
        "GATE_BLOCKED",
        "lifecycle abandon confirmation does not match the attested byte-exact phrase",
      );
    }
    const digests = parseStoredAttestationDigests(text(stored, "artifact_digests_json"));
    if (!digests.includes(parseSha256Digest(lossEvidenceDigest, "lifecycleRecoveryCustody.lossEvidenceDigest"))) {
      throw new ProjectFabricCoreError(
        "GATE_BLOCKED",
        "lifecycle abandon attestation does not bind the exact generation-loss evidence",
      );
    }
    return { attestationId: intent.directInputAttestationId };
  }

  #currentState(intent: LifecycleRecoveryIntent): OperatorLifecycleRecoveryCurrentState {
    const loss = this.#lossIdentity(intent);
    const runId = intent.coordinationRunId;
    const agentId = intent.agentId;
    const head = new GenerationLossRepository(this.#database).readHead(runId, agentId, loss.generationLossId);
    const stored = row(this.#database.prepare(`
      SELECT * FROM lifecycle_generation_losses
       WHERE run_id=? AND agent_id=? AND generation_loss_id=?
    `).get(runId, agentId, loss.generationLossId), "lifecycle recovery generation loss");
    const run = row(this.#database.prepare(`
      SELECT project_session_id,revision,chair_generation FROM runs WHERE run_id=?
    `).get(runId), "lifecycle recovery run");
    if (text(run, "project_session_id") !== intent.projectSessionId) {
      throw new ProjectFabricCoreError("WRONG_PROJECT", "lifecycle recovery run belongs to another project session");
    }
    const session = row(this.#database.prepare(`
      SELECT revision,generation FROM project_sessions WHERE project_session_id=?
    `).get(intent.projectSessionId), "lifecycle recovery project session");
    const bridgeOwnerKind = text(stored, "bridge_owner_kind");
    if (bridgeOwnerKind !== "chair" && bridgeOwnerKind !== "child") {
      throw new Error("lifecycle recovery bridge owner kind is invalid");
    }
    const bridge = bridgeOwnerKind === "chair"
      ? row(this.#database.prepare(`
          SELECT bridge_generation,revision FROM launched_chair_bridge_state
           WHERE project_session_id=? AND coordination_run_id=? AND chair_agent_id=?
        `).get(intent.projectSessionId, runId, agentId), "lifecycle recovery chair bridge")
      : row(this.#database.prepare(`
          SELECT bridge_generation,revision FROM agent_bridge_state
           WHERE run_id=? AND agent_id=?
        `).get(runId, agentId), "lifecycle recovery agent bridge");
    const identity = row(this.#database.prepare(`
      SELECT provider_generation,principal_generation FROM agent_lifecycle_identity_high_water
       WHERE run_id=? AND agent_id=?
    `).get(runId, agentId), "lifecycle recovery identity high-water");
    const providerGeneration = integer(identity, "provider_generation");
    const context = row(this.#database.prepare(`
      SELECT context_revision FROM agent_lifecycle_context_high_water
       WHERE run_id=? AND agent_id=? AND provider_generation=?
    `).get(runId, agentId, providerGeneration), "lifecycle recovery context high-water");
    const gate = row(this.#database.prepare(`
      SELECT project_session_id,coordination_run_id,status,revision FROM scoped_gates
       WHERE gate_id=?
    `).get(intent.gateId), "lifecycle recovery gate");
    if (
      text(gate, "project_session_id") !== intent.projectSessionId ||
      text(gate, "coordination_run_id") !== runId
    ) {
      throw new ProjectFabricCoreError("GATE_BLOCKED", "lifecycle recovery gate belongs to another scope");
    }
    if (text(gate, "status") !== "approved") {
      throw new ProjectFabricCoreError("GATE_BLOCKED", "lifecycle recovery gate is not approved");
    }
    const lossKind = text(stored, "loss_kind");
    if (lossKind !== "generation-advance" && lossKind !== "context-advance") {
      throw new Error("lifecycle recovery loss kind is invalid");
    }
    const checkpointState = text(stored, "checkpoint_state");
    if (checkpointState !== "absent" && checkpointState !== "invalid" && checkpointState !== "last-validated") {
      throw new Error("lifecycle recovery checkpoint state is invalid");
    }
    const agentRevision = integer(bridge, "revision");
    // Checkpoint revision ownership is a later deferred slice; the abandon
    // path never consumes the checkpoint, so a last-validated ref projects
    // its stored identity at revision one until that slice settles it.
    const source: LifecycleRecoveryIntent["source"] = {
      kind: "generation-loss",
      oldCustodyRef: null,
      generationLossRef: {
        schemaVersion: 1,
        runId,
        agentId,
        generationLossId: loss.generationLossId,
        generationLossRevision: head.revision,
      },
      lossKind,
      oldProviderSessionRef: text(stored, "old_provider_session_ref"),
      newProviderSessionRef: text(stored, "new_provider_session_ref"),
      oldProviderGeneration: integer(stored, "old_provider_generation"),
      newProviderGeneration: integer(stored, "new_provider_generation"),
      oldContextRevision: integer(stored, "old_context_revision"),
      newContextRevision: integer(stored, "new_context_revision"),
      sourceBridgeRef: {
        bridgeId: text(stored, "source_bridge_row_id"),
        bridgeRevision: integer(stored, "source_bridge_revision"),
      },
      sourceCapabilityHash: prefixedDigest(text(stored, "source_capability_hash")),
      checkpointState,
      checkpointRef: checkpointState === "last-validated"
        ? { checkpointId: text(stored, "checkpoint_ref"), checkpointRevision: 1 }
        : null,
      checkpointDigest: checkpointState === "last-validated" ? text(stored, "checkpoint_digest") : null,
      lossEvidenceDigest: loss.lossEvidenceDigest,
    };
    return {
      revision: agentRevision,
      projectSessionId: intent.projectSessionId,
      coordinationRunId: runId,
      agentId,
      sessionRevision: integer(session, "revision"),
      sessionGeneration: integer(session, "generation"),
      runRevision: integer(run, "revision"),
      agentRevision,
      source,
      sourceRevision: head.revision,
      principalGeneration: integer(identity, "principal_generation"),
      providerGeneration,
      bridgeGeneration: integer(bridge, "bridge_generation"),
      contextRevision: integer(context, "context_revision"),
      bridgeOwnerKind,
      chairLeaseGeneration: bridgeOwnerKind === "chair" ? integer(run, "chair_generation") : null,
      gate: {
        gateId: intent.gateId,
        revision: integer(gate, "revision"),
        status: "approved",
      },
      recoveryCapability: null,
      checkpoint: null,
    };
  }
}
