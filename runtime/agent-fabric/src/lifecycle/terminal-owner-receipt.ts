import type Database from "better-sqlite3";

import { canonicalJson, integer, row, text } from "../persistence/row-codec.js";
import { lifecycleDigest } from "./custody-codec.js";
import {
  GenerationLossRepository,
  generationLossRef,
  generationLossRevisionBody,
  type GenerationLossHead,
} from "./generation-loss-repository.js";

const MUTATION_PLAN_RELATIONS = new Set([
  "agent-state", "provider-session", "provider-lineage", "provider-action", "principal-capability",
  "agent-bridge", "chair-bridge", "turn-lease", "write-lease", "delivery",
  "task-owner", "result-obligation", "membership", "barrier", "freeze-owner",
  "custody-revision", "custody-head", "generation-loss-revision", "generation-loss-head",
  "review-cut", "review-binding", "review-binding-pointer", "recovery-issue",
  "fresh-preparation", "fresh-handoff", "fresh-commit", "recovery-retirement", "audit",
]);

export type LifecycleMutationPlanWrite = Readonly<{
  relation: string;
  keyDigest: string;
  operation: "insert" | "update" | "delete";
  expectedSemanticDigest: string | null;
  afterSemanticJcs: string | null;
  afterSemanticDigest: string | null;
}>;

export type LifecycleMutationPlan = Readonly<{
  schemaVersion: 1;
  writes: readonly LifecycleMutationPlanWrite[];
  writeSetDigest: string;
}>;

export function mutationPlan(value: Readonly<Record<string, unknown>>): LifecycleMutationPlan {
  if (value.schemaVersion !== 1 || !Array.isArray(value.writes) || typeof value.writeSetDigest !== "string") {
    throw new Error("lifecycle mutation plan is not lifecycleMutationPlanV1");
  }
  const writes = value.writes.map((candidate, index) => {
    const write = row(candidate, `lifecycle mutation plan write ${index}`);
    const relation = text(write, "relation");
    if (!MUTATION_PLAN_RELATIONS.has(relation)) throw new Error("lifecycle mutation plan relation is invalid");
    const operation = text(write, "operation");
    if (!(["insert", "update", "delete"] as const).includes(operation as never)) {
      throw new Error("lifecycle mutation plan operation is invalid");
    }
    const keyDigest = text(write, "keyDigest");
    const expectedSemanticDigest = write.expectedSemanticDigest;
    const afterSemanticJcs = write.afterSemanticJcs;
    const afterSemanticDigest = write.afterSemanticDigest;
    if (operation === "insert" && expectedSemanticDigest !== null) {
      throw new Error("lifecycle mutation plan insert has an expected digest");
    }
    if (operation !== "insert" && typeof expectedSemanticDigest !== "string") {
      throw new Error("lifecycle mutation plan update/delete lacks an expected digest");
    }
    if (operation === "delete" && (afterSemanticJcs !== null || afterSemanticDigest !== null)) {
      throw new Error("lifecycle mutation plan delete has after state");
    }
    if (operation !== "delete" && (typeof afterSemanticJcs !== "string" || typeof afterSemanticDigest !== "string")) {
      throw new Error("lifecycle mutation plan write lacks after state");
    }
    return {
      relation,
      keyDigest,
      operation: operation as LifecycleMutationPlanWrite["operation"],
      expectedSemanticDigest: expectedSemanticDigest as string | null,
      afterSemanticJcs: afterSemanticJcs as string | null,
      afterSemanticDigest: afterSemanticDigest as string | null,
    };
  });
  const ordered = [...writes].sort((left, right) => canonicalJson(left).localeCompare(canonicalJson(right)));
  if (canonicalJson(writes) !== canonicalJson(ordered)) {
    throw new Error("lifecycle mutation plan writes are not strictly sorted");
  }
  const writeSetDigest = lifecycleDigest("mutation-plan", { schemaVersion: 1, writes });
  if (writeSetDigest !== value.writeSetDigest) throw new Error("lifecycle mutation plan write-set digest crossed");
  return { schemaVersion: 1, writes, writeSetDigest };
}

export type PreparedGenerationLossTerminal = Readonly<{
  projectSessionId: string;
  runId: string;
  agentId: string;
  generationLossId: string;
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
  fromState: "open";
  finalState: "abandoned";
  abandonKind: "direct-open";
  recoveryActionRef: null;
  terminalEvidenceDigest: string;
  mutationPlan: LifecycleMutationPlan;
  review: null;
}>;

export type PrepareGenerationLossTerminalInput = Readonly<{
  runId: string;
  agentId: string;
  generationLossId: string;
  expectedRevision: number;
  applyId: string;
  admissionDigest: string;
  operatorDecisionDigest: string;
  transitionProof: Readonly<Record<string, unknown>>;
  mutationPlan: Readonly<Record<string, unknown>>;
  terminalEvidenceDigest: string;
  recordedAt: number;
}>;

export type PersistSingleOwnerTerminalPreparationInput = Readonly<{
  transitionKind: "custody-terminal" | "generation-loss-terminal";
  subjectOwnerKind: "custody" | "generation-loss";
  ownerId: string;
  projectSessionId: string;
  runId: string;
  agentId: string;
  preRevision: number;
  finalRevision: number;
  applyId: string;
  beforeRef: Readonly<Record<string, unknown>>;
  afterRef: Readonly<Record<string, unknown>>;
  beforeJournalDigest: string;
  finalSemanticDigest: string;
  finalSourceRefDigest: string;
  admissionDigest: string;
  providerActionRef: Readonly<{ adapterId: string; actionId: string }> | null;
  terminalDisposition: string;
  terminalEvidenceDigest: string;
  transitionProof: Readonly<Record<string, unknown>>;
  mutationPlan: LifecycleMutationPlan;
  subjectFields: Readonly<Record<string, unknown>>;
  recordedAt: number;
  persistEffect: (batchId: string, effectDigest: string) => void;
}>;

export type PersistedSingleOwnerTerminalPreparation = Readonly<{
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
}>;

export function persistSingleOwnerTerminalPreparation(
  database: Database.Database,
  input: PersistSingleOwnerTerminalPreparationInput,
): PersistedSingleOwnerTerminalPreparation {
  const effectBody = {
    schemaVersion: 1,
    effectKind: "owner-transition",
    role: "primary",
    ownerBeforeRef: input.beforeRef,
    beforeJournalDigest: input.beforeJournalDigest,
    ownerAfterRef: input.afterRef,
    afterSemanticDigest: input.finalSemanticDigest,
  };
  const effectDigest = lifecycleDigest("lifecycle-effect", effectBody);
  const effectsSetDigest = lifecycleDigest("effect-set", [effectDigest]);
  const transitionProofDigest = lifecycleDigest("transition-proof", input.transitionProof);
  const replay = {
    schemaVersion: 1,
    transactionId: input.applyId,
    projectSessionId: input.projectSessionId,
    runId: input.runId,
    agentId: input.agentId,
    transitionKind: input.transitionKind,
    primaryOwnerBeforeRef: input.beforeRef,
    primaryOwnerAfterRef: input.afterRef,
    primaryOwnerBeforeJournalDigest: input.beforeJournalDigest,
    primaryOwnerAfterSemanticDigest: input.finalSemanticDigest,
    effectsSetDigest,
    admissionDigest: input.admissionDigest,
    providerActionRef: input.providerActionRef,
    recoverySource: { kind: "none" },
    terminalDisposition: input.terminalDisposition,
    terminalEvidenceDigest: input.terminalEvidenceDigest,
    transitionProof: input.transitionProof,
    transitionProofDigest,
    mutationPlan: input.mutationPlan,
    mutationPlanDigest: input.mutationPlan.writeSetDigest,
    reviewReservationDigest: null,
    freshHandoffDigest: null,
  };
  const transitionReplayDigest = lifecycleDigest("transition-replay", replay);
  const subject = {
    schemaVersion: 1,
    kind: input.transitionKind,
    projectSessionId: input.projectSessionId,
    runId: input.runId,
    agentId: input.agentId,
    ownerRef: input.afterRef,
    admissionDigest: input.admissionDigest,
    ...input.subjectFields,
    terminalEvidenceDigest: input.terminalEvidenceDigest,
    transitionReplayDigest,
  };
  const subjectDigest = lifecycleDigest("receipt-subject", subject);
  const ownerRefDigest = lifecycleDigest("receipt-owner-ref", input.afterRef);
  const orderedSubjectSetDigest = lifecycleDigest("receipt-subject-set", [{
    ordinalDec: "1",
    kind: input.transitionKind,
    ownerRefDigest,
    ownerRevisionDec: String(input.finalRevision),
    subjectDigest,
  }]);
  const batchBody = {
    schemaVersion: 1,
    projectSessionId: input.projectSessionId,
    runId: input.runId,
    agentId: input.agentId,
    plannedApplyId: input.applyId,
    transitionKind: input.transitionKind,
    primaryOwnerBeforeRef: input.beforeRef,
    primaryOwnerAfterRef: input.afterRef,
    primaryOwnerBeforeJournalDigest: input.beforeJournalDigest,
    primaryOwnerAfterSemanticDigest: input.finalSemanticDigest,
    effectsSetDigest,
    transitionReplayDigest,
    orderedSubjectSetDigest,
    receiptIntentCountDec: "1",
    secondaryIntentKind: "none",
    reviewReservationRef: null,
    freshHandoffRef: null,
  };
  const batchId = lifecycleDigest("receipt-batch-id", batchBody);
  const intent = {
    schemaVersion: 1,
    batchId,
    ordinalDec: "1",
    kind: input.transitionKind,
    subjectDigest,
    transitionReplayDigest,
  };
  const intentDigest = lifecycleDigest("receipt-intent", intent);
  const subjectJson = canonicalJson(subject);
  const intentJson = canonicalJson(intent);
  database.prepare(`
    INSERT INTO lifecycle_receipt_batches(
      batch_id,planned_apply_id,project_session_id,run_id,agent_id,transition_kind,
      planned_apply_kind,effects_set_digest,mutation_plan_digest,transition_replay_json,
      transition_replay_digest,ordered_subject_set_digest,receipt_intent_count,
      secondary_intent_kind,review_adoption_reservation_id,
      review_adoption_reservation_digest,review_decision_loss_effect_key,
      review_decision_loss_effect_role,review_decision_loss_effect_digest,
      review_decision_loss_after_id,review_decision_loss_after_revision,
      review_decision_loss_after_semantic_digest,review_decision_loss_after_source_ref_digest,
      fresh_handoff_id,fresh_handoff_digest,fresh_handoff_source_mode,fresh_handoff_key,
      recovery_retirement_id,recovery_retirement_plan_digest,created_at
    ) VALUES (?,?,?,?,?,?,'terminal',?,?,?,?,?,1,'none',NULL,NULL,'none',
              NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'none',NULL,NULL,?)
  `).run(
    batchId, input.applyId, input.projectSessionId, input.runId, input.agentId,
    input.transitionKind, effectsSetDigest, input.mutationPlan.writeSetDigest,
    canonicalJson(replay), transitionReplayDigest, orderedSubjectSetDigest,
    input.recordedAt,
  );
  input.persistEffect(batchId, effectDigest);
  if (input.subjectOwnerKind === "custody") {
    database.prepare(`
      INSERT INTO lifecycle_receipt_intents(
        batch_id,ordinal,batch_transition_kind,batch_intent_count,
        batch_secondary_intent_kind,kind,project_session_id,run_id,agent_id,
        subject_owner_kind,subject_owner_id,subject_owner_revision,
        custody_effect_digest,generation_loss_effect_role,generation_loss_effect_digest,
        recovery_retirement_effect_digest,fresh_origin_effect_digest,subject_json,
        subject_digest,intent_digest,created_at
      ) VALUES (?,1,'custody-terminal',1,'none','custody-terminal',?,?,?,
                'custody',?,?,?,NULL,NULL,NULL,NULL,?,?,?,?)
    `).run(
      batchId, input.projectSessionId, input.runId, input.agentId, input.ownerId,
      input.finalRevision, effectDigest, subjectJson, subjectDigest, intentDigest,
      input.recordedAt,
    );
  } else {
    database.prepare(`
      INSERT INTO lifecycle_receipt_intents(
        batch_id,ordinal,batch_transition_kind,batch_intent_count,
        batch_secondary_intent_kind,kind,project_session_id,run_id,agent_id,
        subject_owner_kind,subject_owner_id,subject_owner_revision,
        custody_effect_digest,generation_loss_effect_role,generation_loss_effect_digest,
        recovery_retirement_effect_digest,fresh_origin_effect_digest,subject_json,
        subject_digest,intent_digest,created_at
      ) VALUES (?,1,'generation-loss-terminal',1,'none','generation-loss-terminal',?,?,?,
                'generation-loss',?,?,NULL,'primary',?,NULL,NULL,?,?,?,?)
    `).run(
      batchId, input.projectSessionId, input.runId, input.agentId, input.ownerId,
      input.finalRevision, effectDigest, subjectJson, subjectDigest, intentDigest,
      input.recordedAt,
    );
  }
  return {
    batchId,
    effectDigest,
    ownerRefDigest,
    transitionReplayDigest,
    subject,
    subjectJson,
    subjectDigest,
    intent,
    intentJson,
    intentDigest,
  };
}

export class GenerationLossTerminalOwnerAdapter {
  readonly #database: Database.Database;

  constructor(database: Database.Database) {
    this.#database = database;
  }

  readExpectedHead(
    prepared: PreparedGenerationLossTerminal,
    expectedRevision: number,
  ): GenerationLossHead {
    const head = new GenerationLossRepository(this.#database).readHead(
      prepared.runId,
      prepared.agentId,
      prepared.generationLossId,
    );
    if (head.revision !== expectedRevision || head.revision !== prepared.preRevision ||
        head.state !== prepared.fromState || head.terminal) {
      throw new Error("generation-loss is not the expected terminal source head");
    }
    return head;
  }

  systemWrites(prepared: PreparedGenerationLossTerminal): readonly Readonly<{
    relation: string;
    key: string;
    operation: "insert" | "update";
  }>[] {
    return [{
      relation: "lifecycle_generation_loss_revisions",
      key: `${prepared.runId}:${prepared.agentId}:${prepared.generationLossId}:${prepared.finalRevision}`,
      operation: "insert",
    }, {
      relation: "lifecycle_generation_loss_heads",
      key: `${prepared.runId}:${prepared.agentId}:${prepared.generationLossId}`,
      operation: "update",
    }];
  }

  ownerAfterRef(prepared: PreparedGenerationLossTerminal): Readonly<Record<string, unknown>> {
    return {
      kind: "generation-loss",
      generationLossRef: generationLossRef(
        prepared.runId,
        prepared.agentId,
        prepared.generationLossId,
        prepared.finalRevision,
      ),
      sourceRefDigest: prepared.finalSourceRefDigest,
    };
  }

  finalizeAuthorized(
    prepared: PreparedGenerationLossTerminal,
    sourceHead: GenerationLossHead,
    input: Readonly<{
      authorityApplyDigest: string;
      recordedAt: number;
    }>,
  ): void {
    new GenerationLossRepository(this.#database).finalizeAuthorizedDirectOpenInCurrentTransaction({
      sourceHead,
      finalRevision: prepared.finalRevision,
      terminalEvidenceDigest: prepared.terminalEvidenceDigest,
      finalSemanticDigest: prepared.finalSemanticDigest,
      finalSourceRefDigest: prepared.finalSourceRefDigest,
      authorityBatchId: prepared.batchId,
      authorityApplyId: prepared.applyId,
      authorityApplyDigest: input.authorityApplyDigest,
      recordedAt: input.recordedAt,
    });
  }

  readFinalHead(prepared: PreparedGenerationLossTerminal): GenerationLossHead {
    return new GenerationLossRepository(this.#database).readHead(
      prepared.runId,
      prepared.agentId,
      prepared.generationLossId,
    );
  }

  readPrepared(
    runId: string,
    agentId: string,
    generationLossId: string,
    applyId: string,
  ): PreparedGenerationLossTerminal | null {
    const stored = this.#database.prepare(`
      SELECT batch.project_session_id,batch.planned_apply_id,batch.batch_id,
             batch.transition_replay_json,batch.transition_replay_digest,
             effect.pre_revision,effect.final_revision,effect.effect_digest,
             effect.final_semantic_digest,effect.final_source_ref_digest,
             intent.subject_json,intent.subject_digest,intent.intent_digest
        FROM lifecycle_receipt_generation_loss_effects effect
        JOIN lifecycle_receipt_batches batch ON batch.batch_id=effect.batch_id
        JOIN lifecycle_receipt_intents intent
          ON intent.batch_id=batch.batch_id AND intent.ordinal=1
       WHERE effect.run_id=? AND effect.agent_id=? AND effect.generation_loss_id=?
         AND effect.role='primary' AND batch.transition_kind='generation-loss-terminal'
         AND batch.planned_apply_id=?
    `).get(runId, agentId, generationLossId, applyId);
    if (stored === undefined) return null;
    const value = row(stored, "prepared generation-loss terminal");
    const parsedSubject: unknown = JSON.parse(text(value, "subject_json"));
    if (typeof parsedSubject !== "object" || parsedSubject === null || Array.isArray(parsedSubject)) {
      throw new Error("prepared lifecycle receipt subject is invalid");
    }
    const subject = parsedSubject as Readonly<Record<string, unknown>>;
    if (
      subject.kind !== "generation-loss-terminal" ||
      subject.fromState !== "open" ||
      subject.terminalState !== "abandoned" ||
      subject.abandonKind !== "direct-open" ||
      subject.recoveryActionRef !== null ||
      typeof subject.terminalEvidenceDigest !== "string"
    ) {
      throw new Error("prepared generation-loss terminal is invalid");
    }
    const ownerRef = row(subject.ownerRef, "prepared generation-loss owner reference");
    const lossRef = row(ownerRef.generationLossRef, "prepared generation-loss reference");
    if (
      ownerRef.kind !== "generation-loss" ||
      text(lossRef, "runId") !== runId ||
      text(lossRef, "agentId") !== agentId ||
      text(lossRef, "generationLossId") !== generationLossId ||
      integer(lossRef, "generationLossRevision") !== integer(value, "final_revision")
    ) {
      throw new Error("prepared generation-loss owner identity is crossed");
    }
    const batchId = text(value, "batch_id");
    const subjectDigest = text(value, "subject_digest");
    const transitionReplayDigest = text(value, "transition_replay_digest");
    const replay = row(JSON.parse(text(value, "transition_replay_json")), "prepared lifecycle transition replay");
    const parsedPlan = mutationPlan(row(replay.mutationPlan, "prepared lifecycle mutation plan"));
    const intent = {
      schemaVersion: 1,
      batchId,
      ordinalDec: "1",
      kind: "generation-loss-terminal",
      subjectDigest,
      transitionReplayDigest,
    };
    return {
      projectSessionId: text(value, "project_session_id"),
      runId,
      agentId,
      generationLossId,
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
      fromState: "open",
      finalState: "abandoned",
      abandonKind: "direct-open",
      recoveryActionRef: null,
      terminalEvidenceDigest: subject.terminalEvidenceDigest,
      mutationPlan: parsedPlan,
      review: null,
    };
  }

  prepare(input: PrepareGenerationLossTerminalInput): PreparedGenerationLossTerminal {
    if (!this.#database.inTransaction) throw new Error("lifecycle terminal preparation requires a transaction");
    const repository = new GenerationLossRepository(this.#database);
    const head = repository.readHead(input.runId, input.agentId, input.generationLossId);
    if (head.revision !== input.expectedRevision || head.state !== "open" || head.terminal) {
      throw new Error("generation-loss is not the expected terminal source head");
    }
    const source = row(this.#database.prepare(`
      SELECT loss_kind FROM lifecycle_generation_losses
       WHERE project_session_id=? AND run_id=? AND agent_id=? AND generation_loss_id=?
    `).get(
      head.projectSessionId,
      input.runId,
      input.agentId,
      input.generationLossId,
    ), "generation-loss terminal source");
    const lossKind = text(source, "loss_kind");
    if (lossKind !== "generation-advance" && lossKind !== "context-advance") {
      throw new Error("generation-loss terminal source kind is invalid");
    }
    const normalizedMutationPlan = mutationPlan(input.mutationPlan);
    const finalRevision = head.revision + 1;
    const finalBody = generationLossRevisionBody({
      generationLossId: input.generationLossId,
      revision: finalRevision,
      state: "abandoned",
      abandonKind: "direct-open",
      recoveryActionRef: null,
      activeRecoveryCustodyId: null,
      terminalEvidenceDigest: input.terminalEvidenceDigest,
    });
    const finalSemanticDigest = lifecycleDigest("generation-loss-semantic", finalBody);
    const finalSourceRefDigest = finalSemanticDigest;
    const beforeRef = {
      kind: "generation-loss",
      generationLossRef: generationLossRef(
        input.runId,
        input.agentId,
        input.generationLossId,
        head.revision,
      ),
      sourceRefDigest: head.sourceRefDigest,
    };
    const afterRef = {
      kind: "generation-loss",
      generationLossRef: generationLossRef(
        input.runId,
        input.agentId,
        input.generationLossId,
        finalRevision,
      ),
      sourceRefDigest: finalSourceRefDigest,
    };
    const prepared = persistSingleOwnerTerminalPreparation(this.#database, {
      transitionKind: "generation-loss-terminal",
      subjectOwnerKind: "generation-loss",
      ownerId: input.generationLossId,
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
      admissionDigest: input.admissionDigest,
      providerActionRef: null,
      terminalDisposition: "abandoned",
      terminalEvidenceDigest: input.terminalEvidenceDigest,
      transitionProof: input.transitionProof,
      mutationPlan: normalizedMutationPlan,
      subjectFields: {
        lossKind,
        fromState: "open",
        terminalState: "abandoned",
        abandonKind: "direct-open",
        recoveryCustodyRef: null,
        recoveryActionRef: null,
        operatorDecisionDigest: input.operatorDecisionDigest,
      },
      recordedAt: input.recordedAt,
      persistEffect: (batchId, effectDigest) => {
        this.#database.prepare(`
          INSERT INTO lifecycle_receipt_generation_loss_effects(
            batch_id,ordinal,role,planned_apply_id,batch_transition_kind,
            project_session_id,run_id,agent_id,generation_loss_id,pre_revision,
            pre_journal_digest,final_revision,final_semantic_digest,
            final_source_ref_digest,effect_digest
          ) VALUES (?,1,'primary',?,'generation-loss-terminal',?,?,?,?,?,?,?,?,?,?)
        `).run(
          batchId, input.applyId, head.projectSessionId, input.runId, input.agentId,
          input.generationLossId, head.revision, head.journalDigest, finalRevision,
          finalSemanticDigest, finalSourceRefDigest, effectDigest,
        );
      },
    });
    return {
      projectSessionId: head.projectSessionId,
      runId: input.runId,
      agentId: input.agentId,
      generationLossId: input.generationLossId,
      preRevision: head.revision,
      finalRevision,
      applyId: input.applyId,
      ...prepared,
      finalSemanticDigest,
      finalSourceRefDigest,
      fromState: "open",
      finalState: "abandoned",
      abandonKind: "direct-open",
      recoveryActionRef: null,
      terminalEvidenceDigest: input.terminalEvidenceDigest,
      mutationPlan: normalizedMutationPlan,
      review: null,
    };
  }
}
