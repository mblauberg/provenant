import type Database from "better-sqlite3";

import { assertProviderActionOwner, ProviderActionOwnerError } from "../application/provider-action-owner.js";
import { FabricError } from "../errors.js";
import {
  canonicalJson,
  stringDigest as sha256Digest,
  integer as numberField,
  isRow,
  row as rowOrNotFound,
  text as stringField,
} from "../persistence/row-codec.js";
import { isLifecycleCheckpoint } from "./checkpoint-policy.js";
import type { LifecycleCheckpoint } from "../core/contracts.js";
import { type LifecycleRotationRepository } from "./rotation-repository.js";
import type { LifecycleFinalizer } from "./finalizer.js";
import type { LifecycleReceiptRepository } from "./receipt-repository.js";
import type { LifecycleDigest, LifecycleIntegrityReceiptAuthorityPort, LifecycleReceiptRecord } from "./receipt-authority.js";
import { parseAgentProvisionProviderResult, type AgentProvisionProviderResult } from "../adapters/providers/types.js";

// Behaviour-preserving extraction of Fabric's private #recoverLifecycleRotations. The body is
// unchanged, including the ProviderActionOwnerError rethrow (recovery propagates ownership
// crossings to its caller — unlike live continuation, which swallows the same error kind), the
// same SQL, event emissions, and error codes. #recoverCertifyingReviewProviderActions is
// deliberately NOT moved here: it is a generic-side recovery routine and stays on Fabric. Calls
// back into Fabric-private state that stays behind (adapter request dispatch, event emission) are
// narrow injected function ports bound to the same Fabric instance, so observed behaviour is
// identical. Fabric's startup phase order (recoverStartupState) is unchanged; it now calls
// `recoverRotations()` in place of the private method.
type LifecycleRecoveryAdmissionPort = Readonly<{
  ensureReceiptScope: (runId: string, agentId: string) => Promise<void>;
  sourceVectorDigest: (
    runId: string,
    agentId: string,
    taskId: string,
    checkpoint: LifecycleCheckpoint,
    adoptionDeliveries: readonly Readonly<{
      deliveryId: string;
      claimGeneration: number;
      requesterAgentId: string;
      targetProviderSession: string;
    }>[],
  ) => string;
}>;

export class LifecycleRecovery {
  readonly #database: Database.Database;
  readonly #clock: () => number;
  readonly #lifecycleRotations: LifecycleRotationRepository;
  readonly #lifecycleReceipts: LifecycleReceiptRepository;
  readonly #lifecycleReceiptAuthority: LifecycleIntegrityReceiptAuthorityPort | undefined;
  readonly #admission: LifecycleRecoveryAdmissionPort;
  readonly #finalizer: LifecycleFinalizer;
  readonly #requestAdapter: (adapterId: string, method: string, params: Record<string, unknown>) => Promise<unknown>;
  readonly #event: (runId: string, type: string, actorAgentId: string | null, payload: unknown) => void;

  constructor(dependencies: Readonly<{
    database: Database.Database;
    clock: () => number;
    lifecycleRotations: LifecycleRotationRepository;
    lifecycleReceipts: LifecycleReceiptRepository;
    lifecycleReceiptAuthority: LifecycleIntegrityReceiptAuthorityPort | undefined;
    admission: LifecycleRecoveryAdmissionPort;
    finalizer: LifecycleFinalizer;
    requestAdapter: (adapterId: string, method: string, params: Record<string, unknown>) => Promise<unknown>;
    event: (runId: string, type: string, actorAgentId: string | null, payload: unknown) => void;
  }>) {
    this.#database = dependencies.database;
    this.#clock = dependencies.clock;
    this.#lifecycleRotations = dependencies.lifecycleRotations;
    this.#lifecycleReceipts = dependencies.lifecycleReceipts;
    this.#lifecycleReceiptAuthority = dependencies.lifecycleReceiptAuthority;
    this.#admission = dependencies.admission;
    this.#finalizer = dependencies.finalizer;
    this.#requestAdapter = dependencies.requestAdapter;
    this.#event = dependencies.event;
  }

  async recoverRotations(): Promise<void> {
    const pending = this.#database.prepare(`
      SELECT head.state,head.current_revision,custody.*,
             head_revision.terminal_evidence_digest AS head_terminal_evidence_digest,
             action.status AS action_status,action.history_json,action.execution_count,
             action.effect_count,action.idempotency_proven,action.payload_json,action.result_json,
             agent.authority_id,capability.expires_at,checkpoint.checkpoint_json,
             checkpoint.task_id AS checkpoint_task_id,
             (
               SELECT 'sha256:' || observed.sha256 FROM lifecycle_checkpoints observed
                WHERE observed.run_id=custody.run_id AND observed.agent_id=custody.agent_id
                  AND observed.relative_path=custody.checkpoint_ref
                ORDER BY observed.created_at DESC,observed.checkpoint_id DESC LIMIT 1
             ) AS observed_checkpoint_digest
        FROM lifecycle_rotation_custody_heads head
        JOIN lifecycle_rotation_custodies custody
          ON custody.run_id=head.run_id AND custody.agent_id=head.agent_id
         AND custody.custody_id=head.custody_id
        JOIN lifecycle_rotation_custody_revisions head_revision
          ON head_revision.project_session_id=head.project_session_id
         AND head_revision.run_id=head.run_id AND head_revision.agent_id=head.agent_id
         AND head_revision.custody_id=head.custody_id
         AND head_revision.revision=head.current_revision
        LEFT JOIN provider_actions action
          ON action.run_id=custody.run_id
         AND action.adapter_id=custody.provider_action_adapter_id
         AND action.action_id=custody.provider_action_id
        JOIN agents agent ON agent.run_id=custody.run_id AND agent.agent_id=custody.agent_id
        LEFT JOIN capabilities capability ON capability.token_hash=custody.staged_capability_hash
        LEFT JOIN lifecycle_checkpoints checkpoint
          ON checkpoint.run_id=custody.run_id AND checkpoint.agent_id=custody.agent_id
         AND checkpoint.relative_path=custody.checkpoint_ref
         AND ('sha256:' || checkpoint.sha256)=custody.checkpoint_digest
       WHERE head.terminal=0
       ORDER BY custody.created_at,custody.custody_id
    `).all();
    for (const value of pending) {
      const row = rowOrNotFound(value, "recoverable lifecycle rotation");
      try {
      const runId = stringField(row, "run_id");
      const adapterId = stringField(row, "provider_action_adapter_id");
      const actionId = stringField(row, "provider_action_id");
      assertProviderActionOwner(this.#database, { runId, adapterId, actionId }, "lifecycle");
      const agentId = stringField(row, "agent_id");
      const bridgeContractDigest = stringField(row, "replacement_contract_digest");
      const targetBridgeGeneration = numberField(row, "target_bridge_generation");
      const bridgeOwnerKind = stringField(row, "bridge_owner_kind") as "chair" | "child";
      const minimalInput = {
        runId,
        agentId,
        custodyId: stringField(row, "custody_id"),
        adapterId,
        actionId,
        sourceActionId: stringField(row, "source_custody_action_id"),
        sourceCapabilityHash: stringField(row, "source_capability_hash"),
        sourceProviderSessionRef: stringField(row, "source_provider_session_ref"),
        stagedCapabilityHash: stringField(row, "staged_capability_hash"),
      };
      let recoveredCheckpoint: LifecycleCheckpoint | null = null;
      try {
        const candidate: unknown = JSON.parse(stringField(row, "checkpoint_json"));
        if (isLifecycleCheckpoint(candidate)) recoveredCheckpoint = candidate;
      } catch {
        recoveredCheckpoint = null;
      }
      const recoveredAdoptionDeliveries = this.#database.prepare(`
        SELECT ownership.delivery_id AS deliveryId,
               ownership.delivery_generation AS claimGeneration,
               ownership.recipient_agent_id AS requesterAgentId,
               custody.source_provider_session_ref AS targetProviderSession
          FROM lifecycle_custody_adoption_deliveries ownership
          JOIN lifecycle_rotation_custodies custody
            ON custody.run_id=ownership.run_id AND custody.agent_id=ownership.agent_id
           AND custody.custody_id=ownership.custody_id
         WHERE ownership.run_id=? AND ownership.agent_id=? AND ownership.custody_id=?
           AND ownership.active_owner=1
         ORDER BY ownership.ordinal
      `).all(runId, agentId, minimalInput.custodyId).map((candidate) => {
        const delivery = rowOrNotFound(candidate, "recoverable lifecycle adoption delivery");
        return {
          deliveryId: stringField(delivery, "deliveryId"),
          claimGeneration: numberField(delivery, "claimGeneration"),
          requesterAgentId: stringField(delivery, "requesterAgentId"),
          targetProviderSession: stringField(delivery, "targetProviderSession"),
        };
      });
      let head = this.#lifecycleRotations.readHead(runId, agentId, minimalInput.custodyId);
      const finalizeQuarantine = async (reason: string, evidence: unknown): Promise<void> => {
        const proof = {
          schemaVersion: 1,
          kind: "integrity-quarantine",
          sourceState: head.state,
          reason,
          providerActionRef: { runId, adapterId, actionId },
          evidenceDigest: sha256Digest(canonicalJson(evidence)),
        };
        const digest = sha256Digest(canonicalJson(proof));
        await this.#admission.ensureReceiptScope(runId, agentId);
        await this.#finalizer.finalizeRotationAdopted(minimalInput, head, digest, null, {
          disposition: "quarantined",
          proofKind: "integrity-quarantine",
          transitionProof: proof,
        });
      };
      const lifecycleDrift = (): Readonly<Record<string, unknown>> | null => {
        const expectedCheckpointDigest = stringField(row, "checkpoint_digest");
        const observedCheckpointDigest = typeof row.observed_checkpoint_digest === "string"
          ? row.observed_checkpoint_digest
          : null;
        const checkpointChanged = typeof row.checkpoint_json !== "string" &&
          observedCheckpointDigest !== null && observedCheckpointDigest !== expectedCheckpointDigest;
        const expectedSource = {
          projectSessionGeneration: numberField(row, "source_project_session_generation"),
          runGeneration: numberField(row, "source_run_generation"),
          chairLeaseGeneration: numberField(row, "source_chair_lease_generation"),
          adapterId: stringField(row, "source_adapter_id"),
          actionId: stringField(row, "source_custody_action_id"),
          providerSessionRef: stringField(row, "source_provider_session_ref"),
          providerGeneration: numberField(row, "source_provider_generation"),
          principalGeneration: numberField(row, "source_principal_generation"),
          bridgeGeneration: numberField(row, "source_bridge_generation"),
          bridgeRevision: numberField(row, "source_bridge_revision"),
          capabilityHash: stringField(row, "source_capability_hash"),
          bridgeContractDigest: stringField(row, "source_adapter_contract_digest"),
        };
        const observedSource = bridgeOwnerKind === "chair"
          ? this.#database.prepare(`
              SELECT session.generation AS projectSessionGeneration,
                     run.revision AS runGeneration,
                     COALESCE(run.chair_generation,1) AS chairLeaseGeneration,
                     bridge.provider_adapter_id AS adapterId,
                     bridge.provider_action_id AS actionId,
                     bridge.provider_session_ref AS providerSessionRef,
                     bridge.provider_session_generation AS providerGeneration,
                     bridge.principal_generation AS principalGeneration,
                     bridge.bridge_generation AS bridgeGeneration,
                     bridge.revision AS bridgeRevision,
                     bridge.capability_hash AS capabilityHash,
                     bridge.provider_contract_digest AS bridgeContractDigest
                FROM runs run
                JOIN project_sessions session ON session.project_session_id=run.project_session_id
                JOIN launched_chair_bridge_state bridge
                  ON bridge.project_session_id=run.project_session_id
                 AND bridge.coordination_run_id=run.run_id
                 AND bridge.chair_agent_id=? AND bridge.state='active'
                JOIN capabilities capability
                  ON capability.token_hash=bridge.capability_hash AND capability.revoked_at IS NULL
               WHERE run.run_id=? AND run.chair_agent_id=bridge.chair_agent_id
            `).get(agentId, runId)
          : this.#database.prepare(`
              SELECT session.generation AS projectSessionGeneration,
                     run.revision AS runGeneration,
                     COALESCE(run.chair_generation,1) AS chairLeaseGeneration,
                     bridge.adapter_id AS adapterId,bridge.action_id AS actionId,
                     bridge.provider_session_ref AS providerSessionRef,
                     bridge.provider_session_generation AS providerGeneration,
                     capability.principal_generation AS principalGeneration,
                     bridge.bridge_generation AS bridgeGeneration,
                     bridge.revision AS bridgeRevision,bridge.capability_hash AS capabilityHash,
                     source.bridge_contract_digest AS bridgeContractDigest
                FROM runs run
                JOIN project_sessions session ON session.project_session_id=run.project_session_id
                JOIN agent_bridge_state bridge
                  ON bridge.run_id=run.run_id AND bridge.agent_id=? AND bridge.bridge_state='active'
                JOIN provider_agent_custody source
                  ON source.run_id=bridge.run_id AND source.adapter_id=bridge.adapter_id
                 AND source.action_id=bridge.action_id
                JOIN capabilities capability
                  ON capability.token_hash=bridge.capability_hash AND capability.revoked_at IS NULL
               WHERE run.run_id=? AND run.chair_agent_id<>bridge.agent_id
            `).get(agentId, runId);
        const sourceChanged = !isRow(observedSource) ||
          canonicalJson(observedSource) !== canonicalJson(expectedSource);
        const observedSourceVectorDigest = recoveredCheckpoint === null
          ? null
          : this.#admission.sourceVectorDigest(
              runId,
              agentId,
              stringField(row, "checkpoint_task_id"),
              recoveredCheckpoint,
              recoveredAdoptionDeliveries,
            );
        const sourceVectorChanged = observedSourceVectorDigest !== null &&
          observedSourceVectorDigest !== stringField(row, "precondition_digest");
        if (!checkpointChanged && !sourceChanged && !sourceVectorChanged) return null;
        return {
          driftKind: checkpointChanged ? "checkpoint" : sourceChanged ? "source" : "accepted-source-vector",
          expectedCheckpointDigest,
          observedCheckpointDigest,
          expectedSource,
          observedSource: observedSource ?? null,
          expectedSourceVectorDigest: stringField(row, "precondition_digest"),
          observedSourceVectorDigest,
        };
      };
      const finalizeSuperseded = async (
        drift: Readonly<Record<string, unknown>>,
        terminalObservation: unknown = null,
      ): Promise<void> => {
        const postterminal = head.state === "provider-terminal" || head.state === "committing";
        const abandonedAdoption = postterminal
          ? this.#lifecycleReceipts.readPreparedChildCustodyTerminal(
              runId,
              agentId,
              minimalInput.custodyId,
              `${minimalInput.custodyId}:apply`,
            )
          : null;
        let recoveredAdoptionReceipt: LifecycleReceiptRecord | null = null;
        if (abandonedAdoption?.disposition === "adopted" && abandonedAdoption.preRevision === head.revision) {
          const localReceipt = this.#database.prepare(`
            SELECT receipt_digest FROM lifecycle_authority_receipts WHERE intent_digest=?
          `).get(abandonedAdoption.intentDigest);
          if (!isRow(localReceipt)) {
            const authority = this.#lifecycleReceiptAuthority;
            if (authority === undefined) {
              throw new FabricError("CAPABILITY_UNAVAILABLE", "lifecycle receipt authority recovery is unavailable");
            }
            const lookup = {
              kind: "custody-terminal" as const,
              projectSessionId: abandonedAdoption.projectSessionId,
              runId: abandonedAdoption.runId,
              agentId: abandonedAdoption.agentId,
              ownerRefDigest: abandonedAdoption.ownerRefDigest as LifecycleDigest,
              ownerRevision: abandonedAdoption.finalRevision,
            };
            try {
              recoveredAdoptionReceipt = await authority.readReceipt(lookup);
            } catch (error: unknown) {
              throw new FabricError("CAPABILITY_UNAVAILABLE", "stale lifecycle adoption receipt read failed", { cause: error });
            }
            if (
              recoveredAdoptionReceipt === null ||
              canonicalJson(recoveredAdoptionReceipt.subject) !== abandonedAdoption.subjectJson ||
              recoveredAdoptionReceipt.receipt.intentDigest !== abandonedAdoption.intentDigest ||
              recoveredAdoptionReceipt.receipt.subjectDigest !== abandonedAdoption.subjectDigest ||
              !await authority.verifyReceipt(abandonedAdoption.subject, recoveredAdoptionReceipt.receipt)
            ) {
              throw new FabricError(
                "LIFECYCLE_PRECONDITION_FAILED",
                "stale lifecycle adoption receipt authority evidence is absent or crossed",
              );
            }
          }
          head = this.#database.transaction(() => {
            if (recoveredAdoptionReceipt !== null) {
              this.#lifecycleReceipts.persistVerifiedAuthorityReceiptInCurrentTransaction(
                abandonedAdoption,
                recoveredAdoptionReceipt,
              );
            }
            return this.#lifecycleRotations.appendInCurrentTransaction({
              runId,
              agentId,
              custodyId: minimalInput.custodyId,
              expectedRevision: head.revision,
              state: "committing",
              terminalEvidenceDigest: abandonedAdoption.terminalEvidenceDigest,
              recordedAt: this.#clock(),
            });
          }).immediate();
        }
        const expectedSourceJournalDigest = sha256Digest(canonicalJson(drift.expectedSource));
        const observedSourceJournalDigest = sha256Digest(canonicalJson(drift.observedSource));
        const base = {
          schemaVersion: 1,
          sourceState: head.state,
          expectedSourceJournalDigest,
          observedSourceJournalDigest,
          expectedCheckpointDigest: drift.expectedCheckpointDigest,
          observedCheckpointDigest: drift.observedCheckpointDigest ?? drift.expectedCheckpointDigest,
        };
        const proof = postterminal ? {
          ...base,
          kind: "postterminal-adoption-cas-superseded",
          terminalObservationDigest: sha256Digest(canonicalJson(terminalObservation)),
          replacementCandidateDigest: sha256Digest(canonicalJson(terminalObservation)),
          expectedMutationPreconditionDigest: stringField(row, "precondition_digest"),
          failedCasEvidenceDigest: sha256Digest(canonicalJson(drift)),
        } : {
          ...base,
          kind: "predispatch-superseded",
          driftKind: drift.driftKind,
        };
        const digest = postterminal
          ? stringField(row, "head_terminal_evidence_digest")
          : sha256Digest(canonicalJson(proof));
        await this.#admission.ensureReceiptScope(runId, agentId);
        await this.#finalizer.finalizeRotationAdopted(minimalInput, head, digest, null, {
          disposition: "superseded",
          proofKind: postterminal
            ? "postterminal-adoption-cas-superseded"
            : "predispatch-superseded",
          transitionProof: proof,
        });
      };
      if (head.state === "awaiting-boundary" || head.state === "prepared") {
        const drift = lifecycleDrift();
        if (drift !== null) {
          await finalizeSuperseded(drift);
          continue;
        }
        const providerCustody = this.#database.prepare(`
          SELECT 1 FROM provider_agent_custody
           WHERE run_id=? AND adapter_id=? AND action_id=?
        `).get(runId, adapterId, actionId);
        if (
          row.action_status !== "prepared" || numberField(row, "execution_count") !== 0 ||
          numberField(row, "effect_count") !== 0 || providerCustody !== undefined
        ) {
          await finalizeQuarantine("zero-dispatch-evidence-conflict", {
            status: row.action_status ?? null,
            executionCount: row.execution_count ?? null,
            effectCount: row.effect_count ?? null,
            providerCustodyPresent: providerCustody !== undefined,
          });
          continue;
        }
        const proof = {
          schemaVersion: 1,
          kind: "zero-dispatch-no-effect",
          sourceState: head.state,
          providerActionRef: { runId, adapterId, actionId },
          dispatchCountDec: "0",
          effectCountDec: "0",
          journalDigest: sha256Digest(canonicalJson({
            status: row.action_status,
            historyJson: row.history_json,
            executionCount: row.execution_count,
            effectCount: row.effect_count,
          })),
        };
        const digest = sha256Digest(canonicalJson(proof));
        await this.#admission.ensureReceiptScope(runId, agentId);
        await this.#finalizer.finalizeRotationAdopted(minimalInput, head, digest, null, {
          disposition: "no-effect",
          proofKind: "zero-dispatch-no-effect",
          transitionProof: proof,
        });
        continue;
      }
      let rawResult: unknown;
      if (head.state === "dispatched" || head.state === "accepted" || head.state === "ambiguous") {
        let lookup: unknown;
        try {
          lookup = await this.#requestAdapter(adapterId, "lookup_action", { actionId });
        } catch (error: unknown) {
          if (error instanceof Error && error.name === "ACTION_NOT_FOUND") {
            await finalizeQuarantine("provider-action-absent", { message: error.message });
            continue;
          }
          throw new FabricError(
            "CAPABILITY_UNAVAILABLE",
            "lifecycle provider action lookup is unavailable",
            { cause: error },
          );
        }
        if (
          !isRow(lookup) || lookup.actionId !== actionId || lookup.status !== "terminal" ||
          lookup.executionCount !== 1 || lookup.effectCount !== 1 || lookup.idempotencyProven !== true ||
          !Array.isArray(lookup.history) || !lookup.history.every((entry) => typeof entry === "string") ||
          !("result" in lookup)
        ) {
          await finalizeQuarantine("provider-action-terminal-evidence-malformed", lookup);
          continue;
        }
        rawResult = lookup.result;
        try {
          const parsed = parseAgentProvisionProviderResult(rawResult, {
            adapterId, actionId, targetAgentId: agentId, bridgeGeneration: targetBridgeGeneration,
            bridgeContractDigest,
            lifecycleAttestation: {
              custodyId: minimalInput.custodyId,
              checkpointDigest: stringField(row, "checkpoint_digest"),
              challengeDigest: stringField(row, "launch_attest_challenge_digest"),
            },
          });
          if (parsed.providerSessionGeneration !== numberField(row, "target_provider_generation")) {
            throw new Error("lifecycle provider result crossed its reserved generation");
          }
        } catch {
          await finalizeQuarantine("provider-action-terminal-evidence-crossed", lookup);
          continue;
        }
        const terminalEvidenceDigest = sha256Digest(canonicalJson(rawResult));
        this.#database.transaction(() => {
          const changed = this.#database.prepare(`
            UPDATE provider_actions
               SET status='terminal',history_json=?,execution_count=1,effect_count=1,
                   idempotency_proven=1,result_json=?,journal_revision=journal_revision+1,updated_at=?
             WHERE run_id=? AND adapter_id=? AND action_id=?
               AND status IN ('dispatched','accepted','ambiguous')
          `).run(canonicalJson(lookup.history), canonicalJson(rawResult), this.#clock(), runId, adapterId, actionId);
          if (changed.changes !== 1) throw new Error("lifecycle provider action changed during recovery");
          head = this.#lifecycleRotations.appendInCurrentTransaction({
            runId, agentId, custodyId: minimalInput.custodyId, expectedRevision: head.revision,
            state: "provider-terminal", terminalEvidenceDigest, recordedAt: this.#clock(),
          });
          head = this.#lifecycleRotations.appendInCurrentTransaction({
            runId, agentId, custodyId: minimalInput.custodyId, expectedRevision: head.revision,
            state: "committing", terminalEvidenceDigest, recordedAt: this.#clock(),
          });
        }).immediate();
      } else {
        let storedHistory: unknown;
        try {
          storedHistory = JSON.parse(stringField(row, "history_json"));
        } catch {
          storedHistory = null;
        }
        if (
          row.action_status !== "terminal" || numberField(row, "execution_count") !== 1 ||
          numberField(row, "effect_count") !== 1 || numberField(row, "idempotency_proven") !== 1 ||
          !Array.isArray(storedHistory) || storedHistory.at(-1) !== "terminal" ||
          !storedHistory.every((entry) => typeof entry === "string")
        ) {
          await finalizeQuarantine("stored-provider-action-journal-conflict", {
            status: row.action_status ?? null,
            history: storedHistory,
            executionCount: row.execution_count ?? null,
            effectCount: row.effect_count ?? null,
            idempotencyProven: row.idempotency_proven ?? null,
          });
          continue;
        }
        try {
          rawResult = JSON.parse(stringField(row, "result_json"));
        } catch {
          await finalizeQuarantine("stored-provider-terminal-evidence-malformed", row.result_json ?? null);
          continue;
        }
      }
      let result: AgentProvisionProviderResult;
      try {
        result = parseAgentProvisionProviderResult(rawResult, {
          adapterId, actionId, targetAgentId: agentId, bridgeGeneration: targetBridgeGeneration,
          bridgeContractDigest,
          lifecycleAttestation: {
            custodyId: minimalInput.custodyId,
            checkpointDigest: stringField(row, "checkpoint_digest"),
            challengeDigest: stringField(row, "launch_attest_challenge_digest"),
          },
        });
        if (result.providerSessionGeneration !== numberField(row, "target_provider_generation")) {
          throw new Error("lifecycle provider result crossed its reserved generation");
        }
      } catch {
        await finalizeQuarantine("stored-provider-terminal-evidence-crossed", rawResult);
        continue;
      }
      const postterminalDrift = lifecycleDrift();
      if (postterminalDrift !== null) {
        await finalizeSuperseded(postterminalDrift, result);
        continue;
      }
      let payload: unknown;
      let checkpoint: unknown;
      try {
        payload = JSON.parse(stringField(row, "payload_json"));
        checkpoint = JSON.parse(stringField(row, "checkpoint_json"));
      } catch {
        await finalizeQuarantine("immutable-lifecycle-input-malformed", null);
        continue;
      }
      if (!isRow(payload) || !isLifecycleCheckpoint(checkpoint) || typeof row.expires_at !== "number") {
        await finalizeQuarantine("immutable-lifecycle-input-conflict", { payload, checkpoint });
        continue;
      }
      const input = {
        ...minimalInput,
        authorityId: stringField(row, "authority_id"),
        bridgeContractDigest,
        callerActionId: stringField(row, "caller_turn_lease_id"),
        targetProviderGeneration: numberField(row, "target_provider_generation"),
        targetPrincipalGeneration: numberField(row, "target_principal_generation"),
        targetBridgeGeneration,
        stagedCapability: "",
        capabilityExpiresAt: numberField(row, "expires_at"),
        providerPayload: payload,
        checkpointSha256: checkpoint.sha256,
        lifecycleInput: {
          action: payload.action === "compact" ? "compact" as const : "rotate" as const,
          agentId,
          taskId: stringField(row, "checkpoint_task_id"),
          taskRevision: numberField(row, "task_revision"),
          checkpoint,
          commandId: stringField(row, "command_id"),
        },
      };
      const terminalEvidenceDigest = sha256Digest(canonicalJson(result));
      if (head.state === "provider-terminal") {
        head = this.#database.transaction(() => this.#lifecycleRotations.appendInCurrentTransaction({
          runId, agentId, custodyId: input.custodyId, expectedRevision: head.revision,
          state: "committing", terminalEvidenceDigest, recordedAt: this.#clock(),
        })).immediate();
      }
      await this.#admission.ensureReceiptScope(runId, agentId);
      await this.#finalizer.finalizeRotationAdopted(input, head, terminalEvidenceDigest, result);
      } catch (error: unknown) {
        if (error instanceof ProviderActionOwnerError) throw error;
        if (typeof row.run_id === "string" && typeof row.agent_id === "string") {
          this.#event(row.run_id, "lifecycle-recovery-custody-failed", row.agent_id, {
            custodyId: typeof row.custody_id === "string" ? row.custody_id : null,
            message: error instanceof Error ? error.message : String(error),
          });
        }
      }
    }
  }
}
