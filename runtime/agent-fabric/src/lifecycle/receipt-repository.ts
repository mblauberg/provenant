import type Database from "better-sqlite3";

import { canonicalJson, integer, isRow, row, text } from "../persistence/row-codec.js";
import {
  custodyRef,
  lifecycleDigest,
  revisionBody,
  type LifecycleCustodyDisposition,
  type LifecycleCustodyState,
} from "./custody-codec.js";
import type { LifecycleReceiptRecord } from "./receipt-authority.js";
import type { GenerationLossHead } from "./generation-loss-repository.js";
import {
  LifecycleRotationRepository,
  type LifecycleCustodyHead,
} from "./rotation-repository.js";
import { LifecycleReviewAdoptionStore, type ExternalReviewReceipt, type PreparedReviewAdoption } from "./review-adoption.js";
import {
  GenerationLossTerminalOwnerAdapter,
  mutationPlan,
  persistSingleOwnerTerminalPreparation,
  type LifecycleMutationPlan,
  type PreparedGenerationLossTerminal,
  type PrepareGenerationLossTerminalInput,
} from "./terminal-owner-receipt.js";

export type {
  PreparedGenerationLossTerminal,
  PrepareGenerationLossTerminalInput,
} from "./terminal-owner-receipt.js";


export type PreparedChildCustodyTerminal = Readonly<{
  projectSessionId: string;
  runId: string;
  agentId: string;
  custodyId: string;
  preRevision: number;
  finalRevision: number;
  applyId: string;
  batchId: string;
  effectDigest: string;
  ownerRefDigest: string;
  transitionReplayDigest: string;
  subject: Readonly<Record<string, unknown>>;
  subjectJson: string;
  subjectDigest: string;
  intent: Readonly<Record<string, unknown>>;
  intentJson: string;
  intentDigest: string;
  finalSemanticDigest: string;
  finalSourceRefDigest: string;
  fromState: Exclude<LifecycleCustodyState, "finalized">;
  disposition: Extract<LifecycleCustodyDisposition, "adopted" | "no-effect" | "superseded" | "quarantined">;
  proofKind: "provider-terminal" | "zero-dispatch-no-effect" | "predispatch-superseded" |
    "postterminal-adoption-cas-superseded" | "integrity-quarantine";
  terminalEvidenceDigest: string;
  ownerKind: "chair" | "child";
  mutationPlan: LifecycleMutationPlan;
  review: PreparedReviewAdoption | null;
}>;

export type PrepareChildCustodyTerminalInput = Readonly<{
  runId: string;
  agentId: string;
  custodyId: string;
  expectedRevision: number;
  applyId: string;
  transitionProof: Readonly<Record<string, unknown>>;
  mutationPlan: Readonly<Record<string, unknown>>;
  recordedAt: number;
  review?: Readonly<{
    commandId: string;
    lifecycleAdoptionEvidenceDigest: string;
    recordedAt: number;
  }>;
  terminal?: Readonly<{
    disposition: Extract<LifecycleCustodyDisposition, "adopted" | "no-effect" | "superseded" | "quarantined">;
    proofKind: "provider-terminal" | "zero-dispatch-no-effect" | "predispatch-superseded" |
      "postterminal-adoption-cas-superseded" | "integrity-quarantine";
    terminalEvidenceDigest: string;
  }>;
}>;

export type ExternallyVerifiedChildCustodyAuthorization = Readonly<{
  prepared: PreparedChildCustodyTerminal;
  expectedRevision: number;
  expectedScopeHead: Readonly<{ checkpointDigest: string; revision: number }>;
  receipt: Readonly<{
    authorityId: string;
    authoritySequence: number;
    previousReceiptDigest: string | null;
    receiptDigest: string;
    attestation: string;
    verifiedAt: number;
  }>;
  reviewReceipt?: ExternalReviewReceipt;
  scopeCheckpoint: Readonly<{
    receiptCount: number;
    headAuthoritySequence: number;
    headReceiptDigest: string;
    orderedRecordSetDigest: string;
    checkpointDigest: string;
    attestation: string;
    verifiedAt: number;
  }>;
  authorizedAt: number;
  appliedAt: number;
  localWrites: readonly Readonly<{
    relation: string;
    key: string;
    operation: "insert" | "update" | "delete";
  }>[];
  auxiliaryLocalWrites?: readonly Readonly<{
    relation: string;
    key: string;
    operation: "insert" | "update" | "delete";
  }>[];
  revalidateAdoptionWrites: () => void;
  performAdoptionWrites: () => void;
}>;

export type ExternallyVerifiedGenerationLossAuthorization = Readonly<{
  prepared: PreparedGenerationLossTerminal;
  expectedRevision: number;
  expectedScopeHead: Readonly<{ checkpointDigest: string; revision: number }>;
  receipt: ExternallyVerifiedChildCustodyAuthorization["receipt"];
  scopeCheckpoint: ExternallyVerifiedChildCustodyAuthorization["scopeCheckpoint"];
  authorizedAt: number;
  appliedAt: number;
  localWrites: ExternallyVerifiedChildCustodyAuthorization["localWrites"];
  auxiliaryLocalWrites?: ExternallyVerifiedChildCustodyAuthorization["auxiliaryLocalWrites"];
  revalidateAdoptionWrites: () => void;
  performAdoptionWrites: () => void;
}>;


export class LifecycleReceiptRepository {
  readonly #database: Database.Database;
  readonly #rotations: LifecycleRotationRepository;
  readonly #reviews: LifecycleReviewAdoptionStore;
  readonly #clock: () => number;

  constructor(
    database: Database.Database,
    rotations: LifecycleRotationRepository,
    clock: () => number = Date.now,
  ) {
    this.#database = database;
    this.#rotations = rotations;
    this.#reviews = new LifecycleReviewAdoptionStore(database);
    this.#clock = clock;
  }

  persistVerifiedAuthorityReceiptInCurrentTransaction(
    prepared: PreparedChildCustodyTerminal | PreparedGenerationLossTerminal,
    record: LifecycleReceiptRecord,
  ): void {
    if (!this.#database.inTransaction) {
      throw new Error("lifecycle authority receipt persistence requires a transaction");
    }
    const existing = this.#database.prepare(`
      SELECT receipt_digest FROM lifecycle_authority_receipts WHERE intent_digest=?
    `).get(prepared.intentDigest);
    if (isRow(existing)) {
      if (text(existing, "receipt_digest") !== record.receipt.receiptDigest) {
        throw new Error("stale lifecycle adoption receipt changed before supersession");
      }
      return;
    }
    const custodyTerminal = "custodyId" in prepared;
    const kind = custodyTerminal ? "custody-terminal" as const : "generation-loss-terminal" as const;
    const ownerKind = custodyTerminal ? "custody" as const : "generation-loss" as const;
    const ownerId = custodyTerminal ? prepared.custodyId : prepared.generationLossId;
    const receiptBody = {
      schemaVersion: 1,
      kind,
      authorityId: record.receipt.authorityId,
      authoritySequence: record.receipt.authoritySequence,
      previousReceiptDigest: record.receipt.previousReceiptDigest,
      intentDigest: prepared.intentDigest,
      subjectDigest: prepared.subjectDigest,
    };
    this.#database.prepare(`
      INSERT INTO lifecycle_authority_receipts(
        intent_digest,batch_id,ordinal,project_session_id,run_id,agent_id,kind,
        subject_owner_kind,subject_owner_id,subject_owner_revision,subject_digest,
        authority_id,authority_sequence,previous_authority_sequence,previous_receipt_digest,
        receipt_json,receipt_digest,attestation,verified_at
      ) VALUES (@intent,@batch,1,@project,@run,@agent,@kind,@ownerKind,@ownerId,
                @ownerRevision,@subject,@authority,@sequence,@previousSequence,
                @previousDigest,@receiptJson,@receiptDigest,@attestation,@verifiedAt)
    `).run({
      intent: prepared.intentDigest,
      batch: prepared.batchId,
      project: prepared.projectSessionId,
      run: prepared.runId,
      agent: prepared.agentId,
      kind,
      ownerKind,
      ownerId,
      ownerRevision: prepared.finalRevision,
      subject: prepared.subjectDigest,
      authority: record.receipt.authorityId,
      sequence: record.receipt.authoritySequence,
      previousSequence: record.receipt.authoritySequence === 1 ? null : record.receipt.authoritySequence - 1,
      previousDigest: record.receipt.previousReceiptDigest,
      receiptJson: canonicalJson({
        ...receiptBody,
        receiptDigest: record.receipt.receiptDigest,
        attestation: record.receipt.attestation,
      }),
      receiptDigest: record.receipt.receiptDigest,
      attestation: record.receipt.attestation,
      verifiedAt: this.#clock(),
    });
  }

  readPreparedChildCustodyTerminal(
    runId: string,
    agentId: string,
    custodyId: string,
    applyId: string,
  ): PreparedChildCustodyTerminal | null {
    const stored = this.#database.prepare(`
      SELECT batch.project_session_id,batch.planned_apply_id,batch.batch_id,
             batch.secondary_intent_kind,batch.review_adoption_reservation_id,
             batch.review_adoption_reservation_digest,batch.transition_replay_json,
             batch.transition_replay_digest,custody.bridge_owner_kind,
             effect.pre_revision,effect.final_revision,
             effect.effect_digest,effect.final_semantic_digest,
             effect.final_source_ref_digest,intent.subject_json,
             intent.subject_digest,intent.intent_digest,
             review_intent.subject_json AS review_subject_json,
             review_intent.subject_digest AS review_subject_digest,
             review_intent.intent_digest AS review_intent_digest
        FROM lifecycle_receipt_custody_effects effect
        JOIN lifecycle_receipt_batches batch ON batch.batch_id=effect.batch_id
        JOIN lifecycle_rotation_custodies custody
          ON custody.run_id=effect.run_id AND custody.agent_id=effect.agent_id
         AND custody.custody_id=effect.custody_id
        JOIN lifecycle_receipt_intents intent
          ON intent.batch_id=batch.batch_id AND intent.ordinal=1
        LEFT JOIN lifecycle_receipt_intents review_intent
          ON review_intent.batch_id=batch.batch_id AND review_intent.ordinal=2
       WHERE effect.run_id=? AND effect.agent_id=? AND effect.custody_id=?
         AND batch.planned_apply_id=?
    `).get(runId, agentId, custodyId, applyId);
    if (stored === undefined) return null;
    const value = row(stored, "prepared child custody terminal");
    const parsedSubject: unknown = JSON.parse(text(value, "subject_json"));
    if (typeof parsedSubject !== "object" || parsedSubject === null || Array.isArray(parsedSubject)) {
      throw new Error("prepared lifecycle receipt subject is invalid");
    }
    const subject = parsedSubject as Readonly<Record<string, unknown>>;
    const ownerRef = subject.ownerRef;
    if (typeof ownerRef !== "object" || ownerRef === null || Array.isArray(ownerRef)) {
      throw new Error("prepared lifecycle receipt owner reference is invalid");
    }
    const batchId = text(value, "batch_id");
    const subjectDigest = text(value, "subject_digest");
    const transitionReplayDigest = text(value, "transition_replay_digest");
    const fromState = subject.fromState;
    const disposition = subject.disposition;
    const proofKind = subject.terminalProofKind;
    const terminalEvidenceDigest = subject.terminalEvidenceDigest;
    if (
      !["awaiting-boundary", "prepared", "dispatched", "accepted", "ambiguous", "provider-terminal", "committing"]
        .includes(String(fromState)) ||
      !["adopted", "no-effect", "superseded", "quarantined"].includes(String(disposition)) ||
      !["provider-terminal", "zero-dispatch-no-effect", "predispatch-superseded",
        "postterminal-adoption-cas-superseded", "integrity-quarantine"].includes(String(proofKind)) ||
      typeof terminalEvidenceDigest !== "string"
    ) {
      throw new Error("prepared lifecycle terminal disposition is invalid");
    }
    const ownerKind = text(value, "bridge_owner_kind");
    if (ownerKind !== "chair" && ownerKind !== "child") throw new Error("lifecycle custody owner kind is invalid");
    const replay = row(JSON.parse(text(value, "transition_replay_json")), "prepared lifecycle transition replay");
    const parsedPlan = mutationPlan(row(replay.mutationPlan, "prepared lifecycle mutation plan"));
    const review = this.#reviews.readPreparedReview(value, batchId, transitionReplayDigest);
    const intent = {
      schemaVersion: 1,
      batchId,
      ordinalDec: "1",
      kind: "custody-terminal",
      subjectDigest,
      transitionReplayDigest,
    };
    return {
      projectSessionId: text(value, "project_session_id"),
      runId,
      agentId,
      custodyId,
      preRevision: integer(value, "pre_revision"),
      finalRevision: integer(value, "final_revision"),
      applyId: text(value, "planned_apply_id"),
      batchId,
      effectDigest: text(value, "effect_digest"),
      ownerRefDigest: lifecycleDigest("receipt-owner-ref", ownerRef),
      transitionReplayDigest,
      subject,
      subjectJson: text(value, "subject_json"),
      subjectDigest,
      intent,
      intentJson: canonicalJson(intent),
      intentDigest: text(value, "intent_digest"),
      finalSemanticDigest: text(value, "final_semantic_digest"),
      finalSourceRefDigest: text(value, "final_source_ref_digest"),
      fromState: fromState as PreparedChildCustodyTerminal["fromState"],
      disposition: disposition as PreparedChildCustodyTerminal["disposition"],
      proofKind: proofKind as PreparedChildCustodyTerminal["proofKind"],
      terminalEvidenceDigest,
      ownerKind: ownerKind as "chair" | "child",
      mutationPlan: parsedPlan,
      review,
    };
  }

  readPreparedGenerationLossTerminal(
    runId: string,
    agentId: string,
    generationLossId: string,
    applyId: string,
  ): PreparedGenerationLossTerminal | null {
    return new GenerationLossTerminalOwnerAdapter(this.#database).readPrepared(
      runId,
      agentId,
      generationLossId,
      applyId,
    );
  }

  prepareGenerationLossTerminalInCurrentTransaction(
    input: PrepareGenerationLossTerminalInput,
  ): PreparedGenerationLossTerminal {
    return new GenerationLossTerminalOwnerAdapter(this.#database).prepare(input);
  }

  prepareChildCustodyTerminalInCurrentTransaction(
    input: PrepareChildCustodyTerminalInput,
  ): PreparedChildCustodyTerminal {
    if (!this.#database.inTransaction) throw new Error("lifecycle terminal preparation requires a transaction");
    const head = this.#rotations.readHead(input.runId, input.agentId, input.custodyId);
    const terminal = input.terminal ?? {
      disposition: "adopted" as const,
      proofKind: "provider-terminal" as const,
      terminalEvidenceDigest: null,
    };
    const legalSource = terminal.disposition === "adopted"
      ? head.state === "committing" && terminal.proofKind === "provider-terminal"
      : terminal.disposition === "no-effect"
        ? (head.state === "awaiting-boundary" || head.state === "prepared") &&
          terminal.proofKind === "zero-dispatch-no-effect"
        : terminal.disposition === "superseded"
          ? ((head.state === "awaiting-boundary" || head.state === "prepared") &&
              terminal.proofKind === "predispatch-superseded") ||
            ((head.state === "provider-terminal" || head.state === "committing") &&
              terminal.proofKind === "postterminal-adoption-cas-superseded")
          : terminal.proofKind === "integrity-quarantine";
    if (head.revision !== input.expectedRevision || head.terminal || !legalSource) {
      throw new Error("lifecycle custody is not the expected terminal source head");
    }
    const source = row(this.#database.prepare(`
      SELECT custody.project_session_id,custody.bridge_owner_kind,custody.admission_digest,
             custody.provider_action_adapter_id,custody.provider_action_id,
             custody.source_adapter_id,custody.source_adapter_contract_digest,
             custody.source_provider_generation,custody.source_principal_generation,
             custody.source_bridge_generation,custody.source_chair_lease_generation,
             custody.replacement_adapter_id,custody.replacement_contract_digest,
             custody.target_provider_generation,custody.target_principal_generation,
             custody.target_bridge_generation,custody.checkpoint_digest,custody.command_id,
             revision.terminal_evidence_digest
        FROM lifecycle_rotation_custodies custody
        JOIN lifecycle_rotation_custody_revisions revision
          ON revision.run_id=custody.run_id AND revision.agent_id=custody.agent_id
         AND revision.custody_id=custody.custody_id AND revision.revision=?
       WHERE custody.run_id=? AND custody.agent_id=? AND custody.custody_id=?
    `).get(head.revision, input.runId, input.agentId, input.custodyId), "lifecycle committing custody");
    const ownerKind = text(source, "bridge_owner_kind");
    if (ownerKind !== "chair" && ownerKind !== "child") throw new Error("lifecycle custody owner kind is invalid");
    if (ownerKind === "chair" && terminal.disposition === "adopted") {
      throw new Error("true-chair lifecycle adoption requires ordinal-two review authority");
    }
    const normalizedMutationPlan = mutationPlan(input.mutationPlan);
    const terminalEvidenceDigest = terminal.terminalEvidenceDigest ?? text(source, "terminal_evidence_digest");
    const finalRevision = head.revision + 1;
    const finalBody = revisionBody({
      custodyId: input.custodyId,
      revision: finalRevision,
      state: "finalized",
      disposition: terminal.disposition,
      proofKind: terminal.proofKind,
      terminalEvidenceDigest,
    });
    const finalSemanticDigest = lifecycleDigest("custody-semantic", finalBody);
    const finalSourceRefDigest = finalSemanticDigest;
    const beforeRef = {
      kind: "custody",
      custodyRef: custodyRef(input.runId, input.agentId, input.custodyId, head.revision),
      sourceRefDigest: head.sourceRefDigest,
    };
    const afterRef = {
      kind: "custody",
      custodyRef: custodyRef(input.runId, input.agentId, input.custodyId, finalRevision),
      sourceRefDigest: finalSourceRefDigest,
    };
    const providerActionRef = {
      adapterId: text(source, "provider_action_adapter_id"),
      actionId: text(source, "provider_action_id"),
    };
    const prepared = persistSingleOwnerTerminalPreparation(this.#database, {
      transitionKind: "custody-terminal",
      subjectOwnerKind: "custody",
      ownerId: input.custodyId,
      projectSessionId: head.projectSessionId,
      runId: input.runId,
      agentId: input.agentId,
      preRevision: head.revision,
      finalRevision,
      applyId: input.applyId,
      beforeRef,
      afterRef,
      beforeJournalDigest: head.journalDigest,
      finalSemanticDigest,
      finalSourceRefDigest,
      admissionDigest: text(source, "admission_digest"),
      providerActionRef,
      terminalDisposition: terminal.disposition,
      terminalEvidenceDigest,
      transitionProof: input.transitionProof,
      mutationPlan: normalizedMutationPlan,
      subjectFields: {
        providerActionRef,
        fromState: head.state,
        disposition: terminal.disposition,
        terminalProofKind: terminal.proofKind,
        recoverySource: { kind: "none" },
        recoverySourceDecisionDigest: null,
        linkedLossEffectDigest: null,
      },
      recordedAt: input.recordedAt,
      persistEffect: (batchId, effectDigest) => {
        this.#database.prepare(`
          INSERT INTO lifecycle_receipt_custody_effects(
            batch_id,ordinal,role,transition_kind,planned_apply_id,project_session_id,
            run_id,agent_id,custody_id,pre_revision,pre_journal_digest,final_revision,
            final_semantic_digest,final_source_ref_digest,effect_digest
          ) VALUES (?,1,'primary','custody-terminal',?,?,?,?,?,?,?,?,?,?,?)
        `).run(
          batchId, input.applyId, head.projectSessionId, input.runId, input.agentId,
          input.custodyId, head.revision, head.journalDigest, finalRevision,
          finalSemanticDigest, finalSourceRefDigest, effectDigest,
        );
      },
    });
    return {
      projectSessionId: head.projectSessionId,
      runId: input.runId,
      agentId: input.agentId,
      custodyId: input.custodyId,
      preRevision: head.revision,
      finalRevision,
      applyId: input.applyId,
      ...prepared,
      finalSemanticDigest,
      finalSourceRefDigest,
      fromState: head.state as PreparedChildCustodyTerminal["fromState"],
      disposition: terminal.disposition,
      proofKind: terminal.proofKind,
      terminalEvidenceDigest,
      ownerKind: ownerKind as "chair" | "child",
      mutationPlan: normalizedMutationPlan,
      review: null,
    };
  }

  applyAuthorizedChildCustodyTerminalInCurrentTransaction(
    input: ExternallyVerifiedChildCustodyAuthorization,
  ): LifecycleCustodyHead {
    return this.#applyAuthorizedTerminalInCurrentTransaction(input) as LifecycleCustodyHead;
  }

  applyAuthorizedGenerationLossTerminalInCurrentTransaction(
    input: ExternallyVerifiedGenerationLossAuthorization,
  ): GenerationLossHead {
    return this.#applyAuthorizedTerminalInCurrentTransaction(input) as GenerationLossHead;
  }

  #applyAuthorizedTerminalInCurrentTransaction(
    input: ExternallyVerifiedChildCustodyAuthorization | ExternallyVerifiedGenerationLossAuthorization,
  ): LifecycleCustodyHead | GenerationLossHead {
    if (!this.#database.inTransaction) throw new Error("lifecycle terminal apply requires a transaction");
    const prepared = input.prepared;
    const custodyTerminal = "custodyId" in prepared;
    const generationOwner = new GenerationLossTerminalOwnerAdapter(this.#database);
    const generationPrepared = custodyTerminal ? null : prepared as PreparedGenerationLossTerminal;
    const transitionKind = custodyTerminal ? "custody-terminal" as const : "generation-loss-terminal" as const;
    const subjectOwnerKind = custodyTerminal ? "custody" as const : "generation-loss" as const;
    const ownerId = custodyTerminal ? prepared.custodyId : prepared.generationLossId;
    const head = custodyTerminal
      ? this.#rotations.readHead(prepared.runId, prepared.agentId, prepared.custodyId)
      : generationOwner.readExpectedHead(generationPrepared as PreparedGenerationLossTerminal, input.expectedRevision);
    if (custodyTerminal &&
        (head.revision !== input.expectedRevision || head.revision !== prepared.preRevision ||
         head.state !== prepared.fromState || head.terminal)) {
      throw new Error("lifecycle custody is not the expected terminal source head");
    }
    const planSql = custodyTerminal ? `
        SELECT batch.transition_replay_digest,batch.mutation_plan_digest,
               batch.effects_set_digest,batch.secondary_intent_kind,
               intent.subject_json,intent.subject_digest,intent.intent_digest,
               effect.effect_digest,revision.terminal_evidence_digest
          FROM lifecycle_receipt_batches batch
          JOIN lifecycle_receipt_intents intent ON intent.batch_id=batch.batch_id AND intent.ordinal=1
          JOIN lifecycle_receipt_custody_effects effect ON effect.batch_id=batch.batch_id
          JOIN lifecycle_rotation_custody_revisions revision
            ON revision.run_id=batch.run_id AND revision.agent_id=batch.agent_id
           AND revision.custody_id=effect.custody_id AND revision.revision=effect.pre_revision
         WHERE batch.batch_id=? AND batch.planned_apply_id=?
           AND batch.project_session_id=? AND batch.run_id=? AND batch.agent_id=?
           AND effect.final_revision=? AND effect.final_semantic_digest=?
           AND effect.final_source_ref_digest=?
      ` : `
        SELECT batch.transition_replay_digest,batch.mutation_plan_digest,
               batch.effects_set_digest,batch.secondary_intent_kind,
               intent.subject_json,intent.subject_digest,intent.intent_digest,
               effect.effect_digest,revision.terminal_evidence_digest
          FROM lifecycle_receipt_batches batch
          JOIN lifecycle_receipt_intents intent ON intent.batch_id=batch.batch_id AND intent.ordinal=1
          JOIN lifecycle_receipt_generation_loss_effects effect ON effect.batch_id=batch.batch_id
          JOIN lifecycle_generation_loss_revisions revision
            ON revision.run_id=batch.run_id AND revision.agent_id=batch.agent_id
           AND revision.generation_loss_id=effect.generation_loss_id
           AND revision.revision=effect.pre_revision
         WHERE batch.batch_id=? AND batch.planned_apply_id=?
           AND batch.project_session_id=? AND batch.run_id=? AND batch.agent_id=?
           AND effect.final_revision=? AND effect.final_semantic_digest=?
           AND effect.final_source_ref_digest=?
      `;
    const plan = row(this.#database.prepare(planSql).get(
      prepared.batchId, prepared.applyId, prepared.projectSessionId, prepared.runId,
      prepared.agentId, prepared.finalRevision, prepared.finalSemanticDigest,
      prepared.finalSourceRefDigest,
    ), "prepared lifecycle terminal batch");
    const revalidatedMutationPlan = mutationPlan(
      prepared.mutationPlan as unknown as Readonly<Record<string, unknown>>,
    );
    if (text(plan, "mutation_plan_digest") !== revalidatedMutationPlan.writeSetDigest) {
      throw new Error("prepared lifecycle mutation plan changed");
    }
    const actualBusinessWrites = [...input.localWrites]
      .map((write) => ({
        relation: write.relation,
        keyDigest: lifecycleDigest("mutation-key", {
          schemaVersion: 1,
          relation: write.relation,
          key: write.key,
        }),
        operation: write.operation,
      }))
      .sort((left, right) => canonicalJson(left).localeCompare(canonicalJson(right)));
    const plannedBusinessWrites = revalidatedMutationPlan.writes
      .map((write) => ({
        relation: write.relation,
        keyDigest: write.keyDigest,
        operation: write.operation,
      }))
      .sort((left, right) => canonicalJson(left).localeCompare(canonicalJson(right)));
    if (canonicalJson(actualBusinessWrites) !== canonicalJson(plannedBusinessWrites)) {
      throw new Error("lifecycle actual write identities crossed the prepared mutation plan");
    }
    const effectRows = this.#database.prepare(`
      SELECT effect_digest,ordinal,role FROM lifecycle_receipt_custody_effects WHERE batch_id=?
      UNION ALL
      SELECT effect_digest,ordinal,role FROM lifecycle_receipt_generation_loss_effects WHERE batch_id=?
      UNION ALL
      SELECT effect_digest,receipt_ordinal AS ordinal,effect_role AS role
        FROM lifecycle_receipt_fresh_origin_effects WHERE batch_id=?
       ORDER BY ordinal,role
    `).all(prepared.batchId, prepared.batchId, prepared.batchId).map((candidate) => {
      const effect = row(candidate, "prepared lifecycle effect member");
      return [text(effect, "effect_digest"), integer(effect, "ordinal"), text(effect, "role")];
    });
    const effectsSetDigest = lifecycleDigest("effect-set", effectRows.map(([digest]) => digest));
    if (effectsSetDigest !== text(plan, "effects_set_digest")) {
      throw new Error("prepared lifecycle effect set changed");
    }
    const receiptBody = {
      schemaVersion: 1,
      kind: transitionKind,
      authorityId: input.receipt.authorityId,
      authoritySequence: input.receipt.authoritySequence,
      previousReceiptDigest: input.receipt.previousReceiptDigest,
      intentDigest: prepared.intentDigest,
      subjectDigest: prepared.subjectDigest,
    };
    const custodyPrepared = custodyTerminal ? prepared : null;
    const reviewReceipt = this.#reviews.requireAuthorityReceipt(
      custodyPrepared?.review ?? null,
      "reviewReceipt" in input ? input.reviewReceipt : undefined,
    );
    const checkpointBody = {
      schemaVersion: 1,
      authorityId: input.receipt.authorityId,
      projectSessionId: prepared.projectSessionId,
      runId: prepared.runId,
      receiptCountDec: String(input.scopeCheckpoint.receiptCount),
      headAuthoritySequenceDec: String(input.scopeCheckpoint.headAuthoritySequence),
      headReceiptDigest: input.scopeCheckpoint.headReceiptDigest,
      orderedRecordSetDigest: input.scopeCheckpoint.orderedRecordSetDigest,
    };
    const authorizationValid = input.receipt.attestation.length > 0 &&
      input.scopeCheckpoint.attestation.length > 0 &&
      Number.isSafeInteger(input.receipt.authoritySequence) && input.receipt.authoritySequence >= 1 &&
      input.scopeCheckpoint.receiptCount >= input.receipt.authoritySequence &&
      input.scopeCheckpoint.headAuthoritySequence === input.scopeCheckpoint.receiptCount &&
      input.scopeCheckpoint.headReceiptDigest !== null &&
      input.receipt.receiptDigest === lifecycleDigest("authenticated-receipt", receiptBody) &&
      input.scopeCheckpoint.checkpointDigest === lifecycleDigest("scope-checkpoint", checkpointBody);
    if (!authorizationValid) throw new Error("externally verified lifecycle authorization is invalid");
    const validatedReviewReceipt = this.#reviews.validateAuthorityReceipt(
      custodyPrepared?.review ?? null,
      reviewReceipt,
      input.receipt.authorityId,
    );
    const scope = row(this.#database.prepare(`
      SELECT scope.authority_id,head.checkpoint_digest,head.revision
        FROM lifecycle_admitted_run_scopes scope
        JOIN lifecycle_receipt_scope_heads head
          ON head.project_session_id=scope.project_session_id AND head.run_id=scope.run_id
       WHERE scope.project_session_id=? AND scope.run_id=?
    `).get(prepared.projectSessionId, prepared.runId), "admitted lifecycle receipt scope");
    if (text(scope, "authority_id") !== input.receipt.authorityId ||
        text(scope, "checkpoint_digest") !== input.expectedScopeHead.checkpointDigest ||
        integer(scope, "revision") !== input.expectedScopeHead.revision) {
      throw new Error("externally verified lifecycle authorization is invalid");
    }
    const receiptJson = canonicalJson({ ...receiptBody, receiptDigest: input.receipt.receiptDigest,
      attestation: input.receipt.attestation });
    this.#database.prepare(`
      INSERT INTO lifecycle_authority_receipts(
        intent_digest,batch_id,ordinal,project_session_id,run_id,agent_id,kind,
        subject_owner_kind,subject_owner_id,subject_owner_revision,subject_digest,
        authority_id,authority_sequence,previous_authority_sequence,previous_receipt_digest,
        receipt_json,receipt_digest,attestation,verified_at
      ) VALUES (@intent,@batch,1,@project,@run,@agent,@kind,@ownerKind,@ownerId,
                @ownerRevision,@subject,@authority,@sequence,@previousSequence,
                @previousDigest,@receiptJson,@receiptDigest,@attestation,@verifiedAt)
    `).run({
      intent: prepared.intentDigest,
      batch: prepared.batchId,
      project: prepared.projectSessionId,
      run: prepared.runId,
      agent: prepared.agentId,
      kind: transitionKind,
      ownerKind: subjectOwnerKind,
      ownerId,
      ownerRevision: prepared.finalRevision,
      subject: prepared.subjectDigest,
      authority: input.receipt.authorityId,
      sequence: input.receipt.authoritySequence,
      previousSequence: input.receipt.authoritySequence === 1 ? null : input.receipt.authoritySequence - 1,
      previousDigest: input.receipt.previousReceiptDigest,
      receiptJson,
      receiptDigest: input.receipt.receiptDigest,
      attestation: input.receipt.attestation,
      verifiedAt: input.receipt.verifiedAt,
    });
    if (custodyPrepared !== null) {
      this.#reviews.persistAuthorityReceiptInCurrentTransaction(custodyPrepared, validatedReviewReceipt);
    }
    const checkpointJson = canonicalJson({ ...checkpointBody,
      checkpointDigest: input.scopeCheckpoint.checkpointDigest,
      attestation: input.scopeCheckpoint.attestation });
    this.#database.prepare(`
      INSERT INTO lifecycle_receipt_scope_checkpoints(
        project_session_id,run_id,authority_id,receipt_count,head_authority_sequence,
        head_receipt_digest,ordered_record_set_digest,checkpoint_json,checkpoint_digest,
        attestation,verified_at
      ) VALUES (?,?,?,?,?,?,?,?,?,?,?)
    `).run(
      prepared.projectSessionId, prepared.runId, input.receipt.authorityId,
      input.scopeCheckpoint.receiptCount, input.scopeCheckpoint.headAuthoritySequence,
      input.scopeCheckpoint.headReceiptDigest, input.scopeCheckpoint.orderedRecordSetDigest,
      checkpointJson, input.scopeCheckpoint.checkpointDigest,
      input.scopeCheckpoint.attestation, input.scopeCheckpoint.verifiedAt,
    );
    const scopeChanged = this.#database.prepare(`
      UPDATE lifecycle_receipt_scope_heads SET checkpoint_digest=?,revision=revision+1
       WHERE project_session_id=? AND run_id=? AND checkpoint_digest=? AND revision=?
    `).run(
      input.scopeCheckpoint.checkpointDigest, prepared.projectSessionId, prepared.runId,
      input.expectedScopeHead.checkpointDigest, input.expectedScopeHead.revision,
    );
    if (scopeChanged.changes !== 1) throw new Error("lifecycle receipt scope head compare-and-set failed");
    const receiptMembers = [{
      ordinalDec: "1",
      intentDigest: prepared.intentDigest,
      authorityId: input.receipt.authorityId,
      authoritySequenceDec: String(input.receipt.authoritySequence),
      receiptDigest: input.receipt.receiptDigest,
      subjectDigest: prepared.subjectDigest,
    }, ...(validatedReviewReceipt === null ? [] : [validatedReviewReceipt.member])];
    const receiptSetDigest = lifecycleDigest("authority-receipt-set", receiptMembers);
    const hasReview = custodyPrepared?.review !== null && custodyPrepared?.review !== undefined;
    const completionBody = {
      schemaVersion: 1,
      batchId: prepared.batchId,
      transitionKind,
      receiptIntentCountDec: hasReview ? "2" : "1",
      secondaryIntentKind: hasReview ? "review-adoption-decision" : "none",
      ordinalOne: {
        intentDigest: prepared.intentDigest,
        subjectDigest: prepared.subjectDigest,
        authorityReceiptDigest: input.receipt.receiptDigest,
      },
      ordinalTwo: validatedReviewReceipt?.completionOrdinal ?? null,
      primaryEffect: { kind: subjectOwnerKind, effectDigest: prepared.effectDigest },
      linkedLossEffectDigest: null,
      secondaryEffect: null,
      effectsSetDigest,
      orderedAuthorityReceiptSetDigest: receiptSetDigest,
    };
    const completionDigest = lifecycleDigest("batch-completion", completionBody);
    const completionInsertPrefix = `
      INSERT INTO lifecycle_receipt_batch_completions(
        batch_id,transition_kind,receipt_intent_count,secondary_intent_kind,
        ordinal_one,ordinal_one_intent_digest,ordinal_one_subject_digest,
        ordinal_one_receipt_digest,ordinal_two,ordinal_two_intent_digest,
        ordinal_two_subject_digest,ordinal_two_receipt_digest,
        effects_set_digest,
        primary_custody_effect_digest,primary_loss_effect_role,
        primary_loss_effect_digest,primary_retirement_effect_digest,
        primary_fresh_origin_effect_role,primary_fresh_origin_effect_digest,
        linked_loss_effect_role,linked_loss_effect_digest,
        secondary_fresh_origin_effect_role,secondary_fresh_origin_effect_digest,
        ordered_authority_receipt_set_digest,completion_json,completion_digest,completed_at
      )`;
    const completionValues = [
      prepared.batchId,
      hasReview ? 2 : 1,
      hasReview ? "review-adoption-decision" : "none",
      prepared.intentDigest, prepared.subjectDigest,
      input.receipt.receiptDigest,
      validatedReviewReceipt === null ? null : 2,
      custodyPrepared?.review?.intentDigest ?? null,
      custodyPrepared?.review?.subjectDigest ?? null,
      validatedReviewReceipt?.receipt.receiptDigest ?? null,
      effectsSetDigest,
    ];
    if (custodyTerminal) {
      this.#database.prepare(`${completionInsertPrefix}
        VALUES (?,'custody-terminal',?,?,1,?,?,?, ?,?,?,?, ?, ?,
                NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,?,?,?,?)
      `).run(
        ...completionValues, prepared.effectDigest, receiptSetDigest,
        canonicalJson(completionBody), completionDigest, input.authorizedAt,
      );
    } else {
      this.#database.prepare(`${completionInsertPrefix}
        VALUES (?,'generation-loss-terminal',?,?,1,?,?,?, ?,?,?,?, ?, NULL,
                'primary',?,NULL,NULL,NULL,NULL,NULL,NULL,NULL,?,?,?,?)
      `).run(
        ...completionValues, prepared.effectDigest, receiptSetDigest,
        canonicalJson(completionBody), completionDigest, input.authorizedAt,
      );
    }
    const authorizationBody = {
      schemaVersion: 1,
      batchId: prepared.batchId,
      batchCompletionDigest: completionDigest,
      orderedAuthorityReceiptSetDigest: receiptSetDigest,
      verifiedScopeCheckpointDigest: input.scopeCheckpoint.checkpointDigest,
      authorizedAt: input.authorizedAt,
    };
    const authorizationDigest = lifecycleDigest("batch-authorization", authorizationBody);
    this.#database.prepare(`
      INSERT INTO lifecycle_receipt_batch_authorizations VALUES (?,?,?,?,?,?,?,?)
    `).run(
      prepared.batchId, prepared.projectSessionId, prepared.runId, completionDigest,
      receiptSetDigest, input.scopeCheckpoint.checkpointDigest, input.authorizedAt,
      authorizationDigest,
    );
    input.revalidateAdoptionWrites();
    const systemWrites: Array<Readonly<{
      relation: string;
      key: string;
      operation: "insert" | "update" | "delete";
    }>> = [
      { relation: "lifecycle_authority_receipts", key: `${prepared.batchId}:1`, operation: "insert" },
      ...(!hasReview ? [] : [{
        relation: "lifecycle_authority_receipts",
        key: `${prepared.batchId}:2`,
        operation: "insert" as const,
      }]),
      {
        relation: "lifecycle_receipt_scope_checkpoints",
        key: `${prepared.projectSessionId}:${prepared.runId}:${input.scopeCheckpoint.checkpointDigest}`,
        operation: "insert",
      },
      {
        relation: "lifecycle_receipt_scope_heads",
        key: `${prepared.projectSessionId}:${prepared.runId}`,
        operation: "update",
      },
      { relation: "lifecycle_receipt_batch_completions", key: prepared.batchId, operation: "insert" },
      { relation: "lifecycle_receipt_batch_authorizations", key: prepared.batchId, operation: "insert" },
      ...(custodyTerminal ? [{
        relation: "lifecycle_rotation_custody_revisions",
        key: `${prepared.runId}:${prepared.agentId}:${prepared.custodyId}:${prepared.finalRevision}`,
        operation: "insert" as const,
      }, {
        relation: "lifecycle_rotation_custody_heads",
        key: `${prepared.runId}:${prepared.agentId}:${prepared.custodyId}`,
        operation: "update" as const,
      }] : generationOwner.systemWrites(generationPrepared as PreparedGenerationLossTerminal)),
      { relation: "lifecycle_transition_applies", key: prepared.applyId, operation: "insert" },
    ];
    const localWrites = [...input.localWrites, ...(input.auxiliaryLocalWrites ?? []), ...systemWrites]
      .sort((left, right) => canonicalJson(left).localeCompare(canonicalJson(right)));
    const localWriteSetDigest = lifecycleDigest("local-write-set", {
      schemaVersion: 1,
      writes: localWrites,
    });
    const applyBody = {
      schemaVersion: 1,
      applyKind: "terminal",
      applyId: prepared.applyId,
      receiptBatchId: prepared.batchId,
      batchCompletionDigest: completionDigest,
      transitionReplayDigest: text(plan, "transition_replay_digest"),
      orderedAuthorityReceiptSetDigest: receiptSetDigest,
      verifiedScopeCheckpointDigest: input.scopeCheckpoint.checkpointDigest,
      primaryOwnerAfterRef: custodyTerminal ? {
        kind: "custody",
        custodyRef: custodyRef(prepared.runId, prepared.agentId, prepared.custodyId, prepared.finalRevision),
        sourceRefDigest: prepared.finalSourceRefDigest,
      } : generationOwner.ownerAfterRef(generationPrepared as PreparedGenerationLossTerminal),
      freshHandoffRef: null,
      freshSourceMode: null,
      freshApplyPlanDigest: null,
      newCustodyRef: null,
      generationLossAfterRef: null,
      freshOriginEffectDigest: null,
      appliedMutationPlanDigest: text(plan, "mutation_plan_digest"),
      localWriteSetDigest,
    };
    const applyDigest = lifecycleDigest("transition-apply", applyBody);
    if (custodyTerminal) {
      this.#rotations.finalizeAuthorizedLifecycleCustodyInCurrentTransaction({
        sourceHead: head as LifecycleCustodyHead,
        finalRevision: prepared.finalRevision,
        disposition: prepared.disposition,
        proofKind: prepared.proofKind,
        terminalEvidenceDigest: prepared.terminalEvidenceDigest,
        finalSemanticDigest: prepared.finalSemanticDigest,
        finalSourceRefDigest: prepared.finalSourceRefDigest,
        authorityBatchId: prepared.batchId,
        authorityApplyId: prepared.applyId,
        authorityApplyDigest: applyDigest,
        recordedAt: input.appliedAt,
      });
    } else {
      generationOwner.finalizeAuthorized(
        generationPrepared as PreparedGenerationLossTerminal,
        head as GenerationLossHead,
        { authorityApplyDigest: applyDigest, recordedAt: input.appliedAt },
      );
    }
    input.performAdoptionWrites();
    if (validatedReviewReceipt !== null && custodyPrepared !== null) {
      this.#reviews.writePostStateInCurrentTransaction(
        custodyPrepared,
        validatedReviewReceipt.receipt,
        input.appliedAt,
      );
    }
    this.#database.prepare(`
      INSERT INTO lifecycle_transition_applies(
        apply_id,apply_kind,batch_transition_kind,receipt_batch_id,
        batch_completion_digest,transition_replay_digest,
        ordered_authority_receipt_set_digest,verified_scope_checkpoint_digest,
        applied_mutation_plan_digest,fresh_handoff_id,fresh_handoff_digest,
        fresh_handoff_key,fresh_project_session_id,fresh_run_id,fresh_agent_id,
        fresh_source_mode,fresh_apply_plan_digest,new_custody_id,new_custody_revision,
        new_custody_semantic_digest,new_custody_source_ref_digest,
        fresh_generation_loss_id,fresh_generation_loss_after_revision,
        fresh_generation_loss_after_semantic_digest,
        fresh_generation_loss_after_source_ref_digest,fresh_generation_loss_after_key,
        fresh_origin_effect_role,fresh_origin_effect_digest,local_write_set_digest,
        apply_json,apply_digest,applied_at
      ) VALUES (?,'terminal',?,?,?,?,?,?,?,NULL,NULL,'none',
                NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,
                'none',NULL,NULL,?,?,?,?)
    `).run(
      prepared.applyId, transitionKind, prepared.batchId, completionDigest,
      text(plan, "transition_replay_digest"), receiptSetDigest,
      input.scopeCheckpoint.checkpointDigest, text(plan, "mutation_plan_digest"),
      localWriteSetDigest, canonicalJson(applyBody), applyDigest, input.appliedAt,
    );
    return custodyTerminal
      ? this.#rotations.readHead(prepared.runId, prepared.agentId, prepared.custodyId)
      : generationOwner.readFinalHead(generationPrepared as PreparedGenerationLossTerminal);
  }

}
