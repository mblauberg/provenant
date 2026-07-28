import type Database from "better-sqlite3";

import { canonicalJson, integer, isRow, row, text } from "../persistence/row-codec.js";
import {
  custodyRef,
  lifecycleDigest,
  revisionBody,
  type LifecycleCustodyDisposition,
  type LifecycleCustodyState,
} from "./custody-codec.js";

export type { LifecycleCustodyDisposition, LifecycleCustodyState } from "./custody-codec.js";

export type LifecycleCustodyHead = Readonly<{
  projectSessionId: string;
  runId: string;
  agentId: string;
  custodyId: string;
  revision: number;
  state: LifecycleCustodyState;
  disposition: LifecycleCustodyDisposition;
  semanticDigest: string;
  sourceRefDigest: string;
  journalDigest: string;
  terminal: boolean;
}>;

export type ReservedLifecycleGenerations = Readonly<{
  providerGeneration: number;
  principalGeneration: number;
  bridgeGeneration: number;
}>;

export type CreateLifecycleCustodyInput = Readonly<{
  projectSessionId: string;
  runId: string;
  agentId: string;
  custodyId: string;
  commandId: string;
  admissionDigest: string;
  actionRef: Readonly<{ adapterId: string; actionId: string }>;
  bridgeOwnerKind: "chair" | "child";
  callerTurnLeaseId: string;
  callerTurnGeneration: number;
  predecessorTurnSetDigest: string;
  quarantinedWriteSetDigest: string;
  deliveryCutWatermark: number;
  adoptionDeliverySetDigest: string;
  checkpointRef: string;
  checkpointDigest: string;
  taskRevision: number;
  mailboxRevision: number;
  childSetDigest: string;
  openWorkSetDigest: string;
  sourceProviderSessionRef: string;
  sourceCapabilityHash: string;
  sourceCustodyActionId: string;
  sourceAdapterId: string;
  sourceAdapterContractDigest: string;
  sourceBridgeRowId: string;
  sourceBridgeRevision: number;
  sourceProviderGeneration: number;
  sourcePrincipalGeneration: number;
  sourceBridgeGeneration: number;
  sourceProjectSessionGeneration: number;
  sourceRunGeneration: number;
  sourceChairLeaseGeneration: number;
  targetProviderGeneration: number;
  targetPrincipalGeneration: number;
  targetBridgeGeneration: number;
  replacementAdapterId: string;
  replacementContractDigest: string;
  stagedCapabilityHash: string;
  launchAttestChallengeDigest: string;
  preconditionDigest: string;
  createdAt: number;
}>;

type AppendInput = Readonly<{
  runId: string;
  agentId: string;
  custodyId: string;
  expectedRevision: number;
  state: Exclude<LifecycleCustodyState, "finalized">;
  terminalEvidenceDigest?: string;
  recordedAt: number;
}>;

export type FinalizeAuthorizedLifecycleCustodyInput = Readonly<{
  sourceHead: LifecycleCustodyHead;
  finalRevision: number;
  disposition: Extract<LifecycleCustodyDisposition, "adopted" | "no-effect" | "superseded" | "quarantined">;
  proofKind: "provider-terminal" | "zero-dispatch-no-effect" | "predispatch-superseded" |
    "postterminal-adoption-cas-superseded" | "integrity-quarantine";
  terminalEvidenceDigest: string;
  finalSemanticDigest: string;
  finalSourceRefDigest: string;
  authorityBatchId: string;
  authorityApplyId: string;
  authorityApplyDigest: string;
  recordedAt: number;
}>;



function custodyJournal(input: Readonly<{
  runId: string;
  agentId: string;
  custodyId: string;
  revision: number;
  priorJournalDigest: string | null;
  semanticDigest: string;
  sourceRefDigest: string;
  authorityBatchId?: string;
  authorityApplyId?: string;
  authorityApplyDigest?: string;
  recordedAt: number;
}>): Readonly<Record<string, unknown>> {
  return {
    schemaVersion: 1,
    ownerRef: {
      kind: "custody",
      custodyRef: custodyRef(input.runId, input.agentId, input.custodyId, input.revision),
      sourceRefDigest: input.sourceRefDigest,
    },
    priorJournalDigest: input.priorJournalDigest,
    semanticDigest: input.semanticDigest,
    sourceRefDigest: input.sourceRefDigest,
    authorityBatchId: input.authorityBatchId ?? null,
    authorityApplyId: input.authorityApplyId ?? null,
    authorityApplyDigest: input.authorityApplyDigest ?? null,
    originFreshApplyId: null,
    originFreshApplyDigest: null,
    recordedAt: input.recordedAt,
  };
}

export class LifecycleRotationRepository {
  readonly #database: Database.Database;

  constructor(database: Database.Database) {
    this.#database = database;
  }

  nextGenerations(input: Readonly<{
    runId: string;
    agentId: string;
    bridgeOwnerKind: "chair" | "child";
    sourceProviderGeneration: number;
    sourcePrincipalGeneration: number;
    sourceBridgeGeneration: number;
  }>): ReservedLifecycleGenerations {
    for (const [label, value] of [
      ["provider", input.sourceProviderGeneration],
      ["principal", input.sourcePrincipalGeneration],
      ["bridge", input.sourceBridgeGeneration],
    ] as const) {
      if (!Number.isSafeInteger(value) || value < 1) {
        throw new Error(`source ${label} generation must be a positive safe integer`);
      }
    }
    const identityValue = this.#database.prepare(`
      SELECT provider_generation,principal_generation,revision
        FROM agent_lifecycle_identity_high_water
       WHERE run_id=? AND agent_id=?
    `).get(input.runId, input.agentId);
    const bridgeValue = this.#database.prepare(`
      SELECT bridge_generation,revision
        FROM agent_lifecycle_bridge_high_water
       WHERE run_id=? AND agent_id=? AND bridge_owner_kind=?
    `).get(input.runId, input.agentId, input.bridgeOwnerKind);
    const identity = isRow(identityValue) ? identityValue : null;
    const bridge = isRow(bridgeValue) ? bridgeValue : null;
    const providerHighWater = identity === null
      ? input.sourceProviderGeneration
      : integer(identity, "provider_generation");
    const principalHighWater = identity === null
      ? input.sourcePrincipalGeneration
      : integer(identity, "principal_generation");
    const bridgeHighWater = bridge === null
      ? input.sourceBridgeGeneration
      : integer(bridge, "bridge_generation");
    if (principalHighWater < input.sourcePrincipalGeneration || bridgeHighWater < input.sourceBridgeGeneration) {
      throw new Error("lifecycle generation high-water is behind its authoritative source");
    }
    const next = (label: string, value: number): number => {
      if (value >= Number.MAX_SAFE_INTEGER) throw new Error(`${label} generation is exhausted`);
      return value + 1;
    };
    const reserved = {
      providerGeneration: next("provider", Math.max(providerHighWater, input.sourceProviderGeneration)),
      principalGeneration: next("principal", principalHighWater),
      bridgeGeneration: next("bridge", bridgeHighWater),
    };
    return reserved;
  }

  reserveNextGenerationsInCurrentTransaction(input: Readonly<{
    runId: string;
    agentId: string;
    bridgeOwnerKind: "chair" | "child";
    sourceProviderGeneration: number;
    sourcePrincipalGeneration: number;
    sourceBridgeGeneration: number;
  }>): ReservedLifecycleGenerations {
    if (!this.#database.inTransaction) throw new Error("lifecycle generation reservation requires a transaction");
    const reserved = this.nextGenerations(input);
    const identityValue = this.#database.prepare(`
      SELECT provider_generation,principal_generation,revision
        FROM agent_lifecycle_identity_high_water
       WHERE run_id=? AND agent_id=?
    `).get(input.runId, input.agentId);
    const bridgeValue = this.#database.prepare(`
      SELECT bridge_generation,revision
        FROM agent_lifecycle_bridge_high_water
       WHERE run_id=? AND agent_id=? AND bridge_owner_kind=?
    `).get(input.runId, input.agentId, input.bridgeOwnerKind);
    const identity = isRow(identityValue) ? identityValue : null;
    const bridge = isRow(bridgeValue) ? bridgeValue : null;
    const providerHighWater = identity === null
      ? input.sourceProviderGeneration
      : integer(identity, "provider_generation");
    const principalHighWater = identity === null
      ? input.sourcePrincipalGeneration
      : integer(identity, "principal_generation");
    const bridgeHighWater = bridge === null
      ? input.sourceBridgeGeneration
      : integer(bridge, "bridge_generation");
    if (identity === null) {
      this.#database.prepare(`
        INSERT INTO agent_lifecycle_identity_high_water(
          run_id,agent_id,provider_generation,principal_generation,revision
        ) VALUES (?,?,?,?,1)
      `).run(input.runId, input.agentId, reserved.providerGeneration, reserved.principalGeneration);
    } else {
      const changed = this.#database.prepare(`
        UPDATE agent_lifecycle_identity_high_water
           SET provider_generation=?,principal_generation=?,revision=revision+1
         WHERE run_id=? AND agent_id=? AND provider_generation=?
           AND principal_generation=? AND revision=?
      `).run(
        reserved.providerGeneration,
        reserved.principalGeneration,
        input.runId,
        input.agentId,
        providerHighWater,
        principalHighWater,
        integer(identity, "revision"),
      );
      if (changed.changes !== 1) throw new Error("lifecycle identity generation reservation compare-and-set failed");
    }
    if (bridge === null) {
      this.#database.prepare(`
        INSERT INTO agent_lifecycle_bridge_high_water(
          run_id,agent_id,bridge_owner_kind,bridge_generation,revision
        ) VALUES (?,?,?,?,1)
      `).run(input.runId, input.agentId, input.bridgeOwnerKind, reserved.bridgeGeneration);
    } else {
      const changed = this.#database.prepare(`
        UPDATE agent_lifecycle_bridge_high_water
           SET bridge_generation=?,revision=revision+1
         WHERE run_id=? AND agent_id=? AND bridge_owner_kind=?
           AND bridge_generation=? AND revision=?
      `).run(
        reserved.bridgeGeneration,
        input.runId,
        input.agentId,
        input.bridgeOwnerKind,
        bridgeHighWater,
        integer(bridge, "revision"),
      );
      if (changed.changes !== 1) throw new Error("lifecycle bridge generation reservation compare-and-set failed");
    }
    this.#database.prepare(`
      INSERT INTO agent_lifecycle_context_high_water(
        run_id,agent_id,provider_generation,context_revision,revision
      ) VALUES (?,?,?,0,1)
    `).run(input.runId, input.agentId, reserved.providerGeneration);
    return reserved;
  }

  createInCurrentTransaction(input: CreateLifecycleCustodyInput): LifecycleCustodyHead {
    if (!this.#database.inTransaction) throw new Error("lifecycle custody creation requires a transaction");
    const creation = {
      schemaVersion: 1,
      custodyId: input.custodyId,
      commandId: input.commandId,
      actionRef: input.actionRef,
      checkpointDigest: input.checkpointDigest,
      sourceProviderGeneration: input.sourceProviderGeneration,
      sourcePrincipalGeneration: input.sourcePrincipalGeneration,
      sourceBridgeGeneration: input.sourceBridgeGeneration,
      targetProviderGeneration: input.targetProviderGeneration,
      targetPrincipalGeneration: input.targetPrincipalGeneration,
      targetBridgeGeneration: input.targetBridgeGeneration,
    };
    const creationJson = canonicalJson(creation);
    const creationDigest = lifecycleDigest("custody-semantic", creation);
    this.#database.prepare(`
      INSERT INTO lifecycle_rotation_custodies(
        project_session_id,run_id,agent_id,custody_id,command_id,admission_digest,
        provider_action_adapter_id,provider_action_id,recovery_source_kind,
        recovery_from_custody_id,recovery_from_custody_revision,
        recovery_from_generation_loss_id,recovery_from_generation_loss_revision,
        recovery_source_ref_digest,recovery_source_journal_digest,bridge_owner_kind,
        caller_turn_lease_id,caller_turn_generation,predecessor_turn_set_digest,
        quarantined_write_set_digest,delivery_cut_watermark,adoption_delivery_set_digest,
        checkpoint_ref,checkpoint_digest,checkpoint_validation_revision,
        checkpoint_validation_digest,checkpoint_validation_key,task_revision,
        mailbox_revision,child_set_digest,open_work_set_digest,
        source_provider_session_ref,source_capability_hash,source_custody_action_id,
        source_adapter_id,source_adapter_contract_digest,source_bridge_row_id,
        source_bridge_revision,source_provider_generation,source_principal_generation,
        source_bridge_generation,source_project_session_generation,source_run_generation,
        source_chair_lease_generation,target_provider_generation,target_principal_generation,
        target_bridge_generation,replacement_adapter_id,replacement_contract_digest,
        staged_capability_hash,launch_attest_challenge_digest,precondition_digest,
        origin_fresh_handoff_id,origin_fresh_handoff_digest,origin_operation,
        origin_fresh_apply_plan_digest,creation_json,creation_digest,created_at
      ) VALUES (
        @projectSessionId,@runId,@agentId,@custodyId,@commandId,@admissionDigest,
        @actionAdapterId,@actionId,'none',NULL,NULL,NULL,NULL,NULL,NULL,@bridgeOwnerKind,
        @callerTurnLeaseId,@callerTurnGeneration,@predecessorTurnSetDigest,
        @quarantinedWriteSetDigest,@deliveryCutWatermark,@adoptionDeliverySetDigest,
        @checkpointRef,@checkpointDigest,1,NULL,'none',@taskRevision,@mailboxRevision,
        @childSetDigest,@openWorkSetDigest,@sourceProviderSessionRef,
        @sourceCapabilityHash,@sourceCustodyActionId,@sourceAdapterId,
        @sourceAdapterContractDigest,@sourceBridgeRowId,@sourceBridgeRevision,
        @sourceProviderGeneration,@sourcePrincipalGeneration,@sourceBridgeGeneration,
        @sourceProjectSessionGeneration,@sourceRunGeneration,@sourceChairLeaseGeneration,
        @targetProviderGeneration,@targetPrincipalGeneration,@targetBridgeGeneration,
        @replacementAdapterId,@replacementContractDigest,@stagedCapabilityHash,
        @launchAttestChallengeDigest,@preconditionDigest,NULL,NULL,NULL,NULL,
        @creationJson,@creationDigest,@createdAt
      )
    `).run({
      ...input,
      actionAdapterId: input.actionRef.adapterId,
      actionId: input.actionRef.actionId,
      creationJson,
      creationDigest,
    });
    const body = revisionBody({
      custodyId: input.custodyId,
      revision: 1,
      state: "awaiting-boundary",
      disposition: "none",
      terminalEvidenceDigest: null,
    });
    const semanticJson = canonicalJson(body);
    const semanticDigest = lifecycleDigest("custody-semantic", body);
    const sourceRefDigest = semanticDigest;
    const journal = custodyJournal({
      runId: input.runId,
      agentId: input.agentId,
      custodyId: input.custodyId,
      revision: 1,
      priorJournalDigest: null,
      semanticDigest,
      sourceRefDigest,
      recordedAt: input.createdAt,
    });
    const journalJson = canonicalJson(journal);
    const journalDigest = lifecycleDigest("custody-journal", journal);
    this.#database.prepare(`
      INSERT INTO lifecycle_rotation_custody_revisions(
        project_session_id,run_id,agent_id,custody_id,revision,prior_revision,
        prior_journal_digest,state,disposition_code,proof_kind,
        terminal_evidence_digest,semantic_json,semantic_digest,source_ref_digest,
        origin_fresh_apply_id,origin_fresh_apply_digest,receipt_batch_id,
        receipt_apply_id,receipt_apply_digest,journal_json,journal_digest,recorded_at
      ) VALUES (?,?,?,?,1,NULL,NULL,'awaiting-boundary','none','none',NULL,?,?,?,
                NULL,NULL,NULL,NULL,NULL,?,?,?)
    `).run(
      input.projectSessionId, input.runId, input.agentId, input.custodyId,
      semanticJson, semanticDigest, sourceRefDigest, journalJson, journalDigest,
      input.createdAt,
    );
    this.#database.prepare(`
      INSERT INTO lifecycle_rotation_custody_heads(
        project_session_id,run_id,agent_id,custody_id,current_revision,state,
        disposition_code,semantic_digest,source_ref_digest,journal_digest,terminal,
        head_revision
      ) VALUES (?,?,?,?,1,'awaiting-boundary','none',?,?,?,0,1)
    `).run(
      input.projectSessionId, input.runId, input.agentId, input.custodyId,
      semanticDigest, sourceRefDigest, journalDigest,
    );
    return this.readHead(input.runId, input.agentId, input.custodyId);
  }

  appendInCurrentTransaction(input: AppendInput): LifecycleCustodyHead {
    if (!this.#database.inTransaction) throw new Error("lifecycle revision append requires a transaction");
    const prior = this.readHead(input.runId, input.agentId, input.custodyId);
    if (prior.revision !== input.expectedRevision || prior.terminal) {
      throw new Error("lifecycle custody head changed");
    }
    const revision = prior.revision + 1;
    const terminalEvidenceDigest = input.terminalEvidenceDigest ?? null;
    const body = revisionBody({
      custodyId: input.custodyId,
      revision,
      state: input.state,
      disposition: "none",
      terminalEvidenceDigest,
    });
    const semanticJson = canonicalJson(body);
    const semanticDigest = lifecycleDigest("custody-semantic", body);
    const sourceRefDigest = semanticDigest;
    const journal = custodyJournal({
      runId: input.runId,
      agentId: input.agentId,
      custodyId: input.custodyId,
      revision,
      priorJournalDigest: prior.journalDigest,
      semanticDigest,
      sourceRefDigest,
      recordedAt: input.recordedAt,
    });
    const journalJson = canonicalJson(journal);
    const journalDigest = lifecycleDigest("custody-journal", journal);
    this.#database.prepare(`
      INSERT INTO lifecycle_rotation_custody_revisions(
        project_session_id,run_id,agent_id,custody_id,revision,prior_revision,
        prior_journal_digest,state,disposition_code,proof_kind,
        terminal_evidence_digest,semantic_json,semantic_digest,source_ref_digest,
        origin_fresh_apply_id,origin_fresh_apply_digest,receipt_batch_id,
        receipt_apply_id,receipt_apply_digest,journal_json,journal_digest,recorded_at
      ) VALUES (?,?,?,?,?,?,? ,?,'none','none',?,?,?, ?,NULL,NULL,NULL,NULL,NULL,?,?,?)
    `).run(
      prior.projectSessionId, input.runId, input.agentId, input.custodyId,
      revision, prior.revision, prior.journalDigest, input.state,
      terminalEvidenceDigest, semanticJson, semanticDigest, sourceRefDigest,
      journalJson, journalDigest, input.recordedAt,
    );
    const changed = this.#database.prepare(`
      UPDATE lifecycle_rotation_custody_heads
         SET current_revision=?,state=?,disposition_code='none',semantic_digest=?,
             source_ref_digest=?,journal_digest=?,terminal=0,head_revision=head_revision+1
       WHERE run_id=? AND agent_id=? AND custody_id=? AND current_revision=?
         AND journal_digest=? AND terminal=0
    `).run(
      revision, input.state, semanticDigest, sourceRefDigest, journalDigest,
      input.runId, input.agentId, input.custodyId, prior.revision,
      prior.journalDigest,
    );
    if (changed.changes !== 1) throw new Error("lifecycle custody head compare-and-set failed");
    return this.readHead(input.runId, input.agentId, input.custodyId);
  }

  finalizeAuthorizedLifecycleCustodyInCurrentTransaction(
    input: FinalizeAuthorizedLifecycleCustodyInput,
  ): void {
    if (!this.#database.inTransaction) throw new Error("lifecycle terminal apply requires a transaction");
    const head = input.sourceHead;
    const finalBody = revisionBody({
      custodyId: head.custodyId,
      revision: input.finalRevision,
      state: "finalized",
      disposition: input.disposition,
      proofKind: input.proofKind,
      terminalEvidenceDigest: input.terminalEvidenceDigest,
    });
    const finalSemanticDigest = lifecycleDigest("custody-semantic", finalBody);
    if (finalSemanticDigest !== input.finalSemanticDigest ||
        finalSemanticDigest !== input.finalSourceRefDigest) {
      throw new Error("prepared lifecycle terminal semantic changed");
    }
    const journal = custodyJournal({
      runId: head.runId,
      agentId: head.agentId,
      custodyId: head.custodyId,
      revision: input.finalRevision,
      priorJournalDigest: head.journalDigest,
      semanticDigest: finalSemanticDigest,
      sourceRefDigest: input.finalSourceRefDigest,
      authorityBatchId: input.authorityBatchId,
      authorityApplyId: input.authorityApplyId,
      authorityApplyDigest: input.authorityApplyDigest,
      recordedAt: input.recordedAt,
    });
    const journalJson = canonicalJson(journal);
    const journalDigest = lifecycleDigest("custody-journal", journal);
    this.#database.prepare(`
      INSERT INTO lifecycle_rotation_custody_revisions(
        project_session_id,run_id,agent_id,custody_id,revision,prior_revision,
        prior_journal_digest,state,disposition_code,proof_kind,terminal_evidence_digest,
        semantic_json,semantic_digest,source_ref_digest,origin_fresh_apply_id,
        origin_fresh_apply_digest,receipt_batch_id,receipt_apply_id,receipt_apply_digest,
        journal_json,journal_digest,recorded_at
      ) VALUES (?,?,?,?,?,?,?,'finalized',?,?,?,?,?,?,
                NULL,NULL,?,?,?,?,?,?)
    `).run(
      head.projectSessionId, head.runId, head.agentId, head.custodyId,
      input.finalRevision, head.revision, head.journalDigest,
      input.disposition, input.proofKind, input.terminalEvidenceDigest,
      canonicalJson(finalBody), finalSemanticDigest,
      input.finalSourceRefDigest, input.authorityBatchId, input.authorityApplyId,
      input.authorityApplyDigest, journalJson, journalDigest, input.recordedAt,
    );
    const headChanged = this.#database.prepare(`
      UPDATE lifecycle_rotation_custody_heads
         SET current_revision=?,state='finalized',disposition_code=?,
             semantic_digest=?,source_ref_digest=?,journal_digest=?,terminal=1,
             head_revision=head_revision+1
       WHERE run_id=? AND agent_id=? AND custody_id=? AND current_revision=?
         AND journal_digest=? AND terminal=0
    `).run(
      input.finalRevision, input.disposition, finalSemanticDigest, input.finalSourceRefDigest,
      journalDigest, head.runId, head.agentId, head.custodyId,
      head.revision, head.journalDigest,
    );
    if (headChanged.changes !== 1) throw new Error("lifecycle custody head compare-and-set failed");
  }

  readHead(runId: string, agentId: string, custodyId: string): LifecycleCustodyHead {
    const value = row(this.#database.prepare(`
      SELECT project_session_id,run_id,agent_id,custody_id,current_revision,state,
             disposition_code,semantic_digest,source_ref_digest,journal_digest,terminal
        FROM lifecycle_rotation_custody_heads
       WHERE run_id=? AND agent_id=? AND custody_id=?
    `).get(runId, agentId, custodyId), "lifecycle custody head");
    return {
      projectSessionId: text(value, "project_session_id"),
      runId: text(value, "run_id"),
      agentId: text(value, "agent_id"),
      custodyId: text(value, "custody_id"),
      revision: integer(value, "current_revision"),
      state: text(value, "state") as LifecycleCustodyState,
      disposition: text(value, "disposition_code") as LifecycleCustodyDisposition,
      semanticDigest: text(value, "semantic_digest"),
      sourceRefDigest: text(value, "source_ref_digest"),
      journalDigest: text(value, "journal_digest"),
      terminal: integer(value, "terminal") === 1,
    };
  }
}
