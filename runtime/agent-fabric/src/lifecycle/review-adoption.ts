import type Database from "better-sqlite3";

import { canonicalJson, integer, isRow, row, text } from "../persistence/row-codec.js";
import { custodyRef, lifecycleDigest } from "./custody-codec.js";
import type { LifecycleCustodyHead } from "./rotation-repository.js";

export type LifecycleReviewMutationPlan = Readonly<{
  schemaVersion: 1;
  writes: readonly unknown[];
  writeSetDigest: string;
}>;

export type PreparedReviewAdoption = Readonly<{
  reservationId: string;
  reservationDigest: string;
  decision: Readonly<Record<string, unknown>>;
  decisionDigest: string;
  cut: Readonly<Record<string, unknown>> | null;
  cutDigest: string | null;
  subject: Readonly<Record<string, unknown>>;
  subjectJson: string;
  subjectDigest: string;
  intent: Readonly<Record<string, unknown>>;
  intentJson: string;
  intentDigest: string;
  successorBinding: Readonly<Record<string, unknown>> | null;
  rebindReceipt: Readonly<Record<string, unknown>> | null;
}>;

export type ReviewReservationPlan = Readonly<{
  reservationId: string;
  reservationDigest: string;
  decision: Readonly<Record<string, unknown>>;
  decisionDigest: string;
  cut: Readonly<Record<string, unknown>> | null;
  cutDigest: string | null;
  successorBinding: Readonly<Record<string, unknown>> | null;
  rebindReceipt: Readonly<Record<string, unknown>> | null;
}>;

export type PreparedReviewAdoptionContext = Readonly<{
  projectSessionId: string;
  runId: string;
  agentId: string;
  custodyId: string;
  finalRevision: number;
  applyId: string;
  batchId: string;
  review: PreparedReviewAdoption | null;
}>;

export type VerifiedReviewReceipt = Readonly<{ receiptDigest: string }>;

export type ReviewBatchPreparation = Readonly<{
  prepared: Readonly<{
    plan: ReviewReservationPlan;
    subject: Readonly<Record<string, unknown>>;
    subjectJson: string;
    subjectDigest: string;
    pendingIntent: Readonly<Record<string, unknown>>;
  }> | null;
  intentCount: 1 | 2;
  intentCountDec: "1" | "2";
  secondaryKind: "none" | "review-adoption-decision";
  reservationId: string | null;
  reservationDigest: string | null;
  reservationRef: Readonly<{ reservationId: string; reservationDigest: string }> | null;
  orderedSubjectMembers: readonly Readonly<Record<string, unknown>>[];
}>;

export type ExternalReviewReceipt = Readonly<{
  authorityId: string;
  authoritySequence: number;
  previousReceiptDigest: string | null;
  receiptDigest: string;
  attestation: string;
  verifiedAt: number;
}>;

export type ValidatedReviewReceipt = Readonly<{
  receipt: ExternalReviewReceipt;
  receiptBody: Readonly<Record<string, unknown>>;
  member: Readonly<Record<string, unknown>>;
  completionOrdinal: Readonly<{
    intentDigest: string;
    subjectDigest: string;
    authorityReceiptDigest: string;
  }>;
}>;

export class LifecycleReviewAdoptionStore {
  readonly #database: Database.Database;

  constructor(database: Database.Database) {
    this.#database = database;
  }

  #requireTransaction(): void {
    if (!this.#database.inTransaction) {
      throw new Error("lifecycle review adoption requires a transaction");
    }
  }

  readPreparedReview(
    value: Readonly<Record<string, unknown>>,
    batchId: string,
    transitionReplayDigest: string,
  ): PreparedReviewAdoption | null {
    if (text(value, "secondary_intent_kind") !== "review-adoption-decision") return null;
    const reviewSubjectJson = text(value, "review_subject_json");
    const reviewSubject = row(JSON.parse(reviewSubjectJson), "prepared review subject");
    const decision = row(reviewSubject.reviewDecision, "prepared review decision");
    const reviewSubjectDigest = text(value, "review_subject_digest");
    const reviewIntent = {
      schemaVersion: 1,
      batchId,
      ordinalDec: "2",
      kind: "review-adoption-decision",
      subjectDigest: reviewSubjectDigest,
      transitionReplayDigest,
    };
    return {
      reservationId: text(value, "review_adoption_reservation_id"),
      reservationDigest: text(value, "review_adoption_reservation_digest"),
      decision,
      decisionDigest: text(reviewSubject, "reviewDecisionDigest"),
      cut: reviewSubject.certificationCut === null ? null : row(reviewSubject.certificationCut, "prepared review cut"),
      cutDigest: reviewSubject.certificationCutDigest === null ? null : text(reviewSubject, "certificationCutDigest"),
      subject: reviewSubject,
      subjectJson: reviewSubjectJson,
      subjectDigest: reviewSubjectDigest,
      intent: reviewIntent,
      intentJson: canonicalJson(reviewIntent),
      intentDigest: text(value, "review_intent_digest"),
      successorBinding: decision.successorBinding === null ? null :
        row(decision.successorBinding, "prepared successor binding"),
      rebindReceipt: reviewSubject.rebindReceipt === null ? null :
        row(reviewSubject.rebindReceipt, "prepared rebind receipt"),
    };
  }

  buildBatchPreparation(input: Readonly<{
    plan: ReviewReservationPlan | null;
    projectSessionId: string;
    runId: string;
    agentId: string;
    ownerRef: Readonly<Record<string, unknown>>;
    ownerRefDigest: string;
    finalRevision: number;
    custodyTerminalSubjectDigest: string;
    lifecycleAdoptionEvidenceDigest: string | undefined;
    transitionReplayDigest: string;
  }>): ReviewBatchPreparation {
    if (input.plan === null) {
      return {
        prepared: null,
        intentCount: 1,
        intentCountDec: "1",
        secondaryKind: "none",
        reservationId: null,
        reservationDigest: null,
        reservationRef: null,
        orderedSubjectMembers: [],
      };
    }
    const subject = {
      schemaVersion: 1,
      kind: "review-adoption-decision",
      projectSessionId: input.projectSessionId,
      runId: input.runId,
      agentId: input.agentId,
      ownerRef: input.ownerRef,
      custodyTerminalSubjectDigest: input.custodyTerminalSubjectDigest,
      lifecycleAdoptionEvidenceDigest: input.lifecycleAdoptionEvidenceDigest,
      reviewReservationDigest: input.plan.reservationDigest,
      reviewDecision: input.plan.decision,
      reviewDecisionDigest: input.plan.decisionDigest,
      successorBinding: input.plan.successorBinding,
      rebindReceipt: input.plan.rebindReceipt,
      certificationCut: input.plan.cut,
      certificationCutDigest: input.plan.cutDigest,
      recoverySource: { kind: "none" },
      recoverySourceDecisionDigest: null,
      transitionReplayDigest: input.transitionReplayDigest,
    };
    const subjectDigest = lifecycleDigest("receipt-subject", subject);
    const pendingIntent = {
      schemaVersion: 1,
      batchId: "pending",
      ordinalDec: "2",
      kind: "review-adoption-decision",
      subjectDigest,
      transitionReplayDigest: input.transitionReplayDigest,
    };
    return {
      prepared: { plan: input.plan, subject, subjectJson: canonicalJson(subject), subjectDigest, pendingIntent },
      intentCount: 2,
      intentCountDec: "2",
      secondaryKind: "review-adoption-decision",
      reservationId: input.plan.reservationId,
      reservationDigest: input.plan.reservationDigest,
      reservationRef: {
        reservationId: input.plan.reservationId,
        reservationDigest: input.plan.reservationDigest,
      },
      orderedSubjectMembers: [{
        ordinalDec: "2",
        kind: "review-adoption-decision",
        ownerRefDigest: input.ownerRefDigest,
        ownerRevisionDec: String(input.finalRevision),
        subjectDigest,
      }],
    };
  }

  persistPreparedIntentInCurrentTransaction(input: Readonly<{
    preparation: ReviewBatchPreparation;
    batchId: string;
    projectSessionId: string;
    runId: string;
    agentId: string;
    custodyId: string;
    finalRevision: number;
    custodyEffectDigest: string;
    recordedAt: number;
  }>): PreparedReviewAdoption | null {
    this.#requireTransaction();
    const prepared = input.preparation.prepared;
    if (prepared === null) return null;
    const intent = { ...prepared.pendingIntent, batchId: input.batchId };
    const intentDigest = lifecycleDigest("receipt-intent", intent);
    this.#database.prepare(`
      INSERT INTO lifecycle_receipt_intents(
        batch_id,ordinal,batch_transition_kind,batch_intent_count,
        batch_secondary_intent_kind,kind,project_session_id,run_id,agent_id,
        subject_owner_kind,subject_owner_id,subject_owner_revision,
        custody_effect_digest,generation_loss_effect_role,generation_loss_effect_digest,
        recovery_retirement_effect_digest,fresh_origin_effect_digest,subject_json,
        subject_digest,intent_digest,created_at
      ) VALUES (?,2,'custody-terminal',2,'review-adoption-decision',
        'review-adoption-decision',?,?,?,'custody',?,?,?,NULL,NULL,NULL,NULL,?,?,?,?)
    `).run(
      input.batchId, input.projectSessionId, input.runId, input.agentId, input.custodyId,
      input.finalRevision, input.custodyEffectDigest, prepared.subjectJson, prepared.subjectDigest,
      intentDigest, input.recordedAt,
    );
    return {
      reservationId: prepared.plan.reservationId,
      reservationDigest: prepared.plan.reservationDigest,
      decision: prepared.plan.decision,
      decisionDigest: prepared.plan.decisionDigest,
      cut: prepared.plan.cut,
      cutDigest: prepared.plan.cutDigest,
      subject: prepared.subject,
      subjectJson: prepared.subjectJson,
      subjectDigest: prepared.subjectDigest,
      intent,
      intentJson: canonicalJson(intent),
      intentDigest,
      successorBinding: prepared.plan.successorBinding,
      rebindReceipt: prepared.plan.rebindReceipt,
    };
  }

  requireAuthorityReceipt(
    prepared: PreparedReviewAdoption | null,
    receipt: ExternalReviewReceipt | undefined,
  ): ExternalReviewReceipt | null {
    if (prepared === null) return null;
    if (receipt === undefined) throw new Error("true-chair review receipt is missing");
    return receipt;
  }

  validateAuthorityReceipt(
    prepared: PreparedReviewAdoption | null,
    receipt: ExternalReviewReceipt | null,
    authorityId: string,
  ): ValidatedReviewReceipt | null {
    if (prepared === null) return null;
    if (receipt === null) throw new Error("true-chair review receipt is missing");
    const receiptBody = {
      schemaVersion: 1,
      kind: "review-adoption-decision",
      authorityId: receipt.authorityId,
      authoritySequence: receipt.authoritySequence,
      previousReceiptDigest: receipt.previousReceiptDigest,
      intentDigest: prepared.intentDigest,
      subjectDigest: prepared.subjectDigest,
    };
    const valid = receipt.attestation.length > 0 &&
      Number.isSafeInteger(receipt.authoritySequence) && receipt.authoritySequence >= 1 &&
      receipt.authorityId === authorityId &&
      receipt.receiptDigest === lifecycleDigest("authenticated-receipt", receiptBody);
    if (!valid) throw new Error("externally verified review authorization is invalid");
    return {
      receipt,
      receiptBody,
      member: {
        ordinalDec: "2",
        intentDigest: prepared.intentDigest,
        authorityId: receipt.authorityId,
        authoritySequenceDec: String(receipt.authoritySequence),
        receiptDigest: receipt.receiptDigest,
        subjectDigest: prepared.subjectDigest,
      },
      completionOrdinal: {
        intentDigest: prepared.intentDigest,
        subjectDigest: prepared.subjectDigest,
        authorityReceiptDigest: receipt.receiptDigest,
      },
    };
  }

  persistAuthorityReceiptInCurrentTransaction(
    context: PreparedReviewAdoptionContext,
    validated: ValidatedReviewReceipt | null,
  ): void {
    this.#requireTransaction();
    if (context.review === null || validated === null) return;
    const receipt = validated.receipt;
    const receiptJson = canonicalJson({ ...validated.receiptBody,
      receiptDigest: receipt.receiptDigest, attestation: receipt.attestation });
    this.#database.prepare(`
      INSERT INTO lifecycle_authority_receipts(
        intent_digest,batch_id,ordinal,project_session_id,run_id,agent_id,kind,
        subject_owner_kind,subject_owner_id,subject_owner_revision,subject_digest,
        authority_id,authority_sequence,previous_authority_sequence,previous_receipt_digest,
        receipt_json,receipt_digest,attestation,verified_at
      ) VALUES (?, ?,2,?,?,?,'review-adoption-decision','custody',?,?,?,?,?,?,?,?,?,?,?)
    `).run(
      context.review.intentDigest, context.batchId, context.projectSessionId, context.runId,
      context.agentId, context.custodyId, context.finalRevision, context.review.subjectDigest,
      receipt.authorityId, receipt.authoritySequence,
      receipt.authoritySequence === 1 ? null : receipt.authoritySequence - 1,
      receipt.previousReceiptDigest, receiptJson, receipt.receiptDigest,
      receipt.attestation, receipt.verifiedAt,
    );
  }

  prepareReservationInCurrentTransaction(input: Readonly<{
    runId: string;
    agentId: string;
    custodyId: string;
    applyId: string;
    commandId: string;
    head: LifecycleCustodyHead;
    finalRevision: number;
    finalSourceRefDigest: string;
    lifecycleAdoptionEvidenceDigest: string;
    recordedAt: number;
    source: Readonly<Record<string, unknown>>;
    mutationPlan: LifecycleReviewMutationPlan;
  }>): ReviewReservationPlan {
    this.#requireTransaction();
    const highWaterRow = this.#database.prepare(`
      SELECT terminal_sequence FROM review_terminal_sequence_high_water WHERE run_id=?
    `).get(input.runId);
    const terminalSequenceHighWater = isRow(highWaterRow)
      ? integer(highWaterRow, "terminal_sequence") : 0;
    const target = this.#database.prepare(`
      SELECT * FROM review_completion_targets
       WHERE run_id=? AND state='current'
       ORDER BY target_generation DESC LIMIT 1
    `).get(input.runId);
    let targetGeneration: number | null = null;
    let predecessorBindingGeneration: number | null = null;
    let predecessorBindingDigest: string | null = null;
    let targetSnapshot: Readonly<Record<string, unknown>> | null = null;
    let bindingSnapshot: Readonly<Record<string, unknown>> | null = null;
    let reason: "no-current-target" | "target-subject-changed" | "binding-changed" | "target-head-changed" = "no-current-target";
    let sameSubject = false;
    let sourceMatches = false;
    let headMatches = false;
    if (isRow(target)) {
      targetSnapshot = target;
      targetGeneration = integer(target, "target_generation");
      const binding = this.#database.prepare(`
        SELECT binding.*,head.active_binding_generation,head.revision AS binding_head_revision
          FROM review_target_chair_bindings binding
          JOIN review_target_chair_binding_heads head
            ON head.run_id=binding.run_id AND head.target_generation=binding.target_generation
           AND head.active_binding_generation=binding.binding_generation
         WHERE binding.run_id=? AND binding.target_generation=?
      `).get(input.runId, targetGeneration);
      if (isRow(binding)) {
        bindingSnapshot = binding;
        predecessorBindingGeneration = integer(binding, "binding_generation");
        predecessorBindingDigest = text(binding, "binding_digest");
        const targetSubjectMatches = target.review_subject_digest === binding.review_subject_digest &&
          target.task_id === binding.task_id && target.reviewed_artifact_id === binding.reviewed_artifact_id &&
          target.delivery_review_basis_digest === binding.delivery_review_basis_digest &&
          target.repository_source_state_digest === binding.repository_source_state_digest &&
          target.bundle_digest === binding.bundle_digest;
        sourceMatches = binding.agent_id === input.agentId &&
          binding.adapter_id === input.source.source_adapter_id &&
          binding.adapter_contract_digest === input.source.source_adapter_contract_digest &&
          binding.principal_generation === input.source.source_principal_generation &&
          binding.provider_session_generation === input.source.source_provider_generation &&
          binding.bridge_generation === input.source.source_bridge_generation &&
          binding.chair_lease_generation === input.source.source_chair_lease_generation;
        headMatches = binding.active_binding_generation === binding.binding_generation;
        sameSubject = targetSubjectMatches;
        if (!targetSubjectMatches) reason = "target-subject-changed";
        else if (!sourceMatches) reason = "binding-changed";
        else if (!headMatches) reason = "target-head-changed";
        else reason = "binding-changed";
      } else {
        reason = "target-head-changed";
      }
    }
    const lifecycleCustodyRef = custodyRef(input.runId, input.agentId, input.custodyId, input.finalRevision);
    const cutBody = bindingSnapshot === null || targetGeneration === null || predecessorBindingGeneration === null ||
      predecessorBindingDigest === null
      ? null
      : {
          schemaVersion: 1,
          runId: input.runId,
          targetGeneration,
          predecessorBindingGeneration,
          predecessorBindingDigest,
          terminalSequenceHighWater,
          lifecycleCustodyRef,
          lifecycleAdoptionEvidenceDigest: input.lifecycleAdoptionEvidenceDigest,
        };
    const cutDigest = cutBody === null ? null : lifecycleDigest("review-certification-cut", cutBody);
    const rebound = targetSnapshot !== null && bindingSnapshot !== null && sameSubject && sourceMatches && headMatches &&
      targetGeneration !== null && predecessorBindingGeneration !== null && predecessorBindingDigest !== null &&
      cutBody !== null && cutDigest !== null;
    const successorBindingBody = rebound ? {
      run_id: input.runId,
      target_generation: targetGeneration,
      binding_generation: predecessorBindingGeneration! + 1,
      predecessor_binding_generation: predecessorBindingGeneration,
      predecessor_binding_digest: predecessorBindingDigest,
      predecessor_certification_cut_sequence: terminalSequenceHighWater,
      predecessor_certification_cut_digest: cutDigest,
      predecessor_certification_cut_custody_agent_id: input.agentId,
      predecessor_certification_cut_custody_id: input.custodyId,
      predecessor_certification_cut_custody_revision: input.finalRevision,
      agent_id: input.agentId,
      principal_generation: input.source.target_principal_generation,
      chair_lease_generation: input.source.source_chair_lease_generation,
      provider_session_generation: input.source.target_provider_generation,
      bridge_generation: input.source.target_bridge_generation,
      adapter_id: input.source.replacement_adapter_id,
      adapter_contract_digest: input.source.replacement_contract_digest,
      model_family: bindingSnapshot!.model_family,
      model: bindingSnapshot!.model,
      review_subject_digest: bindingSnapshot!.review_subject_digest,
      route_receipt_digest: bindingSnapshot!.route_receipt_digest,
      profile_digest: bindingSnapshot!.profile_digest,
      task_id: bindingSnapshot!.task_id,
      reviewed_artifact_id: bindingSnapshot!.reviewed_artifact_id,
      delivery_review_basis_digest: bindingSnapshot!.delivery_review_basis_digest,
      repository_source_state_digest: bindingSnapshot!.repository_source_state_digest,
      bundle_digest: bindingSnapshot!.bundle_digest,
      lifecycle_custody_id: input.custodyId,
      lifecycle_custody_revision: input.finalRevision,
      checkpoint_digest: input.source.checkpoint_digest,
      lifecycle_adoption_evidence_digest: input.lifecycleAdoptionEvidenceDigest,
    } : null;
    const successorBinding = successorBindingBody === null ? null : {
      ...successorBindingBody,
      binding_digest: lifecycleDigest("review-target-chair-binding", successorBindingBody),
    };
    const rebindReceiptBody = successorBinding === null || bindingSnapshot === null ? null : {
      run_id: input.runId,
      target_generation: targetGeneration,
      lifecycle_custody_agent_id: input.agentId,
      lifecycle_custody_id: input.custodyId,
      lifecycle_custody_revision: input.finalRevision,
      command_id: input.commandId,
      review_subject_digest: successorBinding.review_subject_digest,
      prior_binding_generation: predecessorBindingGeneration,
      new_binding_generation: successorBinding.binding_generation,
      prior_binding_digest: predecessorBindingDigest,
      new_binding_digest: successorBinding.binding_digest,
      lifecycle_adoption_digest: input.lifecycleAdoptionEvidenceDigest,
      bundle_digest: successorBinding.bundle_digest,
      profile_digest: successorBinding.profile_digest,
      slot_head_set_digest: lifecycleDigest("review-slot-head-set", successorBinding.route_receipt_digest),
      open_and_repair_finding_set_digest: lifecycleDigest("review-open-and-repair-finding-set", successorBinding.review_subject_digest),
    };
    const rebindReceipt = rebindReceiptBody === null ? null : {
      ...rebindReceiptBody,
      rebind_receipt_digest: lifecycleDigest("review-target-rebind-receipt", rebindReceiptBody),
    };
    const decision = {
      schemaVersion: 1,
      outcome: rebound ? "rebound" : "left-stale",
      reason: rebound ? null : reason,
      targetGeneration,
      predecessorBindingGeneration,
      predecessorBindingDigest,
      terminalSequenceHighWater,
      target: targetSnapshot,
      predecessorBinding: bindingSnapshot,
      certificationCut: cutBody === null ? null : { ...cutBody, cutDigest },
      successorBinding,
      lifecycleCustodyRef,
      lifecycleAdoptionEvidenceDigest: input.lifecycleAdoptionEvidenceDigest,
    };
    const decisionDigest = lifecycleDigest("review-adoption-decision", decision);
    const reservationId = `${input.applyId}:review`;
    const reservationBody = {
      schemaVersion: 1,
      reservationId,
      projectSessionId: input.head.projectSessionId,
      runId: input.runId,
      agentId: input.agentId,
      finalizedCustodyRef: lifecycleCustodyRef,
      adoptionEvidence: input.lifecycleAdoptionEvidenceDigest,
      terminalSequenceHighWater: String(terminalSequenceHighWater),
      target: targetSnapshot,
      predecessorBinding: bindingSnapshot,
      reviewDecision: decision,
      reviewDecisionDigest: decisionDigest,
      certificationCut: cutBody === null ? null : { ...cutBody, cutDigest },
      localMutationPlan: input.mutationPlan,
    };
    const reservationDigest = lifecycleDigest("review-adoption-reservation", reservationBody);
    this.#database.prepare(`
      INSERT INTO lifecycle_review_adoption_reservations(
        reservation_id,reservation_digest,project_session_id,run_id,agent_id,custody_id,
        finalized_custody_revision,target_generation,predecessor_binding_generation,
        predecessor_binding_digest,terminal_sequence_high_water,
        lifecycle_adoption_evidence_digest,review_decision_json,review_decision_digest,
        certification_cut_json,certification_cut_digest,certification_cut_key,
        recovery_source_kind,decision_loss_after_key,decision_loss_effect_key,
        local_write_set_digest,reservation_json,created_at
      ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,'none','none','none',?,?,?)
    `).run(
      reservationId, reservationDigest, input.head.projectSessionId, input.runId, input.agentId,
      input.custodyId, input.finalRevision, targetGeneration, predecessorBindingGeneration,
      predecessorBindingDigest, String(terminalSequenceHighWater), input.lifecycleAdoptionEvidenceDigest,
      canonicalJson(decision), decisionDigest,
      cutBody === null ? null : canonicalJson({ ...cutBody, cutDigest }), cutDigest,
      cutDigest ?? "none", input.mutationPlan.writeSetDigest, canonicalJson(reservationBody), input.recordedAt,
    );
    return { reservationId, reservationDigest, decision, decisionDigest,
      cut: cutBody === null ? null : { ...cutBody, cutDigest }, cutDigest,
      successorBinding, rebindReceipt };
  }

  writePostStateInCurrentTransaction(
    prepared: PreparedReviewAdoptionContext,
    reviewReceipt: VerifiedReviewReceipt,
    appliedAt: number,
  ): void {
    this.#requireTransaction();
    const review = prepared.review;
    if (review === null) return;
    const canApplyRebind = this.#assertPreparedReviewAuthentic(prepared, review, reviewReceipt);
    if (review.cut !== null) {
      const cut = row(review.cut, "prepared review certification cut");
      const ref = row(cut.lifecycleCustodyRef, "prepared review custody reference");
      this.#database.prepare(`
        INSERT INTO review_certification_cuts(
          run_id,target_generation,predecessor_binding_generation,predecessor_binding_digest,
          terminal_sequence_high_water,lifecycle_custody_agent_id,lifecycle_custody_id,
          lifecycle_custody_revision,lifecycle_adoption_evidence_digest,cut_digest,created_at
        ) VALUES (?,?,?,?,?,?,?,?,?,?,?)
      `).run(
        text(cut, "runId"), integer(cut, "targetGeneration"), integer(cut, "predecessorBindingGeneration"),
        text(cut, "predecessorBindingDigest"), integer(cut, "terminalSequenceHighWater"),
        text(ref, "agentId"), text(ref, "custodyId"), integer(ref, "custodyRevision"),
        text(cut, "lifecycleAdoptionEvidenceDigest"), text(cut, "cutDigest"), appliedAt,
      );
    }
    if (canApplyRebind && review.successorBinding !== null && review.rebindReceipt !== null) {
      const binding = row(review.successorBinding, "prepared successor review binding");
      this.#database.prepare(`
        INSERT INTO review_target_chair_bindings(
          run_id,target_generation,binding_generation,predecessor_binding_generation,
          predecessor_binding_digest,predecessor_certification_cut_sequence,
          predecessor_certification_cut_digest,predecessor_certification_cut_custody_agent_id,
          predecessor_certification_cut_custody_id,predecessor_certification_cut_custody_revision,
          agent_id,principal_generation,chair_lease_generation,provider_session_generation,
          bridge_generation,adapter_id,adapter_contract_digest,model_family,model,
          review_subject_digest,route_receipt_digest,profile_digest,task_id,reviewed_artifact_id,
          delivery_review_basis_digest,repository_source_state_digest,bundle_digest,
          lifecycle_custody_id,lifecycle_custody_revision,checkpoint_digest,
          lifecycle_adoption_evidence_digest,binding_digest,created_at
        ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
      `).run(
        binding.run_id, binding.target_generation, binding.binding_generation,
        binding.predecessor_binding_generation, binding.predecessor_binding_digest,
        binding.predecessor_certification_cut_sequence, binding.predecessor_certification_cut_digest,
        binding.predecessor_certification_cut_custody_agent_id,
        binding.predecessor_certification_cut_custody_id,
        binding.predecessor_certification_cut_custody_revision,
        binding.agent_id, binding.principal_generation, binding.chair_lease_generation,
        binding.provider_session_generation, binding.bridge_generation, binding.adapter_id,
        binding.adapter_contract_digest, binding.model_family, binding.model,
        binding.review_subject_digest, binding.route_receipt_digest, binding.profile_digest,
        binding.task_id, binding.reviewed_artifact_id, binding.delivery_review_basis_digest,
        binding.repository_source_state_digest, binding.bundle_digest,
        binding.lifecycle_custody_id, binding.lifecycle_custody_revision, binding.checkpoint_digest,
        binding.lifecycle_adoption_evidence_digest, binding.binding_digest, appliedAt,
      );
      const priorGeneration = integer(binding, "predecessor_binding_generation");
      const head = this.#database.prepare(`
        SELECT revision FROM review_target_chair_binding_heads
         WHERE run_id=? AND target_generation=? AND active_binding_generation=?
      `).get(binding.run_id, binding.target_generation, priorGeneration);
      if (!isRow(head)) throw new Error("review binding head changed before rebind");
      const headChanged = this.#database.prepare(`
        UPDATE review_target_chair_binding_heads
           SET active_binding_generation=?,revision=revision+1
         WHERE run_id=? AND target_generation=? AND active_binding_generation=? AND revision=?
      `).run(
        binding.binding_generation, binding.run_id, binding.target_generation, priorGeneration,
        integer(head, "revision"),
      );
      if (headChanged.changes !== 1) throw new Error("review binding head compare-and-set failed");
      const receipt = row(review.rebindReceipt, "prepared review rebind receipt");
      this.#database.prepare(`
        INSERT INTO review_target_rebind_receipts(
          run_id,target_generation,lifecycle_custody_agent_id,lifecycle_custody_id,
          lifecycle_custody_revision,command_id,review_subject_digest,
          prior_binding_generation,new_binding_generation,prior_binding_digest,
          new_binding_digest,lifecycle_adoption_digest,bundle_digest,profile_digest,
          slot_head_set_digest,open_and_repair_finding_set_digest,rebind_receipt_digest,created_at
        ) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
      `).run(
        receipt.run_id, receipt.target_generation, receipt.lifecycle_custody_agent_id,
        receipt.lifecycle_custody_id, receipt.lifecycle_custody_revision, receipt.command_id,
        receipt.review_subject_digest, receipt.prior_binding_generation,
        receipt.new_binding_generation, receipt.prior_binding_digest, receipt.new_binding_digest,
        receipt.lifecycle_adoption_digest, receipt.bundle_digest, receipt.profile_digest,
        receipt.slot_head_set_digest, receipt.open_and_repair_finding_set_digest,
        receipt.rebind_receipt_digest, appliedAt,
      );
    }
    const decision = row(review.decision, "prepared review decision");
    const cutDigest = review.cutDigest;
    this.#database.prepare(`
      INSERT INTO lifecycle_review_authority_bindings(
        receipt_digest,intent_digest,batch_id,ordinal,subject_digest,kind,subject_owner_kind,
        project_session_id,run_id,agent_id,custody_id,custody_revision,review_reservation_digest,
        review_decision_digest,certification_cut_digest,certification_cut_key,
        decision_loss_after_key,decision_loss_effect_key,decision_loss_effect_role,
        decision_loss_effect_digest,apply_id
      ) VALUES (?,?,?,?,?,'review-adoption-decision','custody',?,?,?,?,?,?,?,?,?,'none','none',NULL,NULL,?)
    `).run(
      reviewReceipt.receiptDigest, review.intentDigest, prepared.batchId, 2,
      review.subjectDigest, prepared.projectSessionId, prepared.runId, prepared.agentId,
      prepared.custodyId, prepared.finalRevision, review.reservationDigest, review.decisionDigest,
      cutDigest, cutDigest ?? "none", prepared.applyId,
    );
    void decision;
  }

  #assertPreparedReviewAuthentic(
    context: PreparedReviewAdoptionContext,
    review: PreparedReviewAdoption,
    reviewReceipt: VerifiedReviewReceipt,
  ): boolean {
    const invalid = (): never => {
      throw new Error("prepared review adoption is not authentic");
    };
    const reservationValue = this.#database.prepare(`
      SELECT * FROM lifecycle_review_adoption_reservations WHERE reservation_id=?
    `).get(review.reservationId);
    if (!isRow(reservationValue)) invalid();
    const reservation = row(reservationValue, "review adoption reservation row");
    const reservationJson = text(reservation, "reservation_json");
    const reservationBody = (() => {
      try {
        return row(JSON.parse(reservationJson), "review adoption reservation");
      } catch {
        return invalid();
      }
    })();
    const batchValue = this.#database.prepare(`
      SELECT batch.*,
             primary_intent.subject_json AS custody_terminal_subject_json,
             primary_intent.subject_digest AS custody_terminal_subject_digest
        FROM lifecycle_receipt_batches batch
        JOIN lifecycle_receipt_intents primary_intent
          ON primary_intent.batch_id=batch.batch_id AND primary_intent.ordinal=1
       WHERE batch.batch_id=? AND batch.planned_apply_id=?
         AND batch.project_session_id=? AND batch.run_id=? AND batch.agent_id=?
         AND batch.transition_kind='custody-terminal'
         AND batch.receipt_intent_count=2
         AND batch.secondary_intent_kind='review-adoption-decision'
    `).get(
      context.batchId, context.applyId, context.projectSessionId, context.runId, context.agentId,
    );
    if (!isRow(batchValue)) invalid();
    const batch = row(batchValue, "review adoption receipt batch");
    const custodyTerminalSubject = (() => {
      try {
        return row(JSON.parse(text(batch, "custody_terminal_subject_json")), "custody terminal subject");
      } catch {
        return invalid();
      }
    })();
    if (
      canonicalJson(custodyTerminalSubject) !== text(batch, "custody_terminal_subject_json") ||
      lifecycleDigest("receipt-subject", custodyTerminalSubject) !==
        text(batch, "custody_terminal_subject_digest")
    ) invalid();
    const ownerRef = row(custodyTerminalSubject.ownerRef, "custody terminal owner reference");
    const ownerCustodyRef = row(ownerRef.custodyRef, "custody terminal owner custody reference");
    if (
      ownerRef.kind !== "custody" || typeof ownerRef.sourceRefDigest !== "string" ||
      canonicalJson(ownerCustodyRef) !== canonicalJson(custodyRef(
        context.runId, context.agentId, context.custodyId, context.finalRevision,
      ))
    ) invalid();
    const expectedSubject = {
      schemaVersion: 1,
      kind: "review-adoption-decision",
      projectSessionId: context.projectSessionId,
      runId: context.runId,
      agentId: context.agentId,
      ownerRef,
      custodyTerminalSubjectDigest: text(batch, "custody_terminal_subject_digest"),
      lifecycleAdoptionEvidenceDigest: text(reservation, "lifecycle_adoption_evidence_digest"),
      reviewReservationDigest: review.reservationDigest,
      reviewDecision: review.decision,
      reviewDecisionDigest: review.decisionDigest,
      successorBinding: review.successorBinding,
      rebindReceipt: review.rebindReceipt,
      certificationCut: review.cut,
      certificationCutDigest: review.cutDigest,
      recoverySource: { kind: "none" },
      recoverySourceDecisionDigest: null,
      transitionReplayDigest: text(batch, "transition_replay_digest"),
    };
    if (
      text(reservation, "reservation_digest") !== review.reservationDigest ||
      review.reservationDigest !== lifecycleDigest("review-adoption-reservation", reservationBody) ||
      canonicalJson(reservationBody) !== reservationJson ||
      text(reservation, "project_session_id") !== context.projectSessionId ||
      text(reservation, "run_id") !== context.runId ||
      text(reservation, "agent_id") !== context.agentId ||
      text(reservation, "custody_id") !== context.custodyId ||
      integer(reservation, "finalized_custody_revision") !== context.finalRevision ||
      text(reservation, "review_decision_json") !== canonicalJson(review.decision) ||
      text(reservation, "review_decision_digest") !== review.decisionDigest ||
      review.decisionDigest !== lifecycleDigest("review-adoption-decision", review.decision) ||
      canonicalJson(reservationBody.reviewDecision) !== canonicalJson(review.decision) ||
      reservationBody.reviewDecisionDigest !== review.decisionDigest ||
      canonicalJson(review.subject) !== review.subjectJson ||
      canonicalJson(review.subject) !== canonicalJson(expectedSubject) ||
      review.subjectDigest !== lifecycleDigest("receipt-subject", review.subject) ||
      canonicalJson(review.intent) !== review.intentJson ||
      review.intentDigest !== lifecycleDigest("receipt-intent", review.intent) ||
      review.intent.subjectDigest !== review.subjectDigest ||
      review.intent.batchId !== context.batchId ||
      canonicalJson(review.subject.reviewDecision) !== canonicalJson(review.decision) ||
      review.subject.reviewDecisionDigest !== review.decisionDigest ||
      canonicalJson(review.subject.successorBinding) !== canonicalJson(review.successorBinding) ||
      canonicalJson(review.subject.rebindReceipt) !== canonicalJson(review.rebindReceipt)
    ) invalid();

    const storedCutJson = reservation.certification_cut_json;
    const expectedCutJson = review.cut === null ? null : canonicalJson(review.cut);
    if (
      storedCutJson !== expectedCutJson ||
      reservation.certification_cut_digest !== review.cutDigest ||
      canonicalJson(reservationBody.certificationCut) !== canonicalJson(review.cut)
    ) invalid();
    if (review.cut !== null && review.cutDigest !== lifecycleDigest(
      "review-certification-cut",
      Object.fromEntries(Object.entries(review.cut).filter(([key]) => key !== "cutDigest")),
    )) invalid();

    const decision = review.decision;
    if (
      canonicalJson(decision.successorBinding) !== canonicalJson(review.successorBinding) ||
      canonicalJson(decision.certificationCut) !== canonicalJson(review.cut)
    ) invalid();
    if (review.successorBinding !== null) {
      const { binding_digest: bindingDigest, ...bindingBody } = review.successorBinding;
      if (
        typeof bindingDigest !== "string" ||
        bindingDigest !== lifecycleDigest("review-target-chair-binding", bindingBody)
      ) invalid();
    }
    if (review.rebindReceipt !== null) {
      const { rebind_receipt_digest: receiptDigest, ...receiptBody } = review.rebindReceipt;
      if (
        typeof receiptDigest !== "string" ||
        receiptDigest !== lifecycleDigest("review-target-rebind-receipt", receiptBody)
      ) invalid();
    }
    const authorityReceipt = this.#database.prepare(`
      SELECT 1 FROM lifecycle_authority_receipts
       WHERE receipt_digest=? AND intent_digest=? AND batch_id=? AND ordinal=2
         AND kind='review-adoption-decision' AND project_session_id=?
         AND run_id=? AND agent_id=? AND subject_owner_kind='custody'
         AND subject_owner_id=? AND subject_owner_revision=? AND subject_digest=?
    `).get(
      reviewReceipt.receiptDigest, review.intentDigest, context.batchId,
      context.projectSessionId, context.runId, context.agentId, context.custodyId,
      context.finalRevision, review.subjectDigest,
    );
    if (!isRow(authorityReceipt)) invalid();
    if (decision.outcome === "rebound") {
      const targetGeneration = decision.targetGeneration;
      const predecessorBindingGeneration = decision.predecessorBindingGeneration;
      if (!Number.isSafeInteger(targetGeneration) || !Number.isSafeInteger(predecessorBindingGeneration)) {
        invalid();
      }
      const currentTarget = this.#database.prepare(`
        SELECT * FROM review_completion_targets
         WHERE run_id=? AND target_generation=? AND state='current'
      `).get(context.runId, targetGeneration);
      const currentBinding = this.#database.prepare(`
        SELECT binding.*,head.active_binding_generation,
               head.revision AS binding_head_revision
          FROM review_target_chair_bindings binding
          JOIN review_target_chair_binding_heads head
            ON head.run_id=binding.run_id AND head.target_generation=binding.target_generation
           AND head.active_binding_generation=binding.binding_generation
         WHERE binding.run_id=? AND binding.target_generation=?
           AND binding.binding_generation=?
      `).get(context.runId, targetGeneration, predecessorBindingGeneration);
      if (
        !isRow(currentTarget) || !isRow(currentBinding) ||
        canonicalJson(currentTarget) !== canonicalJson(decision.target) ||
        canonicalJson(currentBinding) !== canonicalJson(decision.predecessorBinding)
      ) {
        return false;
      }
    }
    return true;
  }


}
