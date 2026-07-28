import type Database from "better-sqlite3";

import type { AgentProvisionProviderResult } from "../adapters/providers/types.js";
import { FabricError } from "../errors.js";
import { assertProviderActionOwner } from "../application/provider-action-owner.js";
import { canonicalJson, digest as sha256Digest, integer as numberField, isRow, row as rowOrNotFound, sha256, text as stringField } from "../persistence/row-codec.js";
import { lifecycleDigest } from "./custody-codec.js";
import { recoverTerminalAuthorityReceipt } from "./terminal-receipt-authority.js";
import { type LifecycleCustodyHead, type LifecycleRotationRepository } from "./rotation-repository.js";
import type { LifecycleReceiptRepository } from "./receipt-repository.js";
import type { LifecycleDigest, LifecycleIntegrityReceiptAuthorityPort } from "./receipt-authority.js";
import type { LifecycleCheckpoint } from "../core/contracts.js";

export class LifecycleAdoptionSourceVectorDriftError extends Error {
  constructor(readonly expectedDigest: string, readonly observedDigest: string) {
    super("lifecycle accepted source vector changed before adoption apply");
  }
}

export type LifecycleFinalizeInput = Readonly<{
  runId: string;
  agentId: string;
  custodyId: string;
  adapterId: string;
  actionId: string;
  sourceActionId: string;
  sourceCapabilityHash: string;
  sourceProviderSessionRef: string;
  stagedCapabilityHash: string;
  checkpointSha256?: string;
  lifecycleInput?: {
    action: "compact" | "rotate";
    agentId: string;
    taskId: string;
    taskRevision: number;
    checkpoint: LifecycleCheckpoint;
    commandId: string;
  };
}>;

export type LifecycleFinalizeTerminal = Readonly<{
  disposition: "adopted" | "no-effect" | "superseded" | "quarantined";
  proofKind: "provider-terminal" | "zero-dispatch-no-effect" | "predispatch-superseded" |
    "postterminal-adoption-cas-superseded" | "integrity-quarantine";
  transitionProof: Readonly<Record<string, unknown>>;
}>;

type AdmissionSourceVectorDigestPort = (
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

type CheckpointPolicyRecordOperationPort = (
  runId: string,
  lifecycleInput: {
    action: "compact" | "rotate";
    agentId: string;
    taskId: string;
    taskRevision: number;
    checkpoint: LifecycleCheckpoint;
    commandId: string;
  },
  priorResumeReference: string | null,
  replacementResumeReference: string | null,
) => void;

// Behaviour-preserving extraction of Fabric's private #finalizeLifecycleRotationAdopted (plus
// its co-located LifecycleAdoptionSourceVectorDriftError). Body unchanged; calls into
// Fabric-private state that stays behind (admission.sourceVectorDigest, checkpoint policy's
// recordOperation) are narrow injected function ports. Postterminal CAS-supersession recursion
// stays private (self-recursion, not via Fabric).
export class LifecycleFinalizer {
  readonly #database: Database.Database;
  readonly #clock: () => number;
  readonly #fault: (label: string) => void;
  readonly #lifecycleReceiptAuthority: LifecycleIntegrityReceiptAuthorityPort | undefined;
  readonly #lifecycleReceipts: LifecycleReceiptRepository;
  readonly #lifecycleRotations: LifecycleRotationRepository;
  readonly #admissionSourceVectorDigest: AdmissionSourceVectorDigestPort;
  readonly #checkpointPolicyRecordOperation: CheckpointPolicyRecordOperationPort;

  constructor(dependencies: Readonly<{
    database: Database.Database;
    clock: () => number;
    fault: (label: string) => void;
    lifecycleReceiptAuthority: LifecycleIntegrityReceiptAuthorityPort | undefined;
    lifecycleReceipts: LifecycleReceiptRepository;
    lifecycleRotations: LifecycleRotationRepository;
    admissionSourceVectorDigest: AdmissionSourceVectorDigestPort;
    checkpointPolicyRecordOperation: CheckpointPolicyRecordOperationPort;
  }>) {
    this.#database = dependencies.database;
    this.#clock = dependencies.clock;
    this.#fault = dependencies.fault;
    this.#lifecycleReceiptAuthority = dependencies.lifecycleReceiptAuthority;
    this.#lifecycleReceipts = dependencies.lifecycleReceipts;
    this.#lifecycleRotations = dependencies.lifecycleRotations;
    this.#admissionSourceVectorDigest = dependencies.admissionSourceVectorDigest;
    this.#checkpointPolicyRecordOperation = dependencies.checkpointPolicyRecordOperation;
  }

  async finalizeRotationAdopted(
    input: LifecycleFinalizeInput,
    head: LifecycleCustodyHead,
    terminalEvidenceDigest: string,
    result: AgentProvisionProviderResult | null,
    terminal: LifecycleFinalizeTerminal = {
      disposition: "adopted",
      proofKind: "provider-terminal",
      transitionProof: { schemaVersion: 1, kind: "provider-terminal" },
    },
  ): Promise<void> {
    assertProviderActionOwner(this.#database, {
      runId: input.runId,
      adapterId: input.adapterId,
      actionId: input.actionId,
    }, "lifecycle");
    if (terminal.disposition === "adopted" && (result === null || input.lifecycleInput === undefined || input.checkpointSha256 === undefined)) {
      throw new FabricError("LIFECYCLE_PRECONDITION_FAILED", "lifecycle adoption lost its immutable input");
    }
    const authority = this.#lifecycleReceiptAuthority;
    if (authority === undefined) {
      throw new FabricError(
        "CAPABILITY_UNAVAILABLE",
        "lifecycle terminal apply requires an external receipt authority",
      );
    }
    const scopeHead = rowOrNotFound(this.#database.prepare(`
      SELECT scope.authority_id,head.checkpoint_digest,head.revision
        FROM lifecycle_admitted_run_scopes scope
        JOIN lifecycle_receipt_scope_heads head
          ON head.project_session_id=scope.project_session_id AND head.run_id=scope.run_id
       WHERE scope.project_session_id=? AND scope.run_id=?
    `).get(head.projectSessionId, input.runId), "admitted lifecycle receipt scope");
    const applyId = terminal.proofKind === "postterminal-adoption-cas-superseded"
      ? `${input.custodyId}:apply:postterminal-superseded`
      : `${input.custodyId}:apply`;
    const transitionProof = {
      ...terminal.transitionProof,
      actionRef: { adapterId: input.adapterId, actionId: input.actionId },
      terminalEvidenceDigest,
    };
    const custodyOwner = rowOrNotFound(this.#database.prepare(`
      SELECT bridge_owner_kind,target_provider_generation,target_principal_generation,
             target_bridge_generation,staged_capability_hash,
             source_adapter_id,source_custody_action_id,source_adapter_contract_digest,
             source_provider_session_ref,source_provider_generation,
             source_principal_generation,source_bridge_generation,source_bridge_revision,
             precondition_digest,launch_attest_challenge_digest,checkpoint_digest
        FROM lifecycle_rotation_custodies
       WHERE run_id=? AND agent_id=? AND custody_id=?
    `).get(input.runId, input.agentId, input.custodyId), "lifecycle custody owner");
    const bridgeOwnerKind = stringField(custodyOwner, "bridge_owner_kind") as "chair" | "child";
    const targetProviderGeneration = numberField(custodyOwner, "target_provider_generation");
    const targetPrincipalGeneration = numberField(custodyOwner, "target_principal_generation");
    const targetBridgeGeneration = numberField(custodyOwner, "target_bridge_generation");
    const custodyWriteLeases = this.#database.prepare(`
      SELECT ownership.lease_id AS leaseId,ownership.lease_generation AS leaseGeneration,
             lease.status,lease.generation
        FROM lifecycle_custody_write_leases ownership
        JOIN leases lease ON lease.lease_id=ownership.lease_id AND lease.run_id=ownership.run_id
       WHERE ownership.run_id=? AND ownership.agent_id=? AND ownership.custody_id=?
         AND ownership.active_owner=1
       ORDER BY ownership.ordinal
    `).all(input.runId, input.agentId, input.custodyId).map((candidate) =>
      rowOrNotFound(candidate, "lifecycle custody write lease"));
    const custodyAdoptionDeliveries = this.#database.prepare(`
      SELECT ownership.delivery_id AS deliveryId,
             ownership.delivery_generation AS claimGeneration,
             ownership.recipient_agent_id AS requesterAgentId,
             ownership.source_state AS sourceState,
             delivery.claim_generation AS liveClaimGeneration,
             delivery.target_provider_session AS targetProviderSession,
             delivery.state,delivery.revision
        FROM lifecycle_custody_adoption_deliveries ownership
        JOIN result_deliveries delivery
          ON delivery.result_delivery_id=ownership.delivery_id AND delivery.run_id=ownership.run_id
       WHERE ownership.run_id=? AND ownership.agent_id=? AND ownership.custody_id=?
         AND ownership.active_owner=1
       ORDER BY ownership.ordinal
    `).all(input.runId, input.agentId, input.custodyId).map((candidate) =>
      rowOrNotFound(candidate, "lifecycle custody adoption delivery"));
    if (terminal.disposition === "adopted") {
      for (const delivery of custodyAdoptionDeliveries) {
        if (
          !["claimed", "provider-accepted"].includes(stringField(delivery, "state")) ||
          numberField(delivery, "liveClaimGeneration") !== numberField(delivery, "claimGeneration") ||
          stringField(delivery, "requesterAgentId") !== input.agentId ||
          stringField(delivery, "targetProviderSession") !== input.sourceProviderSessionRef
        ) {
          throw new FabricError(
            "LIFECYCLE_PRECONDITION_FAILED",
            "captured lifecycle adoption delivery left its eligible source state",
          );
        }
      }
    }
    const mutationWrite = (
      relation: string,
      key: string,
      operation: "insert" | "update" | "delete",
      before: Readonly<Record<string, unknown>> | null,
      after: Readonly<Record<string, unknown>> | null,
    ) => {
      if ((operation === "insert") !== (before === null) || (operation === "delete") !== (after === null)) {
        throw new Error("lifecycle mutation plan row state is crossed");
      }
      return {
        relation,
        keyDigest: lifecycleDigest("mutation-key", { schemaVersion: 1, relation, key }),
        operation,
        expectedSemanticDigest: before === null ? null : lifecycleDigest("mutation-before", before),
        afterSemanticJcs: after === null ? null : canonicalJson(after),
        afterSemanticDigest: after === null ? null : lifecycleDigest("mutation-after", after),
      };
    };
    const agentBefore = rowOrNotFound(this.#database.prepare(`
      SELECT lifecycle,provider_session_ref FROM agents WHERE run_id=? AND agent_id=?
    `).get(input.runId, input.agentId), "lifecycle mutation agent source");
    const actionBefore = rowOrNotFound(this.#database.prepare(`
      SELECT status,execution_count,effect_count,idempotency_proven
        FROM provider_actions WHERE run_id=? AND adapter_id=? AND action_id=?
    `).get(input.runId, input.adapterId, input.actionId), "lifecycle mutation provider action source");
    const freezeReason = `lifecycle-rotation:${sha256(input.custodyId).slice(0, 32)}`;
    const freezeBeforeValue = this.#database.prepare(`
      SELECT reason FROM delivery_freezes WHERE run_id=? AND agent_id=? AND reason=?
    `).get(input.runId, input.agentId, freezeReason);
    const freezeBefore = isRow(freezeBeforeValue) ? freezeBeforeValue : null;
    const mutationWrites: Array<ReturnType<typeof mutationWrite>> = [];
    const localWrites: Array<Readonly<{
      relation: string;
      key: string;
      operation: "insert" | "update" | "delete";
    }>> = [];
    const auxiliaryLocalWrites: Array<Readonly<{
      relation: string;
      key: string;
      operation: "insert" | "update" | "delete";
    }>> = [];
    if (terminal.disposition === "adopted") {
      const adopted = result as AgentProvisionProviderResult;
      const lifecycleInput = input.lifecycleInput as NonNullable<typeof input.lifecycleInput>;
      const bridgeBefore = bridgeOwnerKind === "chair"
        ? rowOrNotFound(this.#database.prepare(`
            SELECT provider_adapter_id AS adapterId,provider_action_id AS actionId,
                   provider_session_ref AS providerSessionRef,
                   provider_session_generation AS providerGeneration,
                   principal_generation AS principalGeneration,
                   bridge_generation AS bridgeGeneration,capability_hash AS capabilityHash,
                   activation_evidence_digest AS activationEvidenceDigest,revision
              FROM launched_chair_bridge_state
             WHERE project_session_id=(SELECT project_session_id FROM runs WHERE run_id=?)
               AND coordination_run_id=? AND chair_agent_id=? AND state='active'
          `).get(input.runId, input.runId, input.agentId), "lifecycle chair bridge source")
        : rowOrNotFound(this.#database.prepare(`
            SELECT adapter_id AS adapterId,action_id AS actionId,
                   provider_session_ref AS providerSessionRef,
                   provider_session_generation AS providerGeneration,
                   bridge_generation AS bridgeGeneration,capability_hash AS capabilityHash,
                   activation_evidence_digest AS activationEvidenceDigest,revision
              FROM agent_bridge_state
             WHERE run_id=? AND agent_id=? AND bridge_state='active'
          `).get(input.runId, input.agentId), "lifecycle child bridge source");
      const bridgeAfter = {
        adapterId: input.adapterId,
        actionId: input.actionId,
        providerSessionRef: adopted.providerSessionRef,
        providerGeneration: targetProviderGeneration,
        ...(bridgeOwnerKind === "chair" ? { principalGeneration: targetPrincipalGeneration } : {}),
        bridgeGeneration: targetBridgeGeneration,
        capabilityHash: input.stagedCapabilityHash,
        activationEvidenceDigest: adopted.activationEvidenceDigest,
        revision: numberField(bridgeBefore, "revision") + 1,
      };
      mutationWrites.push(mutationWrite(
        bridgeOwnerKind === "chair" ? "chair-bridge" : "agent-bridge",
        `${input.runId}:${input.agentId}`,
        "update",
        bridgeBefore,
        bridgeAfter,
      ));
      localWrites.push({
        relation: bridgeOwnerKind === "chair" ? "chair-bridge" : "agent-bridge",
        key: `${input.runId}:${input.agentId}`,
        operation: "update",
      });
      const capabilityBefore = rowOrNotFound(this.#database.prepare(`
        SELECT principal_generation AS principalGeneration,
               CASE WHEN revoked_at IS NULL THEN 0 ELSE 1 END AS revoked
          FROM capabilities WHERE token_hash=?
      `).get(input.sourceCapabilityHash), "lifecycle source capability");
      mutationWrites.push(mutationWrite(
        "principal-capability",
        input.sourceCapabilityHash,
        "update",
        capabilityBefore,
        { principalGeneration: numberField(capabilityBefore, "principalGeneration"), revoked: 1 },
      ));
      localWrites.push({ relation: "principal-capability", key: input.sourceCapabilityHash, operation: "update" });
      mutationWrites.push(mutationWrite(
        "agent-state",
        `${input.runId}:${input.agentId}`,
        "update",
        agentBefore,
        { lifecycle: "ready", provider_session_ref: adopted.providerSessionRef },
      ));
      localWrites.push({ relation: "agent-state", key: `${input.runId}:${input.agentId}`, operation: "update" });
      const providerBeforeValue = this.#database.prepare(`
        SELECT provider_session_generation AS providerGeneration,context_revision AS contextRevision,
               reconciled_checkpoint_sha256 AS checkpointSha256
          FROM provider_state WHERE run_id=? AND agent_id=?
      `).get(input.runId, input.agentId);
      const providerBefore = isRow(providerBeforeValue) ? providerBeforeValue : null;
      mutationWrites.push(mutationWrite(
        "provider-session",
        `${input.runId}:${input.agentId}`,
        providerBefore === null ? "insert" : "update",
        providerBefore,
        { providerGeneration: targetProviderGeneration, contextRevision: 0, checkpointSha256: input.checkpointSha256 },
      ));
      localWrites.push({
        relation: "provider-session",
        key: `${input.runId}:${input.agentId}`,
        operation: providerBefore === null ? "insert" : "update",
      });
      mutationWrites.push(mutationWrite(
        "audit",
        `${input.runId}:${lifecycleInput.commandId}`,
        "insert",
        null,
        {
          agentId: input.agentId,
          action: lifecycleInput.action,
          taskId: lifecycleInput.taskId,
          taskRevision: lifecycleInput.taskRevision,
          checkpointSha256: input.checkpointSha256,
          priorResumeReference: input.sourceProviderSessionRef,
          replacementResumeReference: adopted.providerSessionRef,
        },
      ));
      localWrites.push({
        relation: "audit",
        key: `${input.runId}:${lifecycleInput.commandId}`,
        operation: "insert",
      });
    } else {
      const stagedCapabilityValue = this.#database.prepare(`
        SELECT principal_generation AS principalGeneration,
               CASE WHEN revoked_at IS NULL THEN 0 ELSE 1 END AS revoked
          FROM capabilities WHERE token_hash=? AND revoked_at IS NULL
      `).get(input.stagedCapabilityHash);
      if (isRow(stagedCapabilityValue)) {
        mutationWrites.push(mutationWrite(
          "principal-capability",
          input.stagedCapabilityHash,
          "update",
          stagedCapabilityValue,
          { principalGeneration: numberField(stagedCapabilityValue, "principalGeneration"), revoked: 1 },
        ));
        localWrites.push({
          relation: "principal-capability",
          key: input.stagedCapabilityHash,
          operation: "update",
        });
      }
      const terminalAction = terminal.disposition === "quarantined"
        ? { status: "quarantined", execution_count: actionBefore.execution_count,
            effect_count: actionBefore.effect_count, idempotency_proven: 0 }
        : { status: "terminal", execution_count: terminal.disposition === "no-effect" ? 0 : actionBefore.execution_count,
            effect_count: terminal.disposition === "no-effect" ? 0 : actionBefore.effect_count, idempotency_proven: 1 };
      if (terminal.disposition !== "superseded" || actionBefore.status === "prepared") {
        mutationWrites.push(mutationWrite(
          "provider-action",
          `${input.runId}:${input.adapterId}:${input.actionId}`,
          "update",
          actionBefore,
          terminalAction,
        ));
        localWrites.push({
          relation: "provider-action",
          key: `${input.runId}:${input.adapterId}:${input.actionId}`,
          operation: "update",
        });
      }
      if (terminal.disposition !== "quarantined") {
        mutationWrites.push(mutationWrite(
          "agent-state",
          `${input.runId}:${input.agentId}`,
          "update",
          agentBefore,
          { lifecycle: "ready", provider_session_ref: agentBefore.provider_session_ref },
        ));
        localWrites.push({ relation: "agent-state", key: `${input.runId}:${input.agentId}`, operation: "update" });
      }
    }
    if (terminal.disposition !== "quarantined" && freezeBefore !== null) {
      mutationWrites.push(mutationWrite(
        "freeze-owner",
        `${input.runId}:${input.agentId}`,
        "delete",
        freezeBefore,
        null,
      ));
      localWrites.push({ relation: "freeze-owner", key: `${input.runId}:${input.agentId}`, operation: "delete" });
    }
    if (terminal.disposition === "adopted") {
      const adopted = result as AgentProvisionProviderResult;
      for (const delivery of custodyAdoptionDeliveries) {
        const key = stringField(delivery, "deliveryId");
        const before = {
          state: stringField(delivery, "state"),
          claimGeneration: numberField(delivery, "claimGeneration"),
          targetProviderSession: stringField(delivery, "targetProviderSession"),
          revision: numberField(delivery, "revision"),
        };
        mutationWrites.push(mutationWrite("delivery", key, "update", before, {
          ...before,
          targetProviderSession: adopted.providerSessionRef,
          revision: before.revision + 1,
        }));
        localWrites.push({ relation: "delivery", key, operation: "update" });
        auxiliaryLocalWrites.push({
          relation: "lifecycle_custody_adoption_deliveries",
          key: `${input.runId}:${input.agentId}:${input.custodyId}:${key}:${before.claimGeneration}`,
          operation: "update",
        });
      }
      for (const lease of custodyWriteLeases) {
        auxiliaryLocalWrites.push({
          relation: "lifecycle_custody_write_leases",
          key: `${input.runId}:${input.agentId}:${input.custodyId}:${stringField(lease, "leaseId")}:${numberField(lease, "leaseGeneration")}`,
          operation: "update",
        });
      }
    } else if (terminal.disposition === "no-effect" || terminal.disposition === "superseded") {
      for (const lease of custodyWriteLeases) {
        const key = stringField(lease, "leaseId");
        const before = {
          status: stringField(lease, "status"),
          generation: numberField(lease, "generation"),
        };
        mutationWrites.push(mutationWrite("write-lease", key, "update", before, {
          status: "active",
          generation: before.generation,
        }));
        localWrites.push({ relation: "write-lease", key, operation: "update" });
        auxiliaryLocalWrites.push({
          relation: "lifecycle_custody_write_leases",
          key: `${input.runId}:${input.agentId}:${input.custodyId}:${key}:${before.generation}`,
          operation: "update",
        });
      }
      for (const delivery of custodyAdoptionDeliveries) {
        auxiliaryLocalWrites.push({
          relation: "lifecycle_custody_adoption_deliveries",
          key: `${input.runId}:${input.agentId}:${input.custodyId}:${stringField(delivery, "deliveryId")}:${numberField(delivery, "claimGeneration")}`,
          operation: "update",
        });
      }
    }
    if (terminal.disposition === "quarantined") {
      for (const lease of custodyWriteLeases) {
        auxiliaryLocalWrites.push({
          relation: "lifecycle_custody_write_leases",
          key: `${input.runId}:${input.agentId}:${input.custodyId}:${stringField(lease, "leaseId")}:${numberField(lease, "leaseGeneration")}`,
          operation: "update",
        });
      }
    }
    mutationWrites.sort((left, right) => canonicalJson(left).localeCompare(canonicalJson(right)));
    const exactMutationPlan = {
      schemaVersion: 1 as const,
      writes: mutationWrites,
      writeSetDigest: lifecycleDigest("mutation-plan", { schemaVersion: 1, writes: mutationWrites }),
    };
    const mutationPlan = {
      schemaVersion: 1,
      writes: mutationWrites,
      writeSetDigest: exactMutationPlan.writeSetDigest,
    };
    const revalidateMutationWrites = (): void => {
      if (terminal.disposition === "adopted") {
        const lifecycleInput = input.lifecycleInput as NonNullable<typeof input.lifecycleInput>;
        const observedSourceVector = this.#admissionSourceVectorDigest(
          input.runId,
          input.agentId,
          lifecycleInput.taskId,
          lifecycleInput.checkpoint,
          custodyAdoptionDeliveries.map((delivery) => ({
            deliveryId: stringField(delivery, "deliveryId"),
            claimGeneration: numberField(delivery, "claimGeneration"),
            requesterAgentId: stringField(delivery, "requesterAgentId"),
            targetProviderSession: input.sourceProviderSessionRef,
          })),
        );
        const expectedSourceVector = stringField(custodyOwner, "precondition_digest");
        if (observedSourceVector !== expectedSourceVector) {
          throw new LifecycleAdoptionSourceVectorDriftError(expectedSourceVector, observedSourceVector);
        }
      }
      const observed = new Map<string, Readonly<Record<string, unknown>> | null>();
      observed.set("agent-state", rowOrNotFound(this.#database.prepare(`
        SELECT lifecycle,provider_session_ref FROM agents WHERE run_id=? AND agent_id=?
      `).get(input.runId, input.agentId), "lifecycle mutation agent revalidation"));
      observed.set("provider-action", rowOrNotFound(this.#database.prepare(`
        SELECT status,execution_count,effect_count,idempotency_proven
          FROM provider_actions WHERE run_id=? AND adapter_id=? AND action_id=?
      `).get(input.runId, input.adapterId, input.actionId), "lifecycle mutation action revalidation"));
      const currentFreeze = this.#database.prepare(`
        SELECT reason FROM delivery_freezes WHERE run_id=? AND agent_id=? AND reason=?
      `).get(input.runId, input.agentId, freezeReason);
      observed.set("freeze-owner", isRow(currentFreeze) ? currentFreeze : null);
      const capabilityHash = terminal.disposition === "adopted"
        ? input.sourceCapabilityHash
        : input.stagedCapabilityHash;
      const currentCapability = this.#database.prepare(`
        SELECT principal_generation AS principalGeneration,
               CASE WHEN revoked_at IS NULL THEN 0 ELSE 1 END AS revoked
          FROM capabilities WHERE token_hash=?
      `).get(capabilityHash);
      observed.set("principal-capability", isRow(currentCapability) ? currentCapability : null);
      if (terminal.disposition === "adopted") {
        const currentBridge = bridgeOwnerKind === "chair"
          ? this.#database.prepare(`
              SELECT provider_adapter_id AS adapterId,provider_action_id AS actionId,
                     provider_session_ref AS providerSessionRef,
                     provider_session_generation AS providerGeneration,
                     principal_generation AS principalGeneration,
                     bridge_generation AS bridgeGeneration,capability_hash AS capabilityHash,
                     activation_evidence_digest AS activationEvidenceDigest,revision
                FROM launched_chair_bridge_state
               WHERE project_session_id=(SELECT project_session_id FROM runs WHERE run_id=?)
                 AND coordination_run_id=? AND chair_agent_id=? AND state='active'
            `).get(input.runId, input.runId, input.agentId)
          : this.#database.prepare(`
              SELECT adapter_id AS adapterId,action_id AS actionId,
                     provider_session_ref AS providerSessionRef,
                     provider_session_generation AS providerGeneration,
                     bridge_generation AS bridgeGeneration,capability_hash AS capabilityHash,
                     activation_evidence_digest AS activationEvidenceDigest,revision
                FROM agent_bridge_state
               WHERE run_id=? AND agent_id=? AND bridge_state='active'
            `).get(input.runId, input.agentId);
        observed.set(bridgeOwnerKind === "chair" ? "chair-bridge" : "agent-bridge",
          isRow(currentBridge) ? currentBridge : null);
        const currentProvider = this.#database.prepare(`
          SELECT provider_session_generation AS providerGeneration,context_revision AS contextRevision,
                 reconciled_checkpoint_sha256 AS checkpointSha256
            FROM provider_state WHERE run_id=? AND agent_id=?
        `).get(input.runId, input.agentId);
        observed.set("provider-session", isRow(currentProvider) ? currentProvider : null);
        const lifecycleInput = input.lifecycleInput as NonNullable<typeof input.lifecycleInput>;
        const currentAudit = this.#database.prepare(`
          SELECT operation_id FROM lifecycle_operations
           WHERE run_id=? AND agent_id=? AND action=? AND task_id=? AND task_revision=?
             AND checkpoint_sha256=? AND prior_resume_reference=?
        `).get(
          input.runId,
          input.agentId,
          lifecycleInput.action,
          lifecycleInput.taskId,
          lifecycleInput.taskRevision,
          input.checkpointSha256,
          input.sourceProviderSessionRef,
        );
        observed.set("audit", isRow(currentAudit) ? currentAudit : null);
      }
      for (const write of mutationWrites) {
        let current = observed.get(write.relation) ?? null;
        if (write.relation === "delivery") {
          const delivery = custodyAdoptionDeliveries.find((candidate) =>
            lifecycleDigest("mutation-key", {
              schemaVersion: 1,
              relation: "delivery",
              key: stringField(candidate, "deliveryId"),
            }) === write.keyDigest);
          const live = delivery === undefined ? undefined : this.#database.prepare(`
            SELECT state,claim_generation AS claimGeneration,
                   target_provider_session AS targetProviderSession,revision
              FROM result_deliveries WHERE result_delivery_id=? AND run_id=?
          `).get(stringField(delivery, "deliveryId"), input.runId);
          current = isRow(live) ? live : null;
        } else if (write.relation === "write-lease") {
          const lease = custodyWriteLeases.find((candidate) =>
            lifecycleDigest("mutation-key", {
              schemaVersion: 1,
              relation: "write-lease",
              key: stringField(candidate, "leaseId"),
            }) === write.keyDigest);
          const live = lease === undefined ? undefined : this.#database.prepare(`
            SELECT status,generation FROM leases WHERE lease_id=? AND run_id=?
          `).get(stringField(lease, "leaseId"), input.runId);
          current = isRow(live) ? live : null;
        } else if (!observed.has(write.relation)) {
          throw new Error(`lifecycle mutation revalidation lacks ${write.relation}`);
        }
        const currentDigest = current === null ? null : lifecycleDigest("mutation-before", current);
        if (currentDigest !== write.expectedSemanticDigest) {
          throw new Error(`lifecycle ${write.relation} changed after terminal preparation`);
        }
      }
    };
    const recordedAt = this.#clock();
    const prepared = this.#lifecycleReceipts.readPreparedChildCustodyTerminal(
      input.runId,
      input.agentId,
      input.custodyId,
      applyId,
    ) ?? this.#database.transaction(() =>
      this.#lifecycleReceipts.prepareChildCustodyTerminalInCurrentTransaction({
        runId: input.runId,
        agentId: input.agentId,
        custodyId: input.custodyId,
        expectedRevision: head.revision,
        applyId,
        transitionProof,
        mutationPlan,
        recordedAt,
        terminal: {
          disposition: terminal.disposition,
          proofKind: terminal.proofKind,
          terminalEvidenceDigest,
        },
      })
    ).immediate();
    if (
      prepared.applyId !== applyId ||
      prepared.preRevision !== head.revision ||
      prepared.disposition !== terminal.disposition ||
      prepared.proofKind !== terminal.proofKind ||
      prepared.terminalEvidenceDigest !== terminalEvidenceDigest ||
      canonicalJson(prepared.mutationPlan) !== canonicalJson(mutationPlan)
    ) {
      throw new FabricError("LIFECYCLE_PRECONDITION_FAILED", "prepared lifecycle terminal apply changed");
    }
    const { record, reviewRecord, checkpoint } = await recoverTerminalAuthorityReceipt(authority, prepared);
    try {
      this.#database.transaction(() => {
        this.#lifecycleReceipts.applyAuthorizedChildCustodyTerminalInCurrentTransaction({
        prepared,
        expectedRevision: head.revision,
        expectedScopeHead: {
          checkpointDigest: stringField(scopeHead, "checkpoint_digest"),
          revision: numberField(scopeHead, "revision"),
        },
        receipt: {
          authorityId: record.receipt.authorityId,
          authoritySequence: record.receipt.authoritySequence,
          previousReceiptDigest: record.receipt.previousReceiptDigest,
          receiptDigest: record.receipt.receiptDigest,
          attestation: record.receipt.attestation,
          verifiedAt: this.#clock(),
        },
        ...(reviewRecord === null ? {} : {
          reviewReceipt: {
            authorityId: reviewRecord.receipt.authorityId,
            authoritySequence: reviewRecord.receipt.authoritySequence,
            previousReceiptDigest: reviewRecord.receipt.previousReceiptDigest,
            receiptDigest: reviewRecord.receipt.receiptDigest,
            attestation: reviewRecord.receipt.attestation,
            verifiedAt: this.#clock(),
          },
        }),
        scopeCheckpoint: {
          receiptCount: checkpoint.receiptCount,
          headAuthoritySequence: checkpoint.headAuthoritySequence,
          headReceiptDigest: checkpoint.headReceiptDigest as LifecycleDigest,
          orderedRecordSetDigest: checkpoint.orderedRecordSetDigest,
          checkpointDigest: checkpoint.checkpointDigest,
          attestation: checkpoint.attestation,
          verifiedAt: this.#clock(),
        },
        authorizedAt: this.#clock(),
        appliedAt: this.#clock(),
        localWrites,
        auxiliaryLocalWrites,
        revalidateAdoptionWrites: revalidateMutationWrites,
        performAdoptionWrites: () => {
          if (terminal.disposition !== "adopted") {
            const now = this.#clock();
            if (terminal.disposition === "no-effect" || terminal.disposition === "superseded") {
              for (const lease of custodyWriteLeases) {
                const leaseId = stringField(lease, "leaseId");
                const generation = numberField(lease, "leaseGeneration");
                const restored = this.#database.prepare(`
                  UPDATE leases SET status='active',updated_at=?
                   WHERE lease_id=? AND run_id=? AND holder_agent_id=? AND kind='write'
                     AND generation=? AND status='quarantined'
                `).run(now, leaseId, input.runId, input.agentId, generation);
                if (restored.changes !== 1) throw new Error("lifecycle custody write lease changed before restore");
                const released = this.#database.prepare(`
                  UPDATE lifecycle_custody_write_leases SET active_owner=0
                   WHERE run_id=? AND agent_id=? AND custody_id=? AND lease_id=?
                     AND lease_generation=? AND active_owner=1
                `).run(input.runId, input.agentId, input.custodyId, leaseId, generation);
                if (released.changes !== 1) throw new Error("lifecycle custody write lease ownership changed");
              }
              for (const delivery of custodyAdoptionDeliveries) {
                const released = this.#database.prepare(`
                  UPDATE lifecycle_custody_adoption_deliveries SET active_owner=0
                   WHERE run_id=? AND agent_id=? AND custody_id=? AND delivery_id=?
                     AND delivery_generation=? AND active_owner=1
                `).run(
                  input.runId,
                  input.agentId,
                  input.custodyId,
                  stringField(delivery, "deliveryId"),
                  numberField(delivery, "claimGeneration"),
                );
                if (released.changes !== 1) throw new Error("lifecycle adoption delivery ownership changed");
              }
            }
            if (terminal.disposition === "quarantined") {
              for (const lease of custodyWriteLeases) {
                const released = this.#database.prepare(`
                  UPDATE lifecycle_custody_write_leases SET active_owner=0
                   WHERE run_id=? AND agent_id=? AND custody_id=? AND lease_id=?
                     AND lease_generation=? AND active_owner=1
                `).run(
                  input.runId,
                  input.agentId,
                  input.custodyId,
                  stringField(lease, "leaseId"),
                  numberField(lease, "leaseGeneration"),
                );
                if (released.changes !== 1) throw new Error("lifecycle custody write lease ownership changed");
              }
            }
            this.#database.prepare(`
              UPDATE capabilities SET revoked_at=?
               WHERE token_hash=? AND revoked_at IS NULL
            `).run(now, input.stagedCapabilityHash);
            if (terminal.disposition === "no-effect") {
              const proof = canonicalJson({ ...transitionProof, evidenceDigest: terminalEvidenceDigest });
              const action = this.#database.prepare(`
                UPDATE provider_actions
                   SET status='terminal',history_json='["prepared","terminal"]',
                       execution_count=0,effect_count=0,idempotency_proven=1,
                       result_json=?,journal_revision=journal_revision+1,updated_at=?
                 WHERE run_id=? AND adapter_id=? AND action_id=? AND status='prepared'
                   AND execution_count=0 AND effect_count=0
              `).run(proof, now, input.runId, input.adapterId, input.actionId);
              if (action.changes !== 1) throw new Error("lifecycle zero-dispatch action changed before apply");
              const agent = this.#database.prepare(`
                UPDATE agents SET lifecycle='ready'
                 WHERE run_id=? AND agent_id=? AND lifecycle='suspended'
              `).run(input.runId, input.agentId);
              if (agent.changes !== 1) throw new Error("lifecycle no-effect agent changed before apply");
              const freeze = this.#database.prepare(`
                DELETE FROM delivery_freezes WHERE run_id=? AND agent_id=? AND reason=?
              `).run(
                input.runId,
                input.agentId,
                `lifecycle-rotation:${sha256(input.custodyId).slice(0, 32)}`,
              );
              if (freeze.changes !== 1) throw new Error("lifecycle no-effect freeze changed before apply");
              return;
            }
            if (terminal.disposition === "superseded") {
              const proof = canonicalJson({ ...transitionProof, evidenceDigest: terminalEvidenceDigest });
              const preparedAction = this.#database.prepare(`
                UPDATE provider_actions
                   SET status='terminal',history_json='["prepared","terminal"]',
                       execution_count=0,effect_count=0,idempotency_proven=1,
                       result_json=?,journal_revision=journal_revision+1,updated_at=?
                 WHERE run_id=? AND adapter_id=? AND action_id=? AND status='prepared'
                   AND execution_count=0 AND effect_count=0
              `).run(proof, now, input.runId, input.adapterId, input.actionId);
              if (preparedAction.changes === 0) {
                const terminalAction = this.#database.prepare(`
                  SELECT status,execution_count,effect_count,idempotency_proven
                    FROM provider_actions
                   WHERE run_id=? AND adapter_id=? AND action_id=?
                `).get(input.runId, input.adapterId, input.actionId);
                if (
                  !isRow(terminalAction) || terminalAction.status !== "terminal" ||
                  terminalAction.execution_count !== 1 || terminalAction.effect_count !== 1 ||
                  terminalAction.idempotency_proven !== 1
                ) throw new Error("lifecycle superseded action changed before apply");
              }
              const agent = this.#database.prepare(`
                UPDATE agents SET lifecycle='ready'
                 WHERE run_id=? AND agent_id=? AND lifecycle='suspended'
              `).run(input.runId, input.agentId);
              if (agent.changes !== 1) throw new Error("lifecycle superseded agent changed before apply");
              const freeze = this.#database.prepare(`
                DELETE FROM delivery_freezes WHERE run_id=? AND agent_id=? AND reason=?
              `).run(
                input.runId,
                input.agentId,
                `lifecycle-rotation:${sha256(input.custodyId).slice(0, 32)}`,
              );
              if (freeze.changes !== 1) throw new Error("lifecycle superseded freeze changed before apply");
              return;
            }
            this.#database.prepare(`
              UPDATE provider_actions
                 SET status='quarantined',idempotency_proven=0,
                     result_json=?,journal_revision=journal_revision+1,updated_at=?
               WHERE run_id=? AND adapter_id=? AND action_id=?
                 AND status IN ('prepared','dispatched','accepted','ambiguous','terminal')
            `).run(
              canonicalJson({ ...transitionProof, evidenceDigest: terminalEvidenceDigest }),
              now,
              input.runId,
              input.adapterId,
              input.actionId,
            );
            return;
          }
          const adopted = result as AgentProvisionProviderResult;
          const lifecycleInput = input.lifecycleInput as NonNullable<typeof input.lifecycleInput>;
          for (const delivery of custodyAdoptionDeliveries) {
            const deliveryId = stringField(delivery, "deliveryId");
            const claimGeneration = numberField(delivery, "claimGeneration");
            const revision = numberField(delivery, "revision");
            const transferred = this.#database.prepare(`
              UPDATE result_deliveries
                 SET target_provider_session=?,revision=revision+1,updated_at=?
               WHERE result_delivery_id=? AND run_id=? AND requester_agent_id=?
                 AND state=? AND claim_generation=? AND target_provider_session=? AND revision=?
            `).run(
              adopted.providerSessionRef,
              this.#clock(),
              deliveryId,
              input.runId,
              stringField(delivery, "requesterAgentId"),
              stringField(delivery, "state"),
              claimGeneration,
              input.sourceProviderSessionRef,
              revision,
            );
            if (transferred.changes !== 1) throw new Error("lifecycle adoption delivery changed before transfer");
            const released = this.#database.prepare(`
              UPDATE lifecycle_custody_adoption_deliveries SET active_owner=0
               WHERE run_id=? AND agent_id=? AND custody_id=? AND delivery_id=?
                 AND delivery_generation=? AND active_owner=1
            `).run(input.runId, input.agentId, input.custodyId, deliveryId, claimGeneration);
            if (released.changes !== 1) throw new Error("lifecycle adoption delivery ownership changed");
          }
          const bridge = bridgeOwnerKind === "chair"
            ? this.#database.prepare(`
                UPDATE launched_chair_bridge_state
                   SET provider_adapter_id=?,provider_action_id=?,provider_session_ref=?,
                       provider_session_generation=?,principal_generation=?,bridge_generation=?,capability_hash=?,
                       activation_evidence_digest=?,revision=revision+1,updated_at=?
                 WHERE project_session_id=(SELECT project_session_id FROM runs WHERE run_id=?)
                   AND coordination_run_id=? AND chair_agent_id=? AND state='active'
                   AND provider_adapter_id=? AND provider_action_id=?
                   AND provider_contract_digest=? AND provider_session_ref=?
                   AND provider_session_generation=? AND principal_generation=?
                   AND bridge_generation=? AND capability_hash=? AND revision=?
              `).run(
                input.adapterId, input.actionId, adopted.providerSessionRef,
                adopted.providerSessionGeneration, targetPrincipalGeneration, adopted.bridgeGeneration,
                input.stagedCapabilityHash, adopted.activationEvidenceDigest, this.#clock(),
                input.runId, input.runId, input.agentId,
                stringField(custodyOwner, "source_adapter_id"),
                stringField(custodyOwner, "source_custody_action_id"),
                stringField(custodyOwner, "source_adapter_contract_digest"),
                stringField(custodyOwner, "source_provider_session_ref"),
                numberField(custodyOwner, "source_provider_generation"),
                numberField(custodyOwner, "source_principal_generation"),
                numberField(custodyOwner, "source_bridge_generation"),
                input.sourceCapabilityHash, numberField(custodyOwner, "source_bridge_revision"),
              )
            : this.#database.prepare(`
                UPDATE agent_bridge_state
                   SET adapter_id=?,action_id=?,provider_session_ref=?,provider_session_generation=?,
                       bridge_state='active',bridge_generation=?,capability_hash=?,
                       activation_evidence_digest=?,revision=revision+1,updated_at=?
                 WHERE run_id=? AND agent_id=? AND bridge_state='active'
                   AND adapter_id=? AND action_id=? AND provider_session_ref=?
                   AND provider_session_generation=? AND bridge_generation=?
                   AND capability_hash=? AND revision=?
              `).run(
                input.adapterId, input.actionId, adopted.providerSessionRef,
                adopted.providerSessionGeneration, adopted.bridgeGeneration,
                input.stagedCapabilityHash, adopted.activationEvidenceDigest, this.#clock(),
                input.runId, input.agentId,
                stringField(custodyOwner, "source_adapter_id"),
                stringField(custodyOwner, "source_custody_action_id"),
                stringField(custodyOwner, "source_provider_session_ref"),
                numberField(custodyOwner, "source_provider_generation"),
                numberField(custodyOwner, "source_bridge_generation"),
                input.sourceCapabilityHash, numberField(custodyOwner, "source_bridge_revision"),
              );
          if (bridge.changes !== 1) throw new Error("lifecycle source bridge changed before apply");
          const capability = this.#database.prepare(`
            UPDATE capabilities SET revoked_at=? WHERE token_hash=? AND revoked_at IS NULL
          `).run(this.#clock(), input.sourceCapabilityHash);
          if (capability.changes !== 1) throw new Error("lifecycle source capability changed before apply");
          const agent = this.#database.prepare(`
            UPDATE agents SET lifecycle='ready',provider_session_ref=?
             WHERE run_id=? AND agent_id=? AND lifecycle='suspended'
          `).run(adopted.providerSessionRef, input.runId, input.agentId);
          if (agent.changes !== 1) throw new Error("lifecycle agent changed before apply");
          this.#database.prepare(`
            INSERT INTO provider_state(
              run_id,agent_id,provider_session_generation,context_revision,
              reconciled_checkpoint_sha256
            ) VALUES (?,?,?,0,?)
            ON CONFLICT(run_id,agent_id) DO UPDATE SET
              provider_session_generation=excluded.provider_session_generation,
              context_revision=excluded.context_revision,
              reconciled_checkpoint_sha256=excluded.reconciled_checkpoint_sha256
          `).run(input.runId, input.agentId, adopted.providerSessionGeneration, input.checkpointSha256);
          const freeze = this.#database.prepare(`
            DELETE FROM delivery_freezes WHERE run_id=? AND agent_id=? AND reason=?
          `).run(
            input.runId,
            input.agentId,
            `lifecycle-rotation:${sha256(input.custodyId).slice(0, 32)}`,
          );
          if (freeze.changes !== 1) throw new Error("lifecycle delivery freeze changed before apply");
          for (const lease of custodyWriteLeases) {
            const released = this.#database.prepare(`
              UPDATE lifecycle_custody_write_leases SET active_owner=0
               WHERE run_id=? AND agent_id=? AND custody_id=? AND lease_id=?
                 AND lease_generation=? AND active_owner=1
            `).run(
              input.runId,
              input.agentId,
              input.custodyId,
              stringField(lease, "leaseId"),
              numberField(lease, "leaseGeneration"),
            );
            if (released.changes !== 1) throw new Error("lifecycle custody write lease ownership changed");
          }
          this.#checkpointPolicyRecordOperation(
            input.runId,
            lifecycleInput,
            input.sourceProviderSessionRef,
            adopted.providerSessionRef,
          );
        },
        });
      }).immediate();
    } catch (error: unknown) {
      if (terminal.disposition !== "adopted" || !(error instanceof LifecycleAdoptionSourceVectorDriftError)) {
        throw error;
      }
      const driftEvidence = {
        schemaVersion: 1,
        expectedSourceVectorDigest: error.expectedDigest,
        observedSourceVectorDigest: error.observedDigest,
      };
      const terminalObservationDigest = sha256Digest(result);
      const proof = {
        schemaVersion: 1,
        kind: "postterminal-adoption-cas-superseded",
        sourceState: head.state,
        expectedSourceJournalDigest: error.expectedDigest,
        observedSourceJournalDigest: error.observedDigest,
        expectedCheckpointDigest: stringField(custodyOwner, "checkpoint_digest"),
        observedCheckpointDigest: stringField(custodyOwner, "checkpoint_digest"),
        terminalObservationDigest,
        replacementCandidateDigest: terminalObservationDigest,
        expectedMutationPreconditionDigest: error.expectedDigest,
        failedCasEvidenceDigest: sha256Digest(driftEvidence),
      };
      this.#fault("lifecycle-rotation:after-authoritative-adoption-receipt");
      this.#database.transaction(() => {
        this.#lifecycleReceipts.persistVerifiedAuthorityReceiptInCurrentTransaction(
          prepared,
          record,
        );
      }).immediate();
      head = this.#database.transaction(() => this.#lifecycleRotations.appendInCurrentTransaction({
        runId: input.runId,
        agentId: input.agentId,
        custodyId: input.custodyId,
        expectedRevision: head.revision,
        state: "committing",
        terminalEvidenceDigest,
        recordedAt: this.#clock(),
      })).immediate();
      await this.finalizeRotationAdopted(
        input,
        head,
        terminalEvidenceDigest,
        null,
        {
          disposition: "superseded",
          proofKind: "postterminal-adoption-cas-superseded",
          transitionProof: proof,
        },
      );
    }
  }
}
